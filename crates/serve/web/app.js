// The admin app (architecture 9): On Air, Library, Item editor, Playlists, Schedule, Insert rules, History,
// Notifications and System. Plain JavaScript over the library service's API; the stations are reached through
// its proxy (`/api/v1/stations/{id}/...`), so the browser only ever talks to one origin.
// the station last selected in this browser; without storage (a private window, blocked site data) the first station
const storeStation = id => { try { localStorage.setItem('station', id); } catch { } };
const S = { product: '', stations: [], station: (() => { try { return localStorage.getItem('station'); } catch { return null; } })() || null, live: null, cleanup: [] };
const routes = {};
const NAV = [['onair', 'On Air'], ['library', 'Library'], ['playlists', 'Playlists'], ['schedule', 'Schedule'], ['insert', 'Insert rules'], ['history', 'History'], ['notify', 'Notifications'], ['system', 'System']];
// A "?" beside a title: the Help page for that part of the app. `target` is an app page's route or a Help page's name; the
// Help section (help.js) turns `#/help/for/<target>` into the page's own address.
const helpLink = (target, anchor) => h('a', { class: 'help-q', href: `#/help/for/${target}${anchor ? '#' + anchor : ''}`, title: 'Help for this', 'aria-label': 'Help' }, '?');
const stApi = p => `/api/v1/stations/${S.station}/${p}`;

function onCleanup(f) { S.cleanup.push(f); }
function leave() { S.cleanup.splice(0).forEach(f => { try { f(); } catch { } }); }
async function safe(p, msg) { try { return await p; } catch (e) { toast((msg ? msg + ': ' : '') + e.message, true); return null; } }

// The sidebar is a drawer below 1024 px (a top bar with a menu button opens it); above that it is a fixed column.
const NARROW = matchMedia('(max-width: 1023px)');
function setDrawer(open, focus) {
  const nav = $('#nav'), btn = $('#menu'), main = $('#main');
  const on = open && NARROW.matches;
  document.body.classList.toggle('drawer-open', on);
  btn.setAttribute('aria-expanded', String(on));
  btn.setAttribute('aria-label', on ? 'Close menu' : 'Menu');
  $('#scrim').hidden = !on;
  // closed on a narrow screen: keep the links out of reach of the keyboard and of screen readers; open: the page behind is inert
  nav.inert = NARROW.matches && !on;
  main.inert = on;
  if (focus && on) nav.querySelector('a.on, a')?.focus();
  if (focus && !on) btn.focus();
}
function initDrawer() {
  $('#menu').addEventListener('click', () => setDrawer(!document.body.classList.contains('drawer-open'), true));
  $('#scrim').addEventListener('click', () => setDrawer(false, true));
  addEventListener('keydown', ev => { if (ev.key === 'Escape' && document.body.classList.contains('drawer-open')) setDrawer(false, true); });
  NARROW.addEventListener('change', () => setDrawer(false));
  $('#nav').addEventListener('click', ev => { if (ev.target.closest('a')) setDrawer(false); });
  setDrawer(false);
}
function buildNav(cur) {
  const nav = $('#nav'); nav.replaceChildren(h('div', { class: 'brand' }, S.product || ''));
  let label = '';
  for (const [id, text] of NAV) { if (cur === id) label = text; nav.append(h('a', { href: '#/' + id, class: cur === id ? 'on' : '', 'aria-current': cur === id ? 'page' : null }, text)); }
  $('#tb-app').textContent = S.product || '';
  $('#tb-page').textContent = label;
  setDrawer(false);
}
// A table wider than the screen scrolls inside its own box instead of widening the page. Tables that arrive later (the
// library, the history) are wrapped as they appear.
function wrapTables() {
  for (const t of $$('#main table')) {
    if (t.closest('.tscroll, .help-table')) continue;
    const w = h('div', { class: 'tscroll', style: `--tcols:${t.rows[0]?.cells.length || 3}` });
    t.replaceWith(w); w.append(t);
  }
}
new MutationObserver(wrapTables).observe($('#main'), { childList: true, subtree: true });
async function route() {
  leave();
  const parts = location.hash.replace(/^#\/?/, '').split('/');
  const name = parts[0] || 'onair';
  buildNav(name === 'item' ? 'library' : name);
  const main = $('#main'); main.replaceChildren(h('div', { class: 'dim' }, 'Loading'));
  const fn = routes[name] || routes.onair;
  try { const node = await fn(parts.slice(1)); main.replaceChildren(node); } catch (e) { main.replaceChildren(h('div', { class: 'notice bad' }, e.message)); }
}
addEventListener('hashchange', route);
initDrawer();

async function boot() {
  const hp = await J('/health').catch(() => ({}));
  S.product = hp.product || ''; document.title = S.product;
  const sl = await J('/api/v1/stations').catch(() => ({ stations: [] }));
  S.stations = sl.stations;
  if (!S.station || !S.stations.some(s => s.id === S.station)) S.station = S.stations[0]?.id || null;
  route();
}
function pickStation(onchange) {
  return h('div', { class: 'tabs' }, S.stations.map(s => h('button', { class: s.id === S.station ? 'on' : '', onclick: () => { S.station = s.id; storeStation(s.id); onchange(); } },
    s.display_name, ' ', h('span', { class: 'chip ' + (s.health?.http === 200 ? 'ok' : 'crit') }, s.health ? (s.health.status || 'down') : 'down'))));
}
const artImg = (url, cls = 'thumb') => url ? h('img', { class: cls, src: url }) : h('div', { class: cls });
const flagChips = fl => (fl || []).map(f => h('span', { class: 'chip' + (['ambient_suspect', 'override_outside_window', 'gain_pending'].includes(f) ? ' warn' : '') }, f));

// ================= On Air =================
// The console (docs/design/onair-2026-10-08/REVIEW.md, option B). One route, built from units that share the state kept
// in `st`, `prog` and `plist`:
//   head          the page title, the station tabs (with assist, held, safe), the Box monitor view
//   onairUnit     what plays, the clocks (elapsed, left, what happens next), the levels, Coming up, the Wait callout
//   transportStrip  the keys, built ONCE and changed in place (see below), and the phone's dock
//   programUnit   the status block or the rundown, the Change program sheet, the menu, the settings line
//   queueUnit     the library search, Reshuffle and the rows; alertsUnit; the Add sheet (the phone dock's Add key)
//   the layout    onairlayout.js: the processing display (A, B, C or off) and the Layout control's choices, written on the
//                 root as data-proc, data-queue, data-prog, data-split and data-phone (onair.css lays the page out from them)
// The rule that the old page's comment gave still holds: a box that has buttons in it is rebuilt only when what it shows has
// changed (a signature, `changed()`), never on every event, and the transport keys are never rebuilt at all: a held station
// keeps sending state and a key replaced between mousedown and mouseup loses the click. Times that merely move are written
// in place by `tick()`. Everything that outlives the page (the socket, the timers, the listeners, the dialog) is registered
// with `onCleanup`, so a page left open for days, or left and entered again, does not pile up.
routes.onair = async () => {
  const root = h('div', { class: 'oa' });
  if (!S.station) { root.append(h('h1', {}, 'On Air', helpLink('onair')), h('div', { class: 'notice' }, 'No station is configured on this box.')); return root; }
  const st = { state: null, now: null, frame: null, frameAt: 0 };
  let prog = null, plist = [], disp = null, meterCfg = null, qrows = null, openRow = null, trNames = [];
  const rowGuards = []; onCleanup(() => rowGuards.splice(0).forEach(g => g.cancel()));
  const sigs = new Map();
  const changed = (key, sig) => { const j = JSON.stringify(sig); if (sigs.get(key) === j) return false; sigs.set(key, j); return true; };
  const guards = [];
  onCleanup(() => guards.splice(0).forEach(g => { try { g.cancel(); } catch { } }));
  const act = async (path, body, msg) => { const r = await safe(jsend(stApi(path), body), msg); if (r) { st.state = r; st.now = r.now; paint(); } return r; };
  const pact = async (path, body, msg) => { const r = await safe(jsend(stApi(path), body), msg); if (r) refreshProg(); return r; };
  // Writes that do nothing when the value is already there: a station held at the end of an item keeps sending state, and an
  // attribute written again with the same value is still a DOM mutation (the check counts them: none may be left over).
  const setAttr = (el, k, v) => { if (el.getAttribute(k) !== v) el.setAttribute(k, v); };
  const setFlag = (el, k, on) => { if (el.hasAttribute(k) !== !!on) { if (on) el.setAttribute(k, ''); else el.removeAttribute(k); } };
  const setCls = (el, c, on) => { if (el.classList.contains(c) !== !!on) el.classList.toggle(c, !!on); };
  const cap = w => w ? w[0].toUpperCase() + w.slice(1) : '';
  const waitingOp = () => prog?.program?.waiting?.kind === 'operator';
  // "today 19:00:00", "yesterday 19:36:18" or "Wed 7 Oct 15:36:18": a time of an earlier day shows its date
  const when = iso => {
    if (!iso) return '';
    const d = new Date(iso), n = new Date(), day = x => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((day(n) - day(d)) / 86400000);
    if (diff === 0) return 'today ' + hhmm(iso);
    if (diff === 1) return 'yesterday ' + hhmm(iso);
    return d.toLocaleDateString('en-US', { weekday: 'short', day: 'numeric', month: 'short' }) + ' ' + hhmm(iso);
  };
  const T0_WORDS = { auto: 'automatically (fallback start)', deck: 'by hand from the Stream Deck', ui: 'by hand on this page', api: 'by hand through the API', nominal: 'at the expected start', block: 'by a Start block', schedule: 'by the schedule', switch: 'by a playlist switch', ended: 'when the program before it ended', abort: 'after an abort', start: 'when the station started' };

  // ---------------- the head: title, station tabs, Box monitor ----------------
  const tabBtns = new Map(), tabInfo = new Map();
  const tabs = h('div', { class: 'tabs' }, S.stations.map(s => {
    const chip = h('span', { class: 'chip fixed' }, s.health?.status || 'down');
    const b = h('button', { class: s.id === S.station ? 'on' : '', 'data-station': s.id, onclick: () => { S.station = s.id; storeStation(s.id); route(); } }, s.display_name, ' ', chip);
    tabBtns.set(s.id, { b, chip });
    tabInfo.set(s.id, { http: s.health?.http, status: s.health?.status, state: s.health?.state });
    return b;
  }));
  // what a tab says: the state that matters on air, most urgent first (safe, down, stopped, held, assist), else the health
  function paintTabs() {
    for (const [id, { chip }] of tabBtns) {
      const i = tabInfo.get(id) || {};
      let w, c;
      if (i.safe || i.state === 'safe') [w, c] = ['safe', 'crit'];
      else if (i.http !== 200) [w, c] = ['down', 'crit'];
      else if (i.state === 'stopped') [w, c] = ['stopped', 'warn'];
      else if (i.hold || i.state === 'held') [w, c] = ['held', 'warn'];
      else if (i.mode === 'assist') [w, c] = ['assist', 'warn'];
      else [w, c] = [i.status || 'ok', i.status === 'critical' ? 'crit' : i.status === 'warning' ? 'warn' : 'ok'];
      setText(chip, w); const cls = 'chip fixed ' + c; if (chip.className !== cls) chip.className = cls;
    }
  }
  // The other stations' tabs need their own state (assist and held are not in the list's health): one request each, from one
  // shared timer for the whole tab bar; the selected station's tab comes from the live socket.
  async function refreshTabs() {
    const l = await J('/api/v1/stations').catch(() => null); if (!l) return;
    for (const s of l.stations) {
      const i = { ...(tabInfo.get(s.id) || {}), http: s.health?.http, status: s.health?.status, state: s.health?.state };
      if (s.id !== S.station && s.health?.http === 200) {
        const x = await J(`/api/v1/stations/${s.id}/state`).catch(() => null);
        if (x) { i.mode = x.mode; i.hold = x.hold; i.safe = x.safe_mode; i.state = x.state; }
      } else if (s.id === S.station && st.state) { Object.assign(i, tabFromState(st.state)); }
      tabInfo.set(s.id, i);
    }
    paintTabs();
  }
  const tabFromState = s => ({ mode: s.mode, hold: s.hold, safe: s.safe_mode, state: s.state, http: 200, status: s.health });
  const tabTimer = setInterval(refreshTabs, 10000); onCleanup(() => clearInterval(tabTimer));

  // The display's view is one setting of the whole box (PUT /api/v1/display), not of the selected station.
  // single and processing show one station: the one of the tab selected here (split and rotate show them all)
  const ONE = { single: true, processing: true };
  const viewBtns = ['single', 'split', 'rotate', 'processing'].map(v => h('button', { type: 'button', 'data-view': v, 'aria-pressed': 'false', onclick: async () => { const r = await safe(jsend('/api/v1/display', ONE[v] && S.station ? { view: v, station: S.station } : { view: v }, 'PUT')); if (r) paintView(r); } }, v));
  const stationName = id => (S.stations || []).find(s => s.id === id)?.display_name || id;
  const paintView = d => {
    disp = d;
    for (const b of viewBtns) {
      const v = b.dataset.view, on = d.view === v; b.classList.toggle('on', on); b.setAttribute('aria-pressed', String(on));
      setText(b, on && ONE[v] && d.station ? `${v} · ${stationName(d.station)}` : v);
    }
  };
  J('/api/v1/display').then(paintView).catch(() => { });
  const monitor = h('span', { class: 'monitor' }, h('span', { class: 'seglab' }, 'Box monitor'), h('span', { class: 'segc', role: 'group', 'aria-label': 'Box monitor view' }, viewBtns), helpLink('display', 'display-view'));
  // the layout of this page, per browser (onairlayout.js); applied by applyLayout() once the units exist
  let layout = OnAirLayout.load();
  const layoutCtl = OnAirLayout.control(layout, l => applyLayout(l), onCleanup);
  root.append(h('div', { class: 'oa-head' }, h('h1', {}, 'On Air', helpLink('onair')), tabs, h('span', { class: 'spacer' }), layoutCtl, monitor));

  // ---------------- the on-air unit ----------------
  const lamp = (name, color) => { const e = h('span', { class: 'rlight', style: `--lc:var(--${color})` }, h('i'), h('span', { class: 'rlt' }, name)); return e; };
  const lamps = { playing: lamp('PLAYING', 'green'), hold: lamp('HOLD', 'amber'), assist: lamp('ASSIST', 'amber'), safe: lamp('SAFE', 'red') };
  const uTitle = h('span', { class: 't' }), uPill = h('span', { class: 'pillstate' }), uOp = h('span', { class: 'chip crit', hidden: true }), uOff = h('span', { class: 'chip crit offline' }, 'reconnecting');
  const art = h('img', { class: 'art', alt: '', hidden: true });
  const tTitle = h('div', { class: 'tt' }), tArtist = h('div', { class: 'aa' });
  const cEl = h('div', { class: 'v', id: 'el' }, '0:00'), cRm = h('div', { class: 'v', id: 'rm' }, '0:00'), cSg = h('div', { class: 'v', id: 'sg' }), cSgL = h('div', { class: 'l', id: 'sgl' });
  const pg = h('i', { id: 'pg' }), pmark = h('b', { hidden: true });
  const ledL = h('div', { class: 'led', 'aria-hidden': 'true' }, h('i', { class: 'lit' }), h('i', { class: 'pk' })), ledR = h('div', { class: 'led', 'aria-hidden': 'true' }, h('i', { class: 'lit' }), h('i', { class: 'pk' }));
  const nums = ['M', 'S', 'W', 'I'].map(k => h('span', {}, k + ' ', h('span', { class: 'lv' }, '--'.padStart(6)))), numNote = h('span', { class: 'unitlab' }, 'LUFS');
  const comingBox = h('div', { class: 'nextbox' }), lufsEl = h('div', { class: 'lufs mono' }, ...nums, numNote);
  root.append(h('section', { class: 'unit', id: 'onairunit' },
    h('div', { class: 'uhead' }, uTitle, uPill, uOp, uOff, h('span', { class: 'r lampbar', role: 'group', 'aria-label': 'Station state lamps' }, lamps.playing, lamps.hold, lamps.assist, lamps.safe)),
    h('div', { class: 'onair' }, art,
      h('div', { class: 'main' }, h('div', { class: 'ttl' }, tTitle, tArtist),
        h('div', { class: 'clockrow' },
          h('div', { class: 'clocks' }, h('div', { class: 'clock' }, cEl, h('div', { class: 'l' }, 'elapsed')), h('div', { class: 'clock left' }, cRm, h('div', { class: 'l' }, 'left')), h('div', { class: 'clock nxt' }, cSg, cSgL)),
          h('div', { class: 'oa-pbar' }, pg, pmark), h('div', { class: 'leds' }, ledL, ledR), lufsEl)),
      comingBox)));
  const setLamp = (l, on) => setCls(l, 'on', on);
  const clocksEl = cEl.parentElement.parentElement;
  // everything in the unit is written in place (setText and attributes): there is nothing to lose a click
  function paintUnit(s) {
    const n = st.now;
    setText(uTitle, `${s.display_name || s.station} on air`);
    const pc = 'pillstate ' + (s.safe_mode ? 'safe' : s.state === 'playing' && s.mode === 'assist' ? 'assist' : s.state); if (uPill.className !== pc) uPill.className = pc;
    setText(uPill, (s.safe_mode ? 'SAFE MODE' : s.state === 'playing' && s.mode === 'assist' ? 'assist' : s.state).toUpperCase());
    const op = s.operating_mode && s.operating_mode !== 'normal' ? s.operating_mode : null; setFlag(uOp, 'hidden', !op); if (op) setText(uOp, op);
    setLamp(lamps.playing, s.state === 'playing' && !s.safe_mode); setLamp(lamps.hold, !!s.hold || s.state === 'held'); setLamp(lamps.assist, s.mode === 'assist'); setLamp(lamps.safe, !!s.safe_mode);
    const url = n?.art_url || ''; if ((art.getAttribute('src') || '') !== url) { if (url) art.setAttribute('src', url); else art.removeAttribute('src'); } setFlag(art, 'hidden', !url);
    setText(tTitle, n ? n.title : 'Nothing on air'); setText(tArtist, n ? [n.artist, n.album].filter(Boolean).join(' - ') : '');
    if (n && n.segue_at && n.ends_at) {
      const a = Date.parse(n.started_at), z = Date.parse(n.ends_at), g = Date.parse(n.segue_at), p = (100 * (g - a) / (z - a)).toFixed(1) + '%';
      setFlag(pmark, 'hidden', !(g < z)); if (pmark.style.left !== p) pmark.style.left = p;
    } else setFlag(pmark, 'hidden', true);
    paintNexts();
  }

  // ---- "Coming up": the next entries of the queue (blocks as badges) and the Wait callout ----
  function paintNexts() {
    const s = st.state; if (!s) return;
    const q = (s.queue || []).slice(0, 3), w = prog?.program?.waiting;
    if (!changed('coming', [q.map(e => e.kind === 'block' ? ['b', e.block_id, e.name, e.badge, e.trigger] : [e.entry_id, e.title, e.artist, e.art_url]), w && [w.block_id, w.kind, w.name], s.state, s.mode])) return;
    const kids = [h('span', { class: 'glab' }, 'Coming up')];
    let afterWait = false;
    for (const e of q) {
      if (e.kind === 'block') {
        const badge = e.badge || BADGE[e.type] || String(e.type || 'BLK').toUpperCase();
        const isWait = w && w.block_id === e.block_id && w.kind === 'operator';
        afterWait = isWait;
        kids.push(h('div', { class: 'nx' }, h('span', { class: 'bdg bdgbox', 'data-b': badge }, badge), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, e.name || e.type || ''),
          h('div', { class: 'aa' }, isWait ? 'holds for the operator' : e.trigger === 'timed' ? 'at a time' : 'in sequence'),
          !isWait && e.planned_start ? h('div', { class: 'when', 'data-nt': 'b' + e.block_id }, hhmm(e.planned_start)) : null)));
      } else {
        kids.push(h('div', { class: 'nx' }, e.art_url ? h('img', { src: e.art_url, alt: '' }) : h('div', { class: 'bdgbox' }), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, e.title), h('div', { class: 'aa' }, e.artist || ''),
          h('div', { class: 'when', 'data-nt': 't' + e.entry_id }, afterWait ? 'after Continue' : hhmm(e.planned_start)))));
        afterWait = false;
      }
    }
    if (!q.length) kids.push(h('div', { class: 'aa dim' }, 'The queue is empty.'));
    if (w) kids.push(w.kind === 'operator'
      ? h('div', { class: 'callout wait', role: 'status' }, h('b', {}, 'Waiting for you'), `: ${w.name}. Press Continue.`)
      : h('div', { class: 'callout', role: 'status' }, `Waiting: ${w.name} (${w.kind === 'time' ? 'until ' + hhmm(w.until) : 'for a length of time'})`));
    comingBox.replaceChildren(...kids);
  }

  // ---------------- the transport strip: built once ----------------
  // A key is [lamp] label + a small line. Continue is lit only while a Wait holds; Hold and Assist latch and show a lamp.
  // Cut is under a cover (press, then press again within 5 s); Stop fires only after a 1 s hold and becomes Play when the
  // station is stopped. The page is the only place with these guards: the API and the Stream Deck are not changed.
  const key = (cls, label, small, o = {}) => { const k = h('button', { type: 'button', class: 'key ' + cls, 'aria-label': o.aria || null }, o.lamp ? h('span', { class: 'lamp', 'aria-hidden': 'true' }) : null, h('span', { class: 'kl' }, label), h('small', {}, small)); k.kl = k.querySelector('.kl'); k.sm = k.querySelector('small'); return k; };
  const K = {
    cont: key('k-cont hero f-continue', 'Continue', 'no Wait', { lamp: true, aria: 'Continue: release the Wait that holds for the operator' }),
    next: key('k-next hero go f-next', 'Next', '1 s fade', { aria: 'Next: start the next item, 1 second fade' }),
    fade: key('k-fade f-fade', 'Fade', '2 s fade', { aria: 'Fade to next: 2 second fade' }),
    hold: key('k-hold f-hold', 'Hold', 'after this', { lamp: true, aria: 'Hold after this item' }),
    asst: key('k-asst f-assist', 'Assist', 'off', { lamp: true, aria: 'Assist mode' }),
    cut: key('k-cut danger guard f-cut', 'Cut', 'lift cover, press', { aria: 'Cut: press once to lift the cover, then press again within 5 seconds' }),
    stop: key('k-stop danger f-stop', 'Stop', 'hold 1 s', { aria: 'Stop: hold for 1 second' }),
  };
  K.cont.disabled = true; K.cont.setAttribute('aria-disabled', 'true');
  K.cont.addEventListener('click', () => pact('program/continue', {}, 'Continue'));
  K.next.addEventListener('click', () => act('transport/next', {}, 'Next'));
  K.fade.addEventListener('click', () => act('transport/fade-next', {}, 'Fade'));
  K.hold.addEventListener('click', () => act(st.state?.hold ? 'transport/resume' : 'transport/hold', {}, 'Hold'));
  K.asst.addEventListener('click', () => act('mode', { mode: st.state?.mode === 'assist' ? 'auto' : 'assist' }, 'Mode'));
  guards.push(coverKey(K.cut, 5000, () => act('transport/cut', {}, 'Cut'), { label: K.cut.sm, text: n => `press again, ${n} s` }));
  const stopped = () => st.state?.state === 'stopped';
  guards.push(holdToFire(K.stop, 1000, () => act('transport/stop', { fade_ms: 1500 }, 'Stop'), { when: () => !stopped() }));
  K.stop.addEventListener('click', () => { if (stopped()) act('transport/play', {}, 'Play'); });
  const strip = h('div', { class: 'tstrip', role: 'group', 'aria-label': 'Transport' },
    h('div', { class: 'keygroup' }, h('span', { class: 'glab' }, 'Program'), h('div', { class: 'keys' }, K.cont)), h('span', { class: 'vsep' }),
    h('div', { class: 'keygroup' }, h('span', { class: 'glab' }, 'Advance'), h('div', { class: 'keys' }, K.next, K.fade)), h('span', { class: 'vsep' }),
    h('div', { class: 'keygroup' }, h('span', { class: 'glab' }, 'Mode'), h('div', { class: 'keys' }, K.hold, K.asst)), h('span', { class: 'push' }),
    h('div', { class: 'keygroup hard' }, h('span', { class: 'glab' }, 'Hard actions'), h('div', { class: 'keys' }, K.cut, K.stop)));
  root.append(h('section', { class: 'unit', id: 'transport' }, strip));
  // the phone's dock: the three keys used all the time, and Add (the library search in a sheet), shown while the strip has
  // scrolled out of sight
  const D = { cont: key('f-dock-continue', 'Continue', '', { lamp: true, aria: 'Continue' }), next: key('go f-dock-next', 'Next', '', { aria: 'Next' }), fade: key('f-dock-fade', 'Fade', '', { aria: 'Fade to next' }),
    add: key('f-dock-add', 'Add', '', { aria: 'Add to the queue: search the library' }) };
  for (const k of Object.values(D)) k.querySelector('small').remove();
  D.cont.disabled = true;
  D.cont.addEventListener('click', () => K.cont.click()); D.next.addEventListener('click', () => K.next.click()); D.fade.addEventListener('click', () => K.fade.click());
  D.add.setAttribute('aria-haspopup', 'dialog'); D.add.addEventListener('click', () => openAdd());
  const dock = h('div', { class: 'dock', role: 'group', 'aria-label': 'Transport dock', hidden: true }, D.cont, D.next, D.fade, D.add);
  root.append(dock);
  if ('IntersectionObserver' in window) {
    const io = new IntersectionObserver(es => { const vis = es[es.length - 1].isIntersecting; dock.hidden = vis; root.classList.toggle('docked', !vis); }, { threshold: 0.01 });
    io.observe(strip); onCleanup(() => io.disconnect());
  } else dock.hidden = false;
  function paintKeys(s) {
    const en = waitingOp();
    for (const c of [K.cont, D.cont]) { setFlag(c, 'disabled', !en); setAttr(c, 'aria-disabled', String(!en)); setCls(c, 'lit-amber', en); }
    setText(K.cont.sm, en ? 'release the Wait' : 'no Wait');
    setCls(K.hold, 'lit-amber', !!s.hold); setAttr(K.hold, 'aria-pressed', String(!!s.hold)); setText(K.hold.sm, s.hold ? 'on' : 'after this');
    const as = s.mode === 'assist'; setCls(K.asst, 'lit-amber', as); setAttr(K.asst, 'aria-pressed', String(as)); setText(K.asst.sm, as ? 'on' : 'off');
    const stp = s.state === 'stopped';
    setCls(K.stop, 'danger', !stp); setCls(K.stop, 'go', stp);
    setText(K.stop.kl, stp ? 'Play' : 'Stop'); setText(K.stop.sm, stp ? 'start the queue' : 'hold 1 s');
    const al = stp ? 'Play: start the first item of the queue' : 'Stop: hold for 1 second'; setAttr(K.stop, 'aria-label', al);
  }

  // ---------------- the program unit ----------------
  const progView = h('div', { class: 'progview' });
  let rdOpen = false;   // a compact program unit's rundown, opened on demand (not kept)
  const pTitle = h('span', { class: 't' }, 'Program');
  const changeBtn = h('button', { type: 'button', class: 'f-change', onclick: () => openChange() }, 'Change', h('span', { class: 'wide' }, ' program'), '…');
  const moreBtn = h('button', { type: 'button', class: 'f-more', title: 'More: arm, open the playlist, end the program', 'aria-haspopup': 'menu', 'aria-expanded': 'false', 'aria-label': 'Program menu', onclick: ev => { ev.stopPropagation(); toggleMenu(); } }, '⋯');
  const miArm = h('button', { type: 'button', class: 'f-arm', role: 'menuitem', hidden: true }), miOpen = h('a', { class: 'mi', role: 'menuitem', hidden: true }, 'Open the playlist', h('small', {}, 'in the Playlists editor'));
  const miSep = h('hr'), miEndMusic = h('button', { type: 'button', class: 'dz f-abort f-abort-music', role: 'menuitem', 'aria-label': 'End program, music continues: hold for 1 second' }, h('span', {}, 'End program, music continues'), h('small', {}, 'no more blocks run; hold 1 s'));
  const miEndStop = h('button', { type: 'button', class: 'dz f-abort f-abort-stop', role: 'menuitem', 'aria-label': 'End program and stop the audio: hold for 1 second' }, h('span', {}, 'End program and stop the audio'), h('small', {}, 'silence; hold 1 s'));
  const pmenu = h('div', { class: 'menu pm', role: 'menu', hidden: true, style: 'top:38px;right:8px' }, miArm, miOpen, miSep, miEndMusic, miEndStop);
  const endAbort = stopAudio => () => { closeMenu(); pact('program/abort', { stop_audio: stopAudio }, 'End program'); };
  guards.push(holdToFire(miEndMusic, 1000, endAbort(false)), holdToFire(miEndStop, 1000, endAbort(true)));
  let menuOpen = false;
  const outside = ev => { if (!pmenu.contains(ev.target) && ev.target !== moreBtn) closeMenu(); };
  const menuKey = ev => { if (ev.key === 'Escape') { closeMenu(); moreBtn.focus(); } };
  function closeMenu() { if (!menuOpen) return; menuOpen = false; pmenu.hidden = true; moreBtn.setAttribute('aria-expanded', 'false'); document.removeEventListener('click', outside); document.removeEventListener('keydown', menuKey); }
  function toggleMenu() { if (menuOpen) { closeMenu(); return; } menuOpen = true; paintMenu(); pmenu.hidden = false; moreBtn.setAttribute('aria-expanded', 'true'); setTimeout(() => { if (menuOpen) { document.addEventListener('click', outside); document.addEventListener('keydown', menuKey); } }, 0); }
  onCleanup(closeMenu);
  const progHelp = helpLink('onair', 'program');
  // the crossfade style (a popover with the picker), the no-repeat rule, the meters and the link to the station's settings
  const slXfade = h('button', { type: 'button', class: 'lnk f-xfade', 'aria-expanded': 'false', title: 'Change the crossfade style' }, 'Crossfade ', h('b', {}, '…'));
  const slRep = h('span', {}), slMeters = h('a', { href: '#/settings/__global#meters', title: 'Meter ballistics are in Settings, This box' }, 'Meters ', h('b', {}, '…'));
  const xpop = h('div', { class: 'xpop', hidden: true });
  let xfadeFor = null;
  slXfade.addEventListener('click', async () => {
    if (xfadeFor !== S.station) {
      xfadeFor = S.station; const picker = await crossfadePicker(S.station, { onchange: v => { setText(slXfade.querySelector('b'), v); } });
      xpop.replaceChildren(picker, h('div', { style: 'margin-top:6px;font-size:12px' }, h('a', { href: `#/settings/${S.station}#transition_preset` }, 'This setting in Settings ›')));
    }
    xpop.hidden = !xpop.hidden; slXfade.setAttribute('aria-expanded', String(!xpop.hidden));
  });
  const settingsLine = h('div', { class: 'settingsline' }, slXfade, slRep, slMeters, h('a', { href: `#/settings/${S.station}`, class: 'f-stationsettings' }, 'Station settings ›'));
  function paintSettingsLine(s) {
    const x = s.transition_preset; if (x) setText(slXfade.querySelector('b'), x);
    const r = s.active_playlist?.rotation;
    const rep = r ? `${Math.round(r.no_repeat_configured)} min` : '';
    if (changed('rep', r && [r.no_repeat_configured, r.no_repeat_effective, r.capped, r.no_repeat_reason])) slRep.replaceChildren(...(r ? ['No repeat ', h('b', {}, rep), r.capped ? h('span', { class: 'chip warn', title: r.no_repeat_reason || r.message || '', style: 'margin-left:6px' }, `only ${Math.round(r.no_repeat_effective)} min`) : null] : []).filter(Boolean));
    setFlag(slRep, 'hidden', !r);
    const lbl = meterCfg?.presets?.find(p => p.preset === meterCfg.preset)?.label; if (lbl) setText(slMeters.querySelector('b'), lbl);
  }
  function paintMenu() {
    const d = prog, pr = d?.program, ex = d?.expected;
    const arm = ex && (ex.fallback_grace_s != null || ex.armed);
    miArm.hidden = !arm;
    if (arm) { miArm.replaceChildren(h('span', {}, ex.armed ? 'Disarm the fallback start' : 'Arm the fallback start'), h('small', {}, 'for the next expected start only')); miArm.onclick = () => { closeMenu(); pact('program/' + (ex.armed ? 'disarm' : 'arm'), { playlist_id: ex.playlist_id }, 'Arm'); }; }
    const pid = pr?.playlist_id || st.state?.active_playlist?.id; miOpen.hidden = !pid; if (pid) miOpen.href = '#/playlists/' + pid;
    for (const e of [miEndMusic, miEndStop]) e.hidden = !pr; miSep.hidden = !pr || (miArm.hidden && miOpen.hidden);
  }
  async function refreshProg() { const r = await safe(J(stApi('program/state')), 'Program'); if (r) { prog = r; sigs.delete('prog'); sigs.delete('coming'); paintProg(); paintKeys(st.state || {}); paintNexts(); } }
  const STAT_CLASS = { fired: 'ok', soft: 'ok', hard: 'ok', done: 'ok', late_run: 'warn', current: 'warn', failed: 'crit', missed: 'crit' };
  const BT_TEXT = { planning: 'planning', on_target: 'on target', within_tolerance: 'within tolerance', fallback: 'fallback: the fade closes the play' };
  const pastTable = list => h('table', { class: 'pasttable' }, h('thead', {}, h('tr', {}, ['Block', 'Type', 'Was due', 'Late by', 'Late policy', 'What happens'].map(x => h('th', {}, x)))),
    h('tbody', {}, list.map(r => h('tr', {}, h('td', {}, r.name || ''), h('td', {}, BADGE[r.type] || r.type || ''), h('td', { class: 'mono' }, r.at || r.target ? hhmm(r.at || r.target) : ''), h('td', { class: 'mono' }, r.late_by_s != null ? r.late_by_s + ' s' : ''), h('td', {}, r.policy || ''), h('td', {}, r.action || r.how || '')))));
  function paintProg() {
    const d = prog; closeMenuIfGone(d);
    if (!d) { progView.replaceChildren(h('div', { class: 'dim' }, 'The station does not report a program.')); return; }
    const pr = d.program;
    const sig = [pr && [pr.run_id, pr.state, pr.cursor, pr.waiting?.block_id, pr.stack.map(x => x.playlist), pr.t0], (d.blocks || []).map(b => [b.block_id, b.status, b.at, b.fired_at, b.current]), d.next_block?.block_id, d.back_time?.state, d.back_time?.label, d.expected && [d.expected.at, d.expected.armed, d.expected.playlist_id, d.expected.fallback_grace_s], plist.length, st.state?.active_playlist?.id, layout.prog, rdOpen];
    paintMenu();
    if (!changed('prog', sig)) return;
    rowGuards.splice(0).forEach(g => g.cancel());
    const kids = [];
    const ex = d.expected;
    if (ex) kids.push(h('div', { class: 'row expline pdim', style: 'margin-bottom:8px' }, h('span', {}, h('b', {}, ex.playlist || 'A program'), ', ' + (ex.label || 'expected ' + hhmm(ex.at).slice(0, 5))), h('span', { class: 'btm mono expcount', 'data-due': Date.now() + ex.in_ms }, mmss(ex.in_ms)), ex.armed ? h('span', { class: 'chip ok' }, 'armed') : null));
    if (!pr) {
      const ap = st.state?.active_playlist;
      kids.push(h('div', { class: 'dim noprog' }, 'No program is running.' + (ap ? ` The station plays ${ap.name}.` : '')));
    } else {
      const blocks = d.blocks || [], hasBlocks = blocks.length > 0;
      const here = blocks.filter(b => b.depth === pr.cursor.depth), curIdx = here.findIndex(b => b.current);
      const pillCls = 'pillstate ' + (pr.state === 'running' ? '' : pr.state === 'waiting' ? 'assist' : 'safe');
      kids.push(h('div', { class: 'pst progline' }, h('span', { class: 'pn progname' }, pr.playlist), h('span', { class: pillCls }, pr.state.toUpperCase())));
      const how = T0_WORDS[pr.t0_source] || pr.t0_source;
      if (hasBlocks) {
        kids.push(h('div', { class: 'pfacts progcursor' }, 'Started ', h('b', {}, when(pr.t0)), ` ${how} · row `, h('b', {}, pr.cursor.position), curIdx >= 0 ? [', block ', h('b', {}, `${curIdx + 1} of ${here.length}`)] : null, pr.cursor.fill != null ? `, fill ${pr.cursor.fill}` : ''));
      } else {
        const pl = plist.find(p => p.id === pr.playlist_id);
        kids.push(h('div', { class: 'pfacts' }, pl ? `${cap(pl.mode)} playlist, ${pl.entries} ${pl.entries === 1 ? 'entry' : 'entries'}. ` : 'Playlist. ', 'Started ', h('b', {}, when(pr.t0)), ` ${how}.`));
      }
      if (pr.stack.length > 1) kids.push(h('div', { class: 'crumbs dim pdim', style: 'margin-top:4px' }, pr.stack.map((s, i) => (i ? ' > ' : '') + s.playlist + ` (row ${s.position})`).join('')));
      const bt = d.back_time;
      if (bt && bt.state && bt.state !== 'off') kids.push(h('div', { class: 'row btline', style: 'margin-top:8px;gap:8px' }, h('span', { class: 'chip ' + (bt.state === 'fallback' ? 'crit' : bt.state === 'within_tolerance' ? 'warn' : 'ok'), title: 'Planned end minus the end time' }, bt.label || 'END'),
        h('span', { class: 'dim pdim' }, `back-timing ${BT_TEXT[bt.state] || bt.state}${bt.mode ? ', ' + String(bt.mode).replace('_', ' ') : ''}${bt.target ? ', end time ' + hhmm(bt.target) : ''}`)));
      const rot = st.state?.active_playlist?.rotation;
      if (!hasBlocks && rot?.capped) kids.push(h('div', { class: 'notice' }, `No repeat: set to ${Math.round(rot.no_repeat_configured)} min, using ${Math.round(rot.no_repeat_effective)} min (only ${rot.units} ${rot.units === 1 ? 'track' : 'tracks'}). `, h('a', { href: '#/playlists/' + (st.state.active_playlist.id) }, 'Add tracks ›')));
      if (pr.waiting && pr.waiting.kind !== 'operator') kids.push(h('div', { class: 'notice waitline' }, `Waiting: ${pr.waiting.name} (${pr.waiting.kind === 'operator' ? 'for the operator' : pr.waiting.kind === 'time' ? 'until ' + hhmm(pr.waiting.until) : 'for a length of time'})`));
      const nextId = d.next_block?.block_id;
      if (hasBlocks) {
        // a compact program unit (the Layout control) shows the current block only, or the next one; the rest on demand
        const compact = layout.prog === 'compact' && !rdOpen;
        const pick = blocks.find(b => b.current) || blocks.find(b => b.block_id === nextId) || blocks.find(b => b.status === 'pending');
        const shown = compact ? (pick ? [pick] : []) : blocks;
        const head = h('div', { class: 'rd head', role: 'row' }, h('span', { class: 'n' }, '#'), h('span', {}, 'Block'), h('span', {}, 'Name'), h('span', { class: 'at' }, 'At'), h('span', { class: 'in' }, 'In'), h('span', { class: 'act x' }));
        const rows = shown.map(b => {
          const pending = b.status === 'pending', due = b.in_ms != null && pending ? Date.now() + b.in_ms : '';
          const trig = b.trigger === 'timed' ? 'at a time' : b.trigger === 'sequence' ? 'in sequence' : b.trigger;
          const fire = pending ? h('button', { type: 'button', class: 'f-fire', 'aria-label': `Fire ${b.name} now: press, then press again within 3 seconds` }, 'Fire') : null;
          if (fire) rowGuards.push(coverKey(fire, 3000, () => pact(`program/blocks/${b.block_id}/fire`, {}, 'Fire'), { text: () => `Fire ${b.badge}?` }));
          return h('div', { class: 'rd' + (b.current ? ' cur' : '') + (!pending && !b.current ? ' done' : ''), role: 'row', 'data-block': b.block_id },
            h('span', { class: 'n' }, b.position), h('span', {}, h('span', { class: 'bdg', 'data-b': b.badge }, b.badge)),
            h('span', { class: 'nm', style: `padding-left:${16 * (b.depth || 0)}px` }, b.name, h('small', {}, trig, h('span', { class: 'ptm' }, (b.at ? ' · ' + hhmm(b.at) : '') + (due !== '' ? ' · in ' : ''), due !== '' ? h('span', { class: 'btm', 'data-due': due }, mmss(b.in_ms)) : null))),
            h('span', { class: 'at' }, b.at ? hhmm(b.at) : ''), h('span', { class: 'in btm', 'data-due': due }, due !== '' ? mmss(b.in_ms) : ''),
            h('span', { class: 'act' }, pending ? [fire, b.block_id === nextId ? h('button', { type: 'button', class: 'f-skip' }, 'Skip') : null] : h('span', { class: 'chip ' + (STAT_CLASS[b.status] || '') }, b.status + (b.deviation_ms != null ? ` (${(b.deviation_ms / 1000).toFixed(1)} s)` : ''))));
        });
        for (const r of rows) { const sk = r.querySelector('.f-skip'); if (sk) sk.addEventListener('click', () => pact('program/blocks/next/skip', {}, 'Skip')); }
        kids.push(h('div', { class: 'rundown', role: 'table', 'aria-label': 'Rundown' }, compact ? null : head, ...rows));
        if (layout.prog === 'compact') kids.push(h('button', { type: 'button', class: 'lnk f-rundown', 'aria-expanded': String(rdOpen), onclick: () => { rdOpen = !rdOpen; root.classList.toggle('rd-open', rdOpen); paintProg(); } },
          rdOpen ? 'Hide the rundown' : `Show the rundown (${blocks.length} blocks)`));
      }
    }
    progView.replaceChildren(...kids);
  }
  const closeMenuIfGone = d => { if (menuOpen && !d) closeMenu(); };

  // ---- the Change program sheet: ONE operation. A switch is a program start with T0 = now in the station. ----
  // A playlist without blocks calls playlist/switch (no T0 question); a playlist with blocks calls program/start with `t0` and
  // `how`, after a dry run shown inline. Both endpoints stay as they are.
  let changeDlg = null; const plDetail = new Map();
  onCleanup(() => { if (changeDlg) { try { changeDlg.close(); } catch { } changeDlg.remove(); changeDlg = null; } });
  async function plKind(id) {
    if (plDetail.has(id)) return plDetail.get(id);
    const d = await safe(J('/api/v1/playlists/' + id)); if (!d) return null;
    const bl = (d.entries || []).filter(e => e.type === 'block' && e.block && e.block.kind !== 'insert_every');
    const k = { blocks: bl.length > 0, timed: bl.some(e => e.block.trigger === 'timed' || e.block.kind === 'start') };
    plDetail.set(id, k); return k;
  }
  function openChange() {
    if (changeDlg) return;
    if (!plist.length) { toast('No playlists yet', true); return; }
    const s = st.state || {}, onId = prog?.program?.playlist_id ?? s.active_playlist?.id;
    const sel = { id: onId && plist.some(p => p.id === onId) ? onId : plist[0]?.id, how: null, t0: 'now', kind: null };
    const dlg = changeDlg = h('dialog', { class: 'unit sheet', 'aria-label': 'Change program' });
    const radios = h('div', { class: 'plpick', role: 'radiogroup', 'aria-label': 'Playlist' });
    const howBox = h('div', { class: 'segc', role: 'group', 'aria-label': 'The first song starts' });
    const t0Wrap = h('div', { hidden: true }), t0Box = h('div', { class: 'segc', role: 'group', 'aria-label': 'T0' });
    const dry = h('div', { class: 'dry', hidden: true }), out = h('div', { class: 'startout' });
    const goBtn = h('button', { type: 'button', class: 'primary', disabled: true }, 'Start'), cancelBtn = h('button', { type: 'button', onclick: () => dlg.close() }, 'Cancel');
    const HOWS = [['segue', 'At the next song change', 'normal segue'], ['now', 'Now', '2 s fade'], ['end', 'When this song ends', 'no overlap']];
    const paintHow = () => { howBox.replaceChildren(...HOWS.map(([v, t, sm]) => h('button', { type: 'button', class: (sel.how === v ? 'on ' : '') + 'f-proghow', 'data-how': v, 'aria-pressed': String(sel.how === v), onclick: () => { sel.how = v; sel.howSet = true; paintHow(); } }, t, h('small', {}, sm)))); };
    const paintT0 = () => { t0Box.replaceChildren(...[['now', 'Now', 'T0 = the press'], ['nominal', 'The expected start', 'T0 = the time the show expects']].map(([v, t, sm]) => h('button', { type: 'button', class: (sel.t0 === v ? 'on ' : '') + 'f-progt0', 'data-t0': v, 'aria-pressed': String(sel.t0 === v), onclick: () => { sel.t0 = v; paintT0(); dryRun(); } }, t, h('small', {}, sm)))); };
    let dryTok = 0;
    async function dryRun() {
      if (!sel.kind?.blocks) { dry.hidden = true; return; }
      const tok = ++dryTok; dry.hidden = false; dry.replaceChildren('Dry run: working');
      const pv = await safe(J(`/api/v1/playlists/${sel.id}/preview?t0=${sel.t0}`), 'Dry run'); if (tok !== dryTok) return;
      if (!pv) { dry.replaceChildren(h('div', { class: 'chip crit' }, 'The dry run did not answer')); return; }
      const errs = (pv.problems || []).filter(x => x.severity === 'error');
      dry.replaceChildren(h('div', {}, h('b', {}, `Dry run, T0 ${String(pv.t0).replace('T', ' ')}: `), pv.past.length ? `${pv.past.length} block${pv.past.length === 1 ? ' is' : 's are'} already past. Its late policy decides what happens to each.` : 'no block is already past.'),
        pv.past.length ? pastTable(pv.past) : null, ...errs.map(x => h('div', { class: 'chip crit', style: 'white-space:normal;display:block;margin-top:4px' }, h('code', {}, x.code), ' ', x.message)));
    }
    async function pick() {
      for (const l of radios.children) l.classList.toggle('on', +l.dataset.id === sel.id);
      goBtn.disabled = true; out.replaceChildren(); const id = sel.id, name = plist.find(p => p.id === id)?.name || '';
      goBtn.textContent = 'Start ' + name;
      const k = await plKind(id); if (id !== sel.id) return; sel.kind = k;
      if (!k) { goBtn.disabled = true; return; }
      if (!sel.howSet) sel.how = k.blocks ? 'now' : 'segue';
      paintHow(); t0Wrap.hidden = !k.timed; if (!k.timed) sel.t0 = 'now'; paintT0();
      goBtn.className = 'primary ' + (k.blocks ? 'f-confirmstart' : 'f-switch'); goBtn.disabled = false; dryRun();
    }
    for (const p of plist) {
      const here = p.id === onId;
      const tag = h('span', {}, here ? h('span', { class: 'chip ok' }, 'on air now') : null);
      plKind(p.id).then(k => { if (k?.blocks) tag.append(h('span', { class: 'chip warn' }, 'show')); });
      radios.append(h('label', { 'data-id': p.id }, h('input', { type: 'radio', name: 'pl', class: 'f-progpl', value: p.id, checked: p.id === sel.id, onchange: () => { sel.id = p.id; pick(); } }),
        h('span', {}, h('b', {}, p.name), h('small', {}, ` · ${p.mode} · ${p.entries} entries`)), tag));
    }
    goBtn.addEventListener('click', async () => {
      const id = sel.id, k = sel.kind; if (!k) return; goBtn.disabled = true;
      if (!k.blocks) {
        const r = await act('playlist/switch', { playlist_id: id, how: sel.how }, 'Switch');
        if (r) { toast('Switched to ' + (plist.find(p => p.id === id)?.name || '')); refreshProg(); dlg.close(); } else goBtn.disabled = false;
        return;
      }
      let r;
      try { r = await jsend(stApi('program/start'), { playlist_id: id, t0: sel.t0, how: sel.how, source: 'ui' }); }
      catch (e) {
        // The station can refuse after it has already made the playlist the program (nothing queued yet): look at what it is running.
        toast('Start: ' + e.message, true); await refreshProg();
        out.replaceChildren(h('div', { class: 'notice bad startwarn' }, `The station answered: ${e.message}.` + (prog?.program?.playlist_id === id ? ' The program is running anyway: check the blocks.' : ' The program did not start.')));
        goBtn.disabled = false; return;
      }
      const past = r.past || [];
      out.replaceChildren(...[h('div', { class: 'notice ok startdone' }, `Started "${r.program?.playlist ?? ''}" with T0 ${r.program ? hhmm(r.program.t0) : ''}.` + (past.length ? ` ${past.length} block${past.length === 1 ? ' was' : 's were'} already past:` : ' No block was already past.')), past.length ? pastTable(past) : null].filter(Boolean));
      dry.hidden = true; goBtn.hidden = true; cancelBtn.textContent = 'Close';
      refreshProg();
    });
    dlg.append(h('div', { class: 'uhead' }, h('span', { class: 't' }, 'Change program'), helpLink('onair', 'switch-playlist'), h('span', { class: 'r' }, h('button', { type: 'button', onclick: () => dlg.close() }, 'Close'))),
      h('div', { class: 'ubody' }, h('div', {}, h('div', { class: 'fl' }, 'Playlist'), radios), h('div', {}, h('div', { class: 'fl' }, 'The first song starts'), howBox),
        h('div', {}, t0Wrap), dry, out), h('div', { class: 'foot' }, cancelBtn, goBtn));
    t0Wrap.append(h('div', { class: 'fl' }, 'T0, the show\'s zero (this playlist has timed blocks)'), t0Box);
    const opener = document.activeElement;
    dlg.addEventListener('close', () => { dlg.remove(); if (changeDlg === dlg) changeDlg = null; dryTok++; try { opener?.focus?.(); } catch { } });
    root.append(dlg); dlg.showModal(); pick();
  }

  // ---------------- the queue unit: library search, Reshuffle, the rows ----------------
  // the library search and the insert, shared by the queue unit and the Add sheet
  const searchItems = (text, n) => safe(J(`/api/v1/items?per_page=${n}&q=` + encodeURIComponent(text)));
  const insertItem = (i, at) => act('queue', { item_id: i.id, at }, 'Insert');
  function queueUnit() {
    const body = h('div', { class: 'ubody', style: 'padding-top:8px' });
    const q = h('input', { class: 'qsearch f-insert', type: 'search', placeholder: 'Insert from the library: search', 'aria-label': 'Insert from the library: search' });
    const results = h('div', { class: 'qres' });
    qrows = h('div', { class: 'qrows' });
    // on a desktop the page fits the window: the rows scroll inside the unit, under the search (onair.css)
    body.append(q, h('div', { class: 'qscroll' }, results, qrows));
    let qt; q.addEventListener('input', () => { clearTimeout(qt); qt = setTimeout(async () => {
      if (!q.value.trim()) { results.replaceChildren(); return; }
      const r = await searchItems(q.value, 8); if (!r) return;
      results.replaceChildren(...r.items.map(i => h('div', { class: 'qr' }, artImg(i.art_url, 'thumb'), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, i.title), h('div', { class: 'aa' }, i.artist)),
        h('button', { type: 'button', onclick: () => insertItem(i, 'next') }, 'Next'), h('button', { type: 'button', onclick: () => insertItem(i, 'end') }, 'End'))));
    }, 200); });
    onCleanup(() => clearTimeout(qt));
    return h('section', { class: 'unit', id: 'queuebox' }, h('div', { class: 'uhead' }, h('span', { class: 't' }, 'Queue'), helpLink('onair', 'queue'), h('span', { class: 'r' }, h('button', { type: 'button', class: 'f-reshuffle', onclick: () => act('playlist/reshuffle', {}, 'Reshuffle') }, 'Reshuffle'))), body);
  }
  // ---- the Add sheet (the phone dock's Add key): the queue's library search on the whole screen, insert as next or at the end ----
  let addDlg = null;
  onCleanup(() => { if (addDlg) { try { addDlg.close(); } catch { } addDlg?.remove(); addDlg = null; } });
  function openAdd() {
    if (addDlg) return;
    let at = 'next', qt, tok = 0;
    const q = h('input', { class: 'qsearch f-addsearch', type: 'search', placeholder: 'Search the library', 'aria-label': 'Search the library', autocomplete: 'off', enterkeyhint: 'search' });
    const atBox = h('div', { class: 'segc', role: 'group', 'aria-label': 'Insert as' });
    const paintAt = () => atBox.replaceChildren(...[['next', 'Next', 'after this item'], ['end', 'At the end', 'of the queue']].map(([v, t, sm]) =>
      h('button', { type: 'button', class: (at === v ? 'on ' : '') + 'f-addat', 'data-at': v, 'aria-pressed': String(at === v), onclick: () => { at = v; paintAt(); } }, t, h('small', {}, sm))));
    paintAt();
    const results = h('div', { class: 'qres addres', 'aria-live': 'polite' }, h('div', { class: 'qempty' }, 'Type a title, an artist or an album.'));
    const dlg = addDlg = h('dialog', { class: 'unit sheet addsheet', 'aria-label': 'Add to the queue' });
    q.addEventListener('input', () => { clearTimeout(qt); qt = setTimeout(async () => {
      const t = ++tok, text = q.value.trim();
      if (!text) { results.replaceChildren(h('div', { class: 'qempty' }, 'Type a title, an artist or an album.')); return; }
      const r = await searchItems(text, 20); if (!r || t !== tok) return;
      if (!r.items.length) { results.replaceChildren(h('div', { class: 'qempty' }, 'Nothing found.')); return; }
      results.replaceChildren(...r.items.map(i => {
        const b = h('button', { type: 'button', class: 'primary f-add', 'aria-label': `Add ${i.title}` }, 'Add');
        b.addEventListener('click', async () => {
          b.disabled = true; const where = at;
          if (await insertItem(i, where)) { b.textContent = 'Added'; toast(`Added: ${i.title}, ${where === 'next' ? 'next' : 'at the end'}`); } else b.disabled = false;
        });
        return h('div', { class: 'qr' }, artImg(i.art_url, 'thumb'), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, i.title), h('div', { class: 'aa' }, [i.artist, i.album].filter(Boolean).join(' - '))), b);
      }));
    }, 200); });
    dlg.append(h('div', { class: 'uhead' }, h('span', { class: 't' }, 'Add to the queue'), helpLink('onair', 'add'), h('span', { class: 'r' }, h('button', { type: 'button', class: 'f-addclose', onclick: () => dlg.close() }, 'Close'))),
      h('div', { class: 'ubody' }, q, h('div', {}, h('div', { class: 'fl' }, 'Insert as'), atBox), results));
    const opener = document.activeElement;
    dlg.addEventListener('close', () => { clearTimeout(qt); tok++; dlg.remove(); if (addDlg === dlg) addDlg = null; try { opener?.focus?.(); } catch { } });
    root.append(dlg); dlg.showModal(); q.focus();
  }
  const trNamesP = J('/api/v1/transition-presets').catch(() => ({ items: [] })).then(r => r.items.map(p => p.name));
  // The per-entry transition (2.6, resolution step 4): how the boundary INTO this track is made, for this one boundary.
  function transitionMenu(e) {
    const t = e.transition, custom = t && t !== 'cut' && (t.patch || !trNames.includes(t.preset));
    const cur = !t ? '' : t === 'cut' ? 'cut' : custom ? 'custom' : t.preset;
    const sel = h('select', { class: 'trsel', 'data-entry': e.entry_id, 'aria-label': 'Transition into ' + e.title, title: e.continuity_from_prev ? 'A gapless run ignores every transition setting.' : 'How this track starts after the one before it, this time only. Default follows the category, the item and the station.', disabled: !!e.continuity_from_prev },
      h('option', { value: '' }, 'transition: default'), h('option', { value: 'cut' }, 'cut (no overlap)'), ...trNames.map(n => h('option', { value: n }, n)), custom ? h('option', { value: 'custom', disabled: true }, 'custom patch') : null);
    sel.value = cur;
    sel.addEventListener('change', async () => {
      const v = sel.value, transition = v === '' ? null : v === 'cut' ? 'cut' : { preset: v };
      const r = await safe(jsend(stApi('queue/' + e.entry_id), { transition }, 'PATCH'), 'Transition'); if (r) { st.state = r; paint(); } else sel.value = cur;
    });
    return sel;
  }
  function queueRow(e) {
    if (e.kind === 'block') return blockQueueRow(e);
    const trWord = !e.transition ? null : e.transition === 'cut' ? 'cut' : (e.transition.preset || 'custom');
    const more = h('button', { type: 'button', class: 'more f-more-row', 'aria-expanded': String(openRow === e.entry_id), 'aria-label': 'Row menu: transition and remove', title: 'Transition for this song, and Remove' });
    more.textContent = '⋯';
    const menu = h('div', { class: 'qmenu', hidden: openRow !== e.entry_id }, transitionMenu(e),
      h('button', { type: 'button', class: 'f-remove', title: 'Remove', onclick: () => safe(jsend(stApi('queue/' + e.entry_id), {}, 'DELETE'), 'Remove').then(r => { if (r) { st.state = r; paint(); } }) }, 'Remove'));
    more.addEventListener('click', () => { const open = menu.hidden; for (const m of qrows.querySelectorAll('.qmenu')) m.hidden = true; for (const m of qrows.querySelectorAll('.more')) m.setAttribute('aria-expanded', 'false'); menu.hidden = !open; more.setAttribute('aria-expanded', String(open)); openRow = open ? e.entry_id : null; });
    const row = h('div', { class: 'qr', draggable: true, 'data-entry': e.entry_id },
      artImg(e.art_url, 'thumb'), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, e.title),
        h('div', { class: 'aa' }, e.artist || '', e.origin && e.origin !== 'rotation' ? h('span', { class: 'chip' }, e.origin) : null, e.continuity_from_prev ? h('span', { class: 'chip' }, 'gapless') : null, e.pinned ? h('span', { class: 'chip warn' }, 'pinned') : null, trWord ? [' · ', h('span', { class: 'tr' }, trWord)] : null)),
      h('span', { class: 'tm', 'data-t': e.entry_id }, hhmm(e.planned_start)), more, menu);
    row.addEventListener('dragstart', ev => { ev.dataTransfer.setData('text/plain', String(e.entry_id)); row.classList.add('drag'); });
    row.addEventListener('dragend', () => row.classList.remove('drag'));
    row.addEventListener('dragover', ev => { ev.preventDefault(); row.classList.add('dropbefore'); });
    row.addEventListener('dragleave', () => row.classList.remove('dropbefore'));
    row.addEventListener('drop', ev => { ev.preventDefault(); row.classList.remove('dropbefore'); const id = +ev.dataTransfer.getData('text/plain'); const to = st.state.queue.filter(x => x.kind !== 'block').findIndex(x => x.entry_id === e.entry_id); if (id && id !== e.entry_id) act(`queue/${id}/move`, { to }, 'Move'); });
    return row;
  }
  // A block of the program in the queue: its badge, name and the time to go. It has no drag handle, no remove and no transition.
  function blockQueueRow(e) {
    const badge = e.badge || BADGE[e.type] || String(e.type || 'BLK').toUpperCase();
    const w = prog?.program?.waiting, hold = w && w.kind === 'operator' && w.block_id === e.block_id;
    return h('div', { class: 'qr blk', 'data-block': e.block_id },
      h('span', { class: 'bdg', 'data-b': badge }, badge),
      h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, e.name || e.type || ''), h('div', { class: 'aa' }, e.trigger === 'timed' ? 'at a time' : 'in sequence', e.planned_start ? ' ' + hhmm(e.planned_start) : '')),
      hold ? h('span', { class: 'tm' }, 'hold') : h('span', { class: 'btm tm', 'data-due': e.in_ms != null ? Date.now() + e.in_ms : '' }, e.in_ms != null ? mmss(e.in_ms) : ''), h('span'));
  }
  function paintQueue(s) {
    const w = prog?.program?.waiting;
    if (changed('queue', [(s.queue || []).map(e => e.kind === 'block' ? ['block', e.block_id, e.name, e.badge, e.trigger, e.planned_start] : [e.entry_id, e.title, e.artist, e.art_url, e.origin, e.pinned, e.continuity_from_prev, e.transition]), w && w.block_id, trNames.length])) {
      qrows.replaceChildren(...(s.queue || []).map(e => queueRow(e)), ...((s.queue || []).length ? [] : [h('div', { class: 'qempty' }, 'The queue is empty.')]));
    }
    for (const e of s.queue || []) {
      if (e.kind === 'block' || e.entry_id == null) continue;
      const t = qrows.querySelector(`[data-t="${e.entry_id}"]`); if (t) setText(t, hhmm(e.planned_start));
      const c = comingBox.querySelector(`[data-nt="t${e.entry_id}"]`); if (c && c.textContent !== 'after Continue') setText(c, hhmm(e.planned_start));
    }
  }

  // ---------------- alerts ----------------
  const alBody = h('div', { class: 'ubody', style: 'padding-top:6px' }), alSum = h('span', { class: 'sum' }, 'none');
  function alertsUnit() { return h('section', { class: 'unit', id: 'alertsbox' }, h('div', { class: 'uhead' }, h('span', { class: 't' }, 'Alerts'), helpLink('alerts'), alSum), alBody); }
  function paintAlerts(s) {
    if (!changed('alerts', (s.alerts || []).map(a => [a.id, a.severity, a.code, a.message]))) return;
    alBody.hidden = !s.alerts?.length; setText(alSum, s.alerts?.length ? `${s.alerts.length} active` : 'none');
    alBody.replaceChildren(...(s.alerts || []).map(a => h('div', { class: 'qr', style: 'grid-template-columns:auto minmax(0,1fr) auto' }, h('span', { class: 'chip ' + (a.severity === 'critical' ? 'crit' : 'warn') }, a.severity), h('div', { style: 'min-width:0' }, h('div', { class: 'tt' }, a.code), h('div', { class: 'aa', style: 'white-space:normal' }, a.message)),
      h('button', { type: 'button', onclick: () => act(`alerts/${a.id}/ack`, {}, 'Acknowledge') }, 'Ack'))));
  }

  // ---------------- the processing display (onairlayout.js): A, B or C, or none; one at a time ----------------
  // It is painted from the meter frame in `tick()`, about 15 times a second, and writes only what changed. A and C also read
  // the AGC's state (GET /chain/agc) once a second while the page is visible, for its FADE and CLAMPING lights.
  const pModel = new OnAirProc.Model();
  let pdisp = null, pKind = null, pchain = null, pchainSeen = null, pchainAt = 0, pPaintAt = 0;
  const presetName = () => pchain ? (pchain.user_preset || pchain.preset || '') : '';
  function mountProc(kind) {
    if (pKind === kind) return;
    pdisp?.el.remove(); pdisp = null; pKind = kind;
    if (!OnAirProc.build[kind]) return;
    pdisp = OnAirProc.build[kind]();
    if (kind === 'a') colsEl.after(pdisp.el); else if (kind === 'b') lufsEl.after(pdisp.el); else leftCol.append(pdisp.el);
    pdisp.preset(presetName()); pdisp.paint(null); pPaintAt = 0;
  }
  const loadPChain = () => { pchainAt = Date.now(); J(stApi('chain')).then(c => { pchain = c; pchainSeen = st.frame ? st.frame.chainRev : c.chain_cfg_rev; pdisp?.preset(presetName()); }).catch(() => { }); };
  const agcTimer = setInterval(() => { if (pdisp?.agc && document.visibilityState === 'visible') J(stApi('chain/agc')).then(v => pModel.setAgc(v)).catch(() => { }); }, 1000);
  onCleanup(() => clearInterval(agcTimer));
  // the layout's choices on the root (onair.css), the processing display moved, the program unit painted again if it changed
  function applyLayout(l) {
    const was = layout; layout = l;
    root.dataset.proc = l.proc; root.dataset.queue = l.queue === 'full' ? 'full' : 'compact'; root.dataset.prog = l.prog; root.dataset.split = l.split; root.dataset.phone = l.phone;
    if (l.queue === 'full') root.style.removeProperty('--qn'); else root.style.setProperty('--qn', l.queue);
    if (l.prog !== 'compact' && rdOpen) { rdOpen = false; root.classList.remove('rd-open'); }
    mountProc(l.proc);
    if (was.prog !== l.prog) paintProg();
  }

  function refreshAlerts() {
    J(stApi('state')).then(x => { if (!st.state) return; st.state.alerts = x.alerts; st.state.operating_mode = x.operating_mode; st.state.health = x.health; paint(); }).catch(() => { });
  }
  // ---------------- one paint for the station's state ----------------
  function paint() {
    const s = st.state; if (!s) return;
    tabInfo.set(S.station, { ...(tabInfo.get(S.station) || {}), ...tabFromState(s) }); paintTabs();
    paintUnit(s); paintKeys(s); paintQueue(s); paintAlerts(s); paintSettingsLine(s);
  }
  // data that arrives once: the playlists (names and modes for the sheet and the status block), the box's view, the meters setting
  J('/api/v1/playlists').then(r => { plist = r.playlists; sigs.delete('prog'); paintProg(); }).catch(() => { });
  const loadMeterCfg = () => J('/api/v1/display/meters?station=' + encodeURIComponent(S.station)).then(d => { meterCfg = d; window.__meters = d; if (st.state) paintSettingsLine(st.state); }).catch(() => { });
  loadMeterCfg();
  const leftCol = h('div', { class: 'col', id: 'leftcol' },
    h('section', { class: 'unit', id: 'progbox' }, h('div', { class: 'uhead' }, pTitle, progHelp, h('span', { class: 'r' }, changeBtn, moreBtn)), pmenu, h('div', { class: 'ubody progbody' }, progView, settingsLine, xpop)));
  const colsEl = h('div', { class: 'cols' }, leftCol, h('div', { class: 'col', id: 'rightcol' }, queueUnit(), alertsUnit()));
  root.append(colsEl);
  applyLayout(layout);
  trNames = await trNamesP;
  const first = await safe(J(stApi('state')), 'Station');
  if (!first) { root.append(h('div', { class: 'notice bad' }, 'The station is not answering.')); return root; }
  st.state = first; st.now = first.now; paint();
  prog = await J(stApi('program/state')).catch(() => null); paintProg(); paintKeys(first); paintNexts();
  refreshTabs();
  const reload = () => { J(stApi('state')).then(x => { st.state = x; st.now = x.now; paint(); }).catch(() => { }); refreshProg(); refreshTabs(); };
  let firstOpen = true;
  const live = new Live(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/v1/stations/${S.station}/ws?topics=state,now,queue,alerts,meters,program`, {
    program(d) { prog = d; paintProg(); paintKeys(st.state || {}); paintNexts(); paintQueue(st.state || {}); },
    snapshot(d) { st.state = d; st.now = d.now; paint(); },
    now(d) { st.now = d; paint(); },
    state(d) {
      if (!st.state) return;
      const was = st.state.state; Object.assign(st.state, d);
      // the audio stopped: the clocks stand where it stopped; it started again: read what is on air now
      if (d.state && d.state !== 'playing' && was === 'playing') st.stoppedAt = Date.now();
      if (d.state === 'playing') { st.stoppedAt = null; if (was !== 'playing') J(stApi('now')).then(n => { st.now = n && n.playing !== false ? n : null; paint(); }).catch(() => { }); }
      paint();
    },
    queue(d) { if (!st.state) return; st.state.queue = d.entries; paint(); },
    // an alert raised or cleared: read the alert list again, and only it (a full state read could arrive after a newer
    // `state` event and put an old transport back, as when the card said STOPPED while the station played)
    alert() { refreshAlerts(); },
    alert_cleared() { refreshAlerts(); },
    meters(f) { st.frame = f; st.frameAt = Date.now(); },
    // after the server (or the network) was away: the page reads everything again, so nothing stays as it was
    open() { root.classList.remove('off'); if (firstOpen) { firstOpen = false; return; } reload(); },
    close() { root.classList.add('off'); },
  });
  onCleanup(() => live.stop());
  // a phone or a laptop that slept: the socket may be dead without telling; read the state when the page is shown again
  const vis = () => { if (document.visibilityState === 'visible') reload(); };
  document.addEventListener('visibilitychange', vis); onCleanup(() => document.removeEventListener('visibilitychange', vis));
  let raf; const bars = new Ballistics.Bars(); Ballistics.follow(S.station);
  loadPChain();
  const pct = db => db == null ? 0 : Math.max(0, Math.min(1, (db + 60) / 60));
  const setLed = (led, db, hold) => {
    const lit = led.firstChild, pk = led.lastChild, v = (100 * (1 - pct(db))).toFixed(1) + '%';
    if (lit.dataset.v !== v) { lit.dataset.v = v; lit.style.clipPath = `inset(0 ${v} 0 0)`; }
    const p = hold == null ? '-4px' : `calc(${(100 * pct(hold)).toFixed(1)}% - 2px)`; if (pk.dataset.v !== p) { pk.dataset.v = p; pk.style.left = p; }
  };
  const tick = () => {
    const n = st.now, now = Date.now();
    if (n) {
      const a = Date.parse(n.started_at), z = Date.parse(n.ends_at), g = n.segue_at ? Date.parse(n.segue_at) : null;
      // Held, stopped or past the item's end: nothing is extrapolated (the unit used to count below zero while a station was
      // held); the item reads 0:00 left and the clock says HELD and for how long, as the native display does.
      const s = st.state, stt = s?.state, over = now >= z, idle = over || stt === 'stopped' || stt === 'safe';
      // stopped: the elapsed time and the bar stand where the audio stopped (a fade out at T0 + 14:40)
      const upto = stt === 'stopped' && st.stoppedAt ? Math.min(now, st.stoppedAt) : now;
      const el = Math.min(Math.max(0, upto - a), z - a);
      // playing, but the item the page knows has ended: what plays now is read again (a missed `now` event)
      if (stt === 'playing' && over && now - (st.nowCheckAt || 0) > 3000) { st.nowCheckAt = now; J(stApi('now')).then(x => { if (x && x.playing !== false && x.entry_id !== n.entry_id) { st.now = x; paint(); } }).catch(() => { }); }
      setCls(clocksEl, 'long', z - a >= 3599500);   // the clocks read h:mm:ss: wider for the whole item (onair.css)
      setText(cEl, mmss(el)); setText(cRm, mmss(idle ? 0 : z - now)); const pw = Math.max(0, Math.min(100, 100 * el / (z - a))).toFixed(1) + '%'; if (pg.style.width !== pw) pg.style.width = pw;
      const lab = { held: 'HELD', stopped: 'STOPPED', safe: 'SAFE MODE' }[stt];
      let v = '', sub = '';
      if (idle) v = lab ? lab + (over ? ' ' + mmss(now - z) : '') : '';
      else if (stt === 'held') v = 'HELD, ENDS ' + mmss(z - now);
      else if (s?.hold) { v = 'HOLDS AT END'; sub = 'hold: then waits'; }
      else if (s?.mode === 'assist' && !n.continuity_next) { v = 'HOLDS AT END'; sub = waitingOp() ? 'assist: then waits for Continue' : 'assist: then waits for Next'; }
      else if (n.continuity_next) { v = 'ALBUM: gapless'; sub = 'to the next track'; }
      else if (g) { v = 'SEGUE IN ' + mmss(Math.max(0, g - now)); sub = 'the next item starts'; }
      setText(cSg, v); setText(cSgL, sub);
    } else { setCls(clocksEl, 'long', false); setText(cEl, '0:00'); setText(cRm, '0:00'); setText(cSg, ''); setText(cSgL, ''); }
    for (const el of root.querySelectorAll('.btm[data-due]')) {
      if (el.dataset.due === '') continue;
      const left = Math.max(0, +el.dataset.due - now); setText(el, mmss(left));
      el.classList.toggle('red', left < 10000); el.classList.toggle('amber', left >= 10000 && left < 60000);
    }
    const f = st.frame;
    const live = f && now - (st.frameAt || 0) < 500 ? f : null;
    const b = bars.update(live, now);
    setLed(ledL, b.frame.ppmL, b.hold[0]); setLed(ledR, b.frame.ppmR, b.hold[1]);
    const fm = v => v == null ? '--' : v.toFixed(1);
    [f?.momentary, f?.shortTerm, f?.window, f?.integrated].forEach((v, i) => setText(nums[i].lastChild, fm(v).padStart(6)));
    // the processing display, about 15 times a second; the chain is read again when the frame says it changed
    if (live && pchainSeen != null && live.chainRev !== pchainSeen && now - pchainAt > 900) loadPChain();
    if (pdisp && now - pPaintAt > 66) { pPaintAt = now; pdisp.paint(pModel.update(live, now)); }
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick); onCleanup(() => cancelAnimationFrame(raf));
  return root;
};
function rotationText(r) {
  // "Set to 120 min, using 84 min (only 18 tracks)": both no-repeat values, always.
  const using = Math.round(r.no_repeat_effective);
  return `No repeat: set to ${r.no_repeat_configured} min, using ${using} min` + (r.capped ? ` (only ${r.units} ${r.units === 1 ? 'track' : 'tracks'})` : '');
}

// ================= Library =================
routes.library = async () => {
  const root = h('div'); root.append(h('h1', {}, 'Library', helpLink('library')));
  const cats = (await J('/api/v1/categories')).categories;
  const st = { q: '', category: '', flag: '', page: 1, sort: 'title', view: 'items' };
  const q = h('input', { placeholder: 'Search title, artist, album', style: 'min-width:260px' });
  const cat = h('select', {}, h('option', { value: '' }, 'All categories'), cats.map(c => h('option', { value: c.name }, c.name + ' (' + c.items + ')')));
  const flag = h('select', {}, [['', 'Any flag'], ['long_intro', 'long_intro'], ['ambient_suspect', 'ambient_suspect'], ['truncated_fade', 'truncated_fade'], ['quarantined', 'quarantined'], ['disabled', 'disabled'], ['no_art', 'no artwork']].map(([v, l]) => h('option', { value: v }, l)));
  const sort = h('select', {}, [['title', 'Title'], ['artist', 'Artist'], ['album', 'Album'], ['imported', 'Newest'], ['duration', 'Longest']].map(([v, l]) => h('option', { value: v }, l)));
  const viewBtn = h('button', {}, 'Albums');
  const list = h('div'), pager = h('div', { class: 'row' });
  const drop = h('div', { class: 'dropzone' }, 'Drop audio files here, or ', h('a', { href: '#', onclick: e => { e.preventDefault(); fileIn.click(); } }, 'choose files'));
  const fileIn = h('input', { type: 'file', multiple: true, style: 'display:none', accept: 'audio/*,.flac,.mp3,.m4a,.wav,.aif,.aiff,.ogg' });
  const upCat = h('select', {}, cats.map(c => h('option', { value: c.name }, c.name)));
  const jobs = h('div');
  root.append(h('div', { class: 'card', style: 'margin-bottom:14px' }, h('h3', {}, 'Import', helpLink('library', 'import')), drop, fileIn, h('div', { class: 'row', style: 'margin-top:8px' }, h('span', { class: 'dim' }, 'Category'), upCat), jobs));
  root.append(await reanalyzeCard(), await plexCard(), h('div', { class: 'row', style: 'margin-bottom:10px' }, q, cat, flag, sort, viewBtn), list, pager);
  async function upload(files) {
    if (!files.length) return;
    // A file may be 1 GB, and one request 16 GB in all: batches of at most 4 GB go one after the other.
    const GB = 1 << 30, big = files.filter(f => f.size > GB);
    if (big.length) toast(`Skipped ${big.length} file(s) over 1 GB: ${big.map(f => f.name).join(', ')}`, true);
    files = files.filter(f => f.size <= GB);
    const batches = []; let cur = [], size = 0;
    for (const f of files) { if (cur.length && size + f.size > 4 * GB) { batches.push(cur); cur = []; size = 0; } cur.push(f); size += f.size; }
    if (cur.length) batches.push(cur);
    try {
      for (const b of batches) {
        const fd = new FormData(); fd.append('category', upCat.value); for (const f of b) fd.append('file', f, f.name);
        const r = await fetch('/api/v1/imports', { method: 'POST', headers: { 'X-Requested-With': 'hotclock-ui' }, body: fd });
        if (!r.ok) throw new Error((await r.json()).error?.message || r.status);
      }
      if (files.length) { toast(`Uploading ${files.length} file(s)`); pollJobs(); }
    } catch (e) { toast('Upload failed: ' + e.message, true); }
  }
  drop.addEventListener('dragover', e => { e.preventDefault(); drop.classList.add('over'); });
  drop.addEventListener('dragleave', () => drop.classList.remove('over'));
  drop.addEventListener('drop', e => { e.preventDefault(); drop.classList.remove('over'); upload([...e.dataTransfer.files]); });
  fileIn.addEventListener('change', () => upload([...fileIn.files]));
  let timer;
  async function pollJobs() {
    const r = await J('/api/v1/imports').catch(() => null); if (!r) return;
    const recent = r.jobs.slice(0, 8);
    jobs.replaceChildren(...recent.map(j => h('div', { class: 'row', style: 'font-size:13px' }, h('span', { class: 'chip ' + (j.status === 'done' ? 'ok' : j.status === 'failed' ? 'crit' : j.status === 'duplicate' ? 'warn' : '') }, j.status), j.source, j.item_id ? h('a', { href: '#/item/' + j.item_id }, 'item ' + j.item_id) : null, j.error ? h('span', { class: 'dim' }, j.error) : null)));
    clearTimeout(timer);
    if (r.jobs.some(j => j.status === 'queued' || j.status === 'running')) timer = setTimeout(() => { pollJobs(); load(); }, 1200);
  }
  onCleanup(() => clearTimeout(timer));
  async function load() {
    if (st.view === 'albums') return loadAlbums();
    const p = new URLSearchParams({ q: st.q, category: st.category, flag: st.flag, sort: st.sort, page: st.page, per_page: 40 });
    const r = await safe(J('/api/v1/items?' + p)); if (!r) return;
    list.replaceChildren(h('table', {}, h('thead', {}, h('tr', {}, h('th', {}), h('th', {}, 'Title'), h('th', {}, 'Artist'), h('th', {}, 'Album'), h('th', {}, 'Category'), h('th', {}, 'Length'), h('th', {}, 'Flags'))),
      h('tbody', {}, r.items.map(i => h('tr', { style: 'cursor:pointer', onclick: () => location.hash = '#/item/' + i.id },
        h('td', {}, artImg(i.art_url)), h('td', {}, i.title), h('td', {}, i.artist), h('td', {}, i.album), h('td', {}, h('span', { class: 'chip' }, i.category)), h('td', { class: 'mono' }, dur(i.duration_ms)),
        h('td', {}, i.quarantined ? h('span', { class: 'chip crit' }, 'quarantined') : null, !i.enabled ? h('span', { class: 'chip warn' }, 'disabled') : null, flagChips(i.flags)))))));
    const pages = Math.max(1, Math.ceil(r.total / r.per_page));
    pager.replaceChildren(h('button', { disabled: st.page <= 1, onclick: () => { st.page--; load(); } }, 'Previous'), h('span', { class: 'dim' }, `Page ${st.page} of ${pages}, ${r.total} items`), h('button', { disabled: st.page >= pages, onclick: () => { st.page++; load(); } }, 'Next'));
  }
  async function loadAlbums() {
    const r = await safe(J('/api/v1/albums?q=' + encodeURIComponent(st.q))); if (!r) return;
    pager.replaceChildren();
    list.replaceChildren(...r.albums.map(a => h('div', { class: 'qrow' }, artImg(a.art_url), h('div', { class: 'meta' }, h('div', {}, a.album), h('div', { class: 'dim' }, `${a.album_artist || a.artist}  -  ${a.tracks} tracks, ${dur(a.duration_ms)}`)),
      h('button', { onclick: () => playAlbum(a, true) }, 'Play album (gapless)'), h('button', { onclick: () => playAlbum(a, false) }, 'Play album (normal)'))));
  }
  async function playAlbum(a, gapless) {
    if (!S.station) return;
    const r = await safe(jsend(stApi('queue/album'), { item_id: a.first_item_id, at: 'end', continuity: gapless ? 'gapless' : 'normal' }, 'POST'), 'Play album');
    if (r) toast(`Queued ${r.result.entries.length} tracks (${gapless ? 'gapless' : 'normal'})`);
  }
  let t2; q.addEventListener('input', () => { clearTimeout(t2); t2 = setTimeout(() => { st.q = q.value; st.page = 1; load(); }, 200); });
  cat.addEventListener('change', () => { st.category = cat.value; st.page = 1; load(); });
  flag.addEventListener('change', () => { st.flag = flag.value; st.page = 1; load(); });
  sort.addEventListener('change', () => { st.sort = sort.value; load(); });
  viewBtn.addEventListener('click', () => { st.view = st.view === 'items' ? 'albums' : 'items'; viewBtn.textContent = st.view === 'items' ? 'Albums' : 'Tracks'; load(); });
  await load(); pollJobs();
  return root;
};


// ================= Plex (import a playlist, fill missing artwork) =================
// The same functions as `hotclock import-plex`, behind /api/v1/plex. The token stays a file on the box: this card
// sets its path and is told only whether it is set and readable.
/** Items analyzed by an older version of the analysis: one button analyzes them again from their stored files. */
async function reanalyzeCard() {
  const r = await J('/api/v1/reanalyze').catch(() => null);
  if (!r || !r.stale) return '';
  const msg = h('span', { class: 'dim' }, `${r.stale} of ${r.items} items were analyzed by an older version of the analysis. Analyze them again to add the newer checks, such as truncated_fade. Your markers stay.`);
  const go = h('button', { class: 'primary', onclick: async () => {
    const q = await safe(jsend('/api/v1/reanalyze', { stale: true }), 'Analyze again'); if (!q) return;
    go.disabled = true; msg.textContent = `Analyzing ${q.queued} items in the background. Reload the page later to see their flags.`;
  } }, 'Analyze them again');
  return h('div', { class: 'card reanalyze', style: 'margin-bottom:14px' }, h('h3', {}, 'Analysis', helpLink('library', 'analyze-again')), h('div', { class: 'row' }, go, msg));
}
async function plexCard() {
  const card = h('div', { class: 'card', style: 'margin-bottom:14px' }, h('h3', {}, 'Plex', helpLink('plex')));
  const body = h('div'); const log = h('pre', { class: 'mono', style: 'max-height:260px;overflow:auto;margin-top:8px;font-size:12px;white-space:pre-wrap' });
  const summary = h('div', { class: 'notice', 'data-plex': 'summary', style: 'display:none;margin-top:8px' });
  const open = h('button', {}, 'Plex tools'); let shown = false;
  const st = await J('/api/v1/plex').catch(() => null);
  async function poll(id, label) {
    summary.style.display = 'none'; log.textContent = '';
    for (;;) {
      const j = await J('/api/v1/plex/jobs/' + id); log.textContent = j.lines.join('\n'); log.scrollTop = log.scrollHeight;
      if (j.state !== 'running') {
        const r = j.result || {};
        if (j.state === 'failed') summary.textContent = label + ' failed: ' + j.error;
        else if (j.kind === 'fill-art') summary.textContent = `${j.dry_run ? 'Preview: ' : ''}${r.checked} items checked, ${r.had_art} already have a picture, ${j.dry_run ? r.would_fill + ' Plex can fill' : r.filled + ' filled from Plex'}, ${r.plex_none} Plex has none, ${r.failed} failed; ${r.meta_filled} ${j.dry_run ? 'would get' : 'got'} a blank title, artist, album or year from Plex.`;
        else summary.textContent = `${j.dry_run ? 'Preview of ' : ''}${r.playlist}: ${r.tracks} tracks in Plex; ${j.dry_run ? '' : r.imported + ' imported, '}${r.known} already in the library, ${(r.failed || []).length} failed; ${j.dry_run ? 'pictures Plex can fill: ' + r.art.would_fill : r.entries_added + ' added to the playlist, ' + r.art.filled + ' pictures filled from Plex'}.`;
        summary.style.display = ''; return;
      }
      await new Promise(r => setTimeout(r, 1200));
    }
  }
  const start = async (path, b, label) => { const r = await safe(jsend(path, b), label); if (r) await poll(r.job, label); };
  const render = async () => {
    const cfg = await J('/api/v1/plex');
    const server = h('input', { value: cfg.server, style: 'width:20em' }), tok = h('input', { placeholder: cfg.token_file_set ? 'set (type a path to change it)' : '/var/lib/hotclock/plex-token', style: 'width:24em' });
    const sets = h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Server'), server, h('span', { class: 'dim' }, 'Token file on this box'), tok,
      h('span', { class: 'chip ' + (cfg.token_ok ? 'ok' : 'crit') }, cfg.token_ok ? 'token readable' : cfg.token_file_set ? 'token unreadable' : 'no token file'),
      h('button', { onclick: async () => { const b = { server: server.value }; if (tok.value) b.token_file = tok.value; const r = await safe(jsend('/api/v1/plex', b, 'PUT'), 'Save'); if (r) { toast('Saved'); render(); } } }, 'Save'));
    const pls = h('select', {}), plmode = h('select', {}, ['sequential', 'shuffle'].map(m => h('option', { value: m }, m)));
    const fillPl = h('select', {}, h('option', { value: '' }, 'every Plex item'));
    if (cfg.token_ok) J('/api/v1/plex/playlists').then(r => r.playlists.forEach(p => { pls.append(h('option', { value: p.title }, `${p.title} (${p.tracks} tracks${p.smart ? ', smart' : ''}${p.in_library ? ', imported' : ''})`)); fillPl.append(h('option', { value: p.title }, p.title)); })).catch(e => toast('Plex: ' + e.message, true));
    const imp = (dry) => () => pls.value ? start('/api/v1/plex/import', { playlist: pls.value, mode: plmode.value, dry_run: dry }, dry ? 'Preview' : 'Import') : toast('Choose a playlist', true);
    const fill = (dry) => () => start('/api/v1/plex/fill-art', fillPl.value ? { playlist: fillPl.value, dry_run: dry } : { all: true, dry_run: dry }, dry ? 'Preview' : 'Fill');
    body.replaceChildren(sets,
      h('h3', { style: 'margin-top:14px' }, 'Import a Plex playlist'), h('div', { class: 'row' }, pls, h('span', { class: 'dim' }, 'new playlist plays'), plmode, h('button', { onclick: imp(true) }, 'Preview'), h('button', { class: 'primary', onclick: imp(false) }, 'Import'),
        h('span', { class: 'dim' }, 'Read-only on Plex. Running it again adds nothing already here.')),
      h('h3', { style: 'margin-top:14px' }, 'Fill missing artwork from Plex'), h('div', { class: 'row' }, h('span', { class: 'dim' }, 'For'), fillPl, h('button', { onclick: fill(true) }, 'Preview'), h('button', { class: 'primary', onclick: fill(false) }, 'Fill missing artwork'),
        h('span', { class: 'dim' }, 'Only items without a picture; a picture from the file is never replaced.')), summary, log);
  };
  open.addEventListener('click', async () => { shown = !shown; if (shown) { card.append(body); await render(); } else body.remove(); });
  card.append(h('div', { class: 'row' }, open, h('span', { class: 'dim' }, st && st.token_ok ? 'Import playlists and fill missing artwork from the Plex server.' : 'Set the Plex server and token file to import playlists and fill missing artwork.')));
  return card;
}

// ================= Item editor =================
/** Release an item from quarantine in the library and on every station. */
async function releaseItem(id) {
  const r = await safe(jsend(`/api/v1/items/${id}/release`, {}), 'Release');
  if (!r) return false;
  const bad = Object.entries(r.stations || {}).filter(([, v]) => v !== 'released');
  toast(bad.length ? 'Released in the library; ' + bad.map(([k, v]) => `station ${k}: ${v}`).join(', ') : 'Released', bad.length > 0);
  return true;
}
routes.item = async ([id]) => {
  const it = await J('/api/v1/items/' + id);
  const w = await J(`/api/v1/items/${id}/waveform`).catch(() => null);
  const root = h('div');
  root.append(h('div', { class: 'row' }, h('a', { href: '#/library' }, 'Library'), h('span', { class: 'dim' }, '/'), h('h1', { style: 'margin:0' }, it.title, helpLink('item'))));
  const meta = h('div', { class: 'card', style: 'margin:14px 0' });
  const f = {};
  const field = (k, label, type = 'text') => { f[k] = h('input', { type, value: it[k] ?? '', style: 'width:100%' }); return [h('label', {}, label), f[k]]; };
  f.category = h('select', {}); J('/api/v1/categories').then(r => r.categories.forEach(c => f.category.append(h('option', { value: c.name, selected: c.name === it.category }, c.name))));
  f.tags = h('input', { value: (it.tags || []).join(', '), style: 'width:100%' });
  f.enabled = h('input', { type: 'checkbox', checked: it.enabled }); f.silence_ok = h('input', { type: 'checkbox', checked: it.silence_ok });
  meta.append(h('div', { class: 'row' }, artImg(it.art_url ? `/api/v1/items/${id}/art?size=600` : null, 'thumb big'),
    h('div', { class: 'form grow' }, ...field('title', 'Title'), ...field('artist', 'Artist'), ...field('album', 'Album'), ...field('disc_no', 'Disc', 'number'), ...field('track_no', 'Track', 'number'),
      h('label', {}, 'Category'), f.category, h('label', {}, 'Tags'), f.tags, h('label', {}, 'Enabled'), f.enabled, h('label', {}, 'Quiet piece'), h('div', {}, f.silence_ok, ' exempt from the silence detector'))),
    h('div', { class: 'row', style: 'margin-top:10px' }, h('button', { class: 'primary', onclick: async () => {
      const num = v => v === '' ? null : +v;
      const r = await safe(jsend(`/api/v1/items/${id}`, { title: f.title.value, artist: f.artist.value, album: f.album.value, disc_no: num(f.disc_no.value), track_no: num(f.track_no.value), category: f.category.value,
        tags: f.tags.value.split(',').map(s => s.trim()).filter(Boolean), enabled: f.enabled.checked, silence_ok: f.silence_ok.checked }, 'PATCH'), 'Save'); if (r) toast('Saved');
    } }, 'Save details'), h('label', { class: 'dim' }, 'Replace artwork ', h('input', { type: 'file', accept: 'image/png,image/jpeg', onchange: async e => {
      const file = e.target.files[0]; if (!file) return;
      const r = await fetch(`/api/v1/items/${id}/art`, { method: 'PUT', headers: { 'Content-Type': file.type }, body: file });
      toast(r.ok ? 'Artwork replaced' : 'Artwork refused', !r.ok); if (r.ok) route();
    } })), h('button', { class: 'danger', onclick: async () => { if (confirm('Delete this item?')) { const r = await safe(J('/api/v1/items/' + id, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } }), 'Delete'); if (r !== null) location.hash = '#/library'; } } }, 'Delete')));
  if (it.quarantined) root.append(h('div', { class: 'notice bad', style: 'margin-top:14px' }, h('div', {}, h('b', {}, 'Quarantined'), ': ' + (it.quarantine_reason || '')),
    h('div', { class: 'dim' }, 'The stations never pick it. Release it to make it playable again (the error count restarts at 0).'),
    h('div', { class: 'row', style: 'margin-top:8px' }, h('button', { class: 'primary', onclick: () => releaseItem(id).then(ok => ok && route()) }, 'Release from quarantine'))));
  root.append(meta);

  // ---- the waveform with markers ----
  const eff = it.effective, ov = it.overrides || {}, auto = it.analysis?.auto || {};
  // The station's automatic fade for a file that cuts its own fade short (Settings: truncated_fade, truncated_fade_s).
  let autoFade = 0;
  if (eff.flags.includes('truncated_fade')) {
    const st = await J('/api/v1/settings').then(d => (d.stations.find(x => x.id === S.station) || d.stations[0])?.effective).catch(() => null);
    autoFade = st ? (st.truncated_fade === false ? 0 : +(st.truncated_fade_s ?? 2)) : 2;
  }
  const marks = { cue_in: eff.cue_in, attack: eff.attack, intro: eff.intro, segue: eff.segue, fade_out: eff.fade_out, cue_out: eff.cue_out };
  const COLORS = { cue_in: '#3ecf8e', attack: '#5aa9ff', intro: '#b18cff', segue: '#f5a623', fade_out: '#ff8a5c', cue_out: '#ff5c5c' };
  const card = h('div', { class: 'card' }, h('h3', {}, 'Waveform and markers', helpLink('item', 'markers')));
  const cv = h('canvas', { class: 'wave', width: 1400, height: 440 });
  card.append(cv, h('div', { class: 'legend', style: 'margin-top:8px' }, Object.entries(COLORS).map(([k, c]) => h('span', {}, h('i', { style: 'background:' + c }), k.replace('_', '-'))), h('span', {}, h('i', { style: 'background:#5a6478' }), 'K200 loudness'), h('span', {}, h('i', { style: 'background:#fff4' }), 'auto values (ghosts)')));
  const dur_ms = w?.file_ms || it.duration_ms;
  const X = ms => ms / dur_ms * cv.width;
  function draw() {
    const c = cv.getContext('2d'), W = cv.width, H = cv.height; c.clearRect(0, 0, W, H); c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H);
    if (w) {
      const n = w.min.length, mid = H * 0.42;
      for (let i = 0; i < n; i++) {
        const x = X(i * w.hop_ms), lo = w.min[i] / 127, hi = w.max[i] / 127, t = i * w.hop_ms;
        c.fillStyle = (t < marks.cue_in || t > marks.cue_out) ? '#2a3140' : '#4a7fb8';
        c.fillRect(x, mid - hi * mid, Math.max(1, W / n), Math.max(1, (hi - lo) * mid));
      }
      // The K200 loudness curve and the threshold line (the profile's segue threshold on the file loudness).
      c.strokeStyle = '#5a6478'; c.lineWidth = 2; c.beginPath();
      const top = H * 0.86, span = H * 0.14 + 0;
      w.k200_db.forEach((db, i) => { const y = H - 6 - Math.max(0, Math.min(1, (db + 60) / 60)) * (H * 0.30); i ? c.lineTo(X(i * w.k200_hop_ms), y) : c.moveTo(X(i * w.k200_hop_ms), y); });
      c.stroke();
      const seg = w.profile_params?.segue_lu;
      if (seg != null) { const y = H - 6 - Math.max(0, Math.min(1, (w.l_int + seg + 60) / 60)) * (H * 0.30); c.strokeStyle = '#f5a62388'; c.setLineDash([6, 6]); c.beginPath(); c.moveTo(0, y); c.lineTo(W, y); c.stroke(); c.setLineDash([]); c.fillStyle = '#f5a623'; c.font = '18px sans-serif'; c.fillText('segue threshold ' + (w.l_int + seg).toFixed(1) + ' LU', 8, y - 6); }
    }
    for (const [k, v] of Object.entries(auto)) { const key = k; if (v != null && COLORS[key]) { c.fillStyle = '#fff3'; c.fillRect(X(v) - 1, 0, 2, H); } }
    // The fade-out: the gain falling from its start to the cue-out. Without one, the automatic fade of an item whose
    // file cuts its own fade short (dashed).
    const fadeLine = (from, to, dashed, label) => { c.strokeStyle = COLORS.fade_out; c.lineWidth = 3; c.setLineDash(dashed ? [8, 6] : []); c.beginPath(); c.moveTo(X(from), 34); c.lineTo(X(to), H - 8); c.stroke(); c.setLineDash([]); if (label) { c.font = '18px sans-serif'; const tw = c.measureText(label).width, lx = Math.max(4, X(from) - tw - 12); c.fillStyle = '#0b0e13'; c.fillRect(lx - 4, H - 34, tw + 8, 26); c.fillStyle = COLORS.fade_out; c.fillText(label, lx, H - 15); } };
    if (marks.fade_out != null && marks.cue_out > marks.fade_out) fadeLine(marks.fade_out, marks.cue_out, false, null);
    else if (marks.fade_out == null && autoFade) fadeLine(Math.max(marks.cue_in, marks.cue_out - autoFade * 1000), marks.cue_out, true, `automatic fade ${autoFade} s`);
    for (const [k, v] of Object.entries(marks)) if (v != null) { c.fillStyle = COLORS[k]; c.fillRect(X(v) - 1.5, 0, 3, H); c.font = 'bold 20px sans-serif'; c.fillText(k.replace('_', '-'), Math.min(W - 110, X(v) + 6), 22 + Object.keys(marks).indexOf(k) * 22); }
  }
  let drag = null;
  cv.addEventListener('mousedown', e => {
    const r = cv.getBoundingClientRect(), ms = (e.clientX - r.left) / r.width * dur_ms; let best = null, bd = 1e12;
    for (const [k, v] of Object.entries(marks)) if (v != null && Math.abs(v - ms) < bd) { bd = Math.abs(v - ms); best = k; }
    if (best && bd / dur_ms * r.width < 14) drag = best;
  });
  addEventListener('mouseup', () => { if (drag) { drag = null; syncFields(); } });
  cv.addEventListener('mousemove', e => { if (!drag) return; const r = cv.getBoundingClientRect(); setMark(drag, Math.max(0, Math.min(dur_ms, Math.round((e.clientX - r.left) / r.width * dur_ms)))); syncFields(); draw(); });
  root.append(card);

  // ---- numeric fields, quiet stretch, flags ----
  const nf = {}, form = h('div', { class: 'form', style: 'margin-top:10px' });
  // The fade-out is typed in seconds before the end (the default) or in ms from the start of the file.
  const fo = { mode: 'end', note: h('div', { class: 'fo-note dim' }), info: h('span', { class: 'fo-info dim' }) };
  const foLow = () => Math.max(marks.cue_in, marks.attack, marks.segue - 30000);
  const sec = ms => (ms / 1000).toFixed(3).replace(/\.?0+$/, '');
  const clock = ms => `${Math.floor(ms / 60000)}:${((ms % 60000) / 1000).toFixed(1).padStart(4, '0')}`;
  /** Set a marker; a fade-out typed from the end moves with the cue-out. */
  function setMark(k, v) {
    if (k === 'cue_out' && fo.mode === 'end' && marks.fade_out != null && marks.cue_out != null && v != null) marks.fade_out += v - marks.cue_out;
    marks[k] = v;
  }
  /** The fade-out from the field: from the end, empty or 0 is none; kept between the attack (or 30 s before the segue) and the cue-out. */
  function readFade() {
    const raw = nf.fade_out.value.trim();
    fo.note.textContent = '';
    if (raw === '') { marks.fade_out = null; return; }
    const v = +raw;
    if (!isFinite(v)) { fo.note.textContent = 'Not a number.'; return; }
    if (fo.mode === 'end') {
      if (v <= 0) { marks.fade_out = null; if (v < 0) fo.note.textContent = 'A fade-out before the end is a positive number of seconds. Empty or 0 is no fade-out.'; return; }
      marks.fade_out = marks.cue_out - Math.round(v * 1000);
    } else marks.fade_out = Math.round(v);
    const lo = foLow(), hi = marks.cue_out;
    if (marks.fade_out < lo || marks.fade_out > hi) {
      const to = Math.min(hi, Math.max(lo, marks.fade_out));
      fo.note.textContent = marks.fade_out > hi ? `The fade-out cannot start after the cue-out: moved to the cue-out.`
        : `The fade-out cannot start before the attack, or more than 30 s before the segue. Moved to ${clock(to)}, ${sec(hi - to)} s before the end.`;
      marks.fade_out = to;
    }
  }
  function showFade() {
    const f = marks.fade_out;
    nf.fade_out.value = f == null ? '' : fo.mode === 'end' ? sec(marks.cue_out - f) : f;
    fo.info.textContent = f == null ? (autoFade ? `none (the automatic ${autoFade} s fade applies: truncated_fade)` : 'none')
      : fo.mode === 'end' ? `starts at ${clock(f)} (${f} ms), fades ${sec(marks.cue_out - f)} s to the cue-out`
        : `${sec(marks.cue_out - f)} s before the end, fades ${sec(marks.cue_out - f)} s to the cue-out`;
  }
  for (const k of Object.keys(COLORS)) {
    if (k === 'fade_out') {
      nf[k] = h('input', { type: 'number', step: '0.1', min: 0, class: 'fo-value', placeholder: 'none', onchange: () => { readFade(); showFade(); draw(); } });
      fo.toggle = h('select', { class: 'fo-mode', title: 'How the fade-out is typed' }, h('option', { value: 'end' }, 's before the end'), h('option', { value: 'start' }, 'ms from the start'));
      fo.toggle.addEventListener('change', () => { fo.mode = fo.toggle.value; nf[k].step = fo.mode === 'end' ? '0.1' : '1'; fo.note.textContent = ''; showFade(); });
      const autoTxt = auto[k] != null ? 'auto ' + auto[k] : '';
      form.append(h('label', {}, 'fade-out'), h('div', {}, h('div', { class: 'row fo-row' }, nf[k], fo.toggle, fo.info, h('span', { class: 'dim' }, autoTxt + (ov[k] != null ? '  (overridden)' : '')), ov[k] != null ? h('button', { onclick: () => setOv({ [k]: null }) }, 'Back to auto') : null), fo.note));
      continue;
    }
    nf[k] = h('input', { type: 'number', value: marks[k] ?? '', placeholder: 'none', onchange: () => { setMark(k, nf[k].value === '' ? null : +nf[k].value); showFade(); draw(); } });
    form.append(h('label', {}, k.replace('_', '-') + ' (ms)'), h('div', { class: 'row' }, nf[k], h('span', { class: 'dim' }, 'auto ' + (auto[k] ?? '-') + (ov[k] != null ? '  (overridden)' : '')), ov[k] != null ? h('button', { onclick: () => setOv({ [k]: null }) }, 'Back to auto') : null));
  }
  function syncFields() { for (const k of Object.keys(nf)) if (k !== 'fade_out') nf[k].value = marks[k] ?? ''; showFade(); }
  const trim = h('input', { type: 'number', step: '0.1', value: ov.gain_trim_db ?? '', placeholder: '0' });
  form.append(h('label', {}, 'Gain trim (dB)'), trim);
  async function setOv(body) { const r = await safe(jsend(`/api/v1/items/${id}/overrides`, body, 'PUT'), 'Overrides'); if (r) { if (r.notes?.length) toast('Saved. ' + r.notes.join(' '), true); else toast('Markers saved'); route(); } }
  const quiet = h('div', { class: 'notice' }, `Longest quiet stretch: ${(eff.longest_quiet_ms / 1000).toFixed(1)} s inside the cue window, ${(eff.longest_quiet_file_ms / 1000).toFixed(1)} s over the whole file (below -50 dBFS). The silence detector waits at least this long plus 5 s before it skips this item.`);
  card.append(form, h('div', { class: 'row', style: 'margin-top:10px' },
    h('button', { class: 'primary', onclick: () => { const body = Object.fromEntries(Object.keys(COLORS).filter(k => (marks[k] ?? null) !== (eff[k] ?? null)).map(k => [k, marks[k] ?? null]));
      if ('fade_out' in body && fo.mode === 'end') { delete body.fade_out; body.fade_out_before_end_ms = marks.fade_out == null ? null : marks.cue_out - marks.fade_out; } if ((trim.value === '' ? null : +trim.value) !== (ov.gain_trim_db ?? null)) body.gain_trim_db = trim.value === '' ? null : +trim.value; if (!Object.keys(body).length) return toast('Nothing changed'); setOv(body); } }, 'Save markers'),
    h('button', { class: 'danger', onclick: async () => { if (confirm('Clear every override and go back to the automatic values?')) { const r = await safe(J(`/api/v1/items/${id}/overrides`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); if (r) route(); } } }, 'Everything back to auto'),
    (eff.flags.includes('long_intro')) ? h('button', { onclick: () => { marks.cue_in = marks.attack; syncFields(); draw(); toast('Cue-in moved to the attack: save to keep it'); } }, 'Cue in at attack') : null),
    quiet, h('div', {}, flagChips(eff.flags), it.analysis ? h('span', { class: 'dim' }, `  L ${it.analysis.l_int.toFixed(1)} LUFS, LRA ${it.analysis.lra.toFixed(1)}, true peak ${it.analysis.true_peak.toFixed(1)} dBTP`) : null));
  syncFields(); draw();

  // ---- audition ----
  const audio = h('audio', { controls: true, preload: 'none', src: `/api/v1/items/${id}/audio`, style: 'width:100%' });
  const seek = ms => { audio.currentTime = Math.max(0, ms / 1000); audio.play(); };
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Audition', helpLink('item', 'audition')), audio, h('div', { class: 'row', style: 'margin-top:8px' },
    h('button', { onclick: () => seek(marks.cue_in) }, 'From cue-in'), h('button', { onclick: () => seek(marks.attack) }, 'From attack'), marks.intro != null ? h('button', { onclick: () => seek(marks.intro) }, 'From intro end') : null,
    h('button', { onclick: () => seek(marks.segue - 8000) }, 'Segue minus 8 s'), h('button', { onclick: () => seek(marks.cue_out - 5000) }, 'Cue-out minus 5 s'))));

  // ---- the item's own transition settings (2.6, resolution step 3) ----
  const presets = (await J('/api/v1/transition-presets')).items;
  const curT = (ov.params && ov.params.transition) || {};
  const stInfo = S.station ? await J(`/api/v1/stations/${S.station}/transition-preset`).catch(() => null) : null;
  const itemCat = (await J('/api/v1/categories').catch(() => ({ categories: [] }))).categories.find(c => c.name === it.category);
  const inheritText = itemCat && itemCat.transition_preset ? `category ${itemCat.name}: ${itemCat.transition_preset}` : stInfo ? `station: ${stInfo.preset}` : 'category and station';
  const ipre = h('select', {}, h('option', { value: '' }, `inherit (${inheritText})`), presets.map(p => h('option', { value: p.name, selected: curT.preset === p.name }, p.name)));
  const ipatch = h('textarea', { rows: 2, style: 'width:100%', placeholder: 'optional JSON patch for this item, e.g. {"tail_max_s": 6}' }, curT.patch ? JSON.stringify(curT.patch) : '');
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'This item\'s own transition', helpLink('item', 'item-transition')),
    h('div', { class: 'dim' }, 'Applies whenever this item is on either side of a boundary, above the category and below a queue entry\'s own override.'),
    h('div', { class: 'row', style: 'margin-top:8px' }, h('span', { class: 'dim' }, 'Preset'), ipre), ipatch,
    h('div', { class: 'row', style: 'margin-top:8px' }, h('button', { class: 'primary', onclick: async () => {
      let patch = null; if (ipatch.value.trim()) { try { patch = JSON.parse(ipatch.value); } catch (e) { return toast('The patch is not JSON: ' + e.message, true); } }
      const params = { ...(ov.params || {}) }; const t = {}; if (ipre.value) t.preset = ipre.value; if (patch) t.patch = patch;
      if (Object.keys(t).length) params.transition = t; else delete params.transition;
      setOv({ params: Object.keys(params).length ? params : null });
    } }, 'Save transition settings'))));

  // ---- transition preview: pick the pair, pick a preset, hear it, see where it joins ----
  const psel = h('select', {}, h('option', { value: 'station' }, 'as the station plays it'), presets.map(p => h('option', { value: p.name }, p.name)));
  const dir = h('select', {}, h('option', { value: 'after' }, 'this item, then the other'), h('option', { value: 'before' }, 'the other, then this item'));
  const other = h('input', { type: 'number', placeholder: 'item id', style: 'width:7em' });
  const find = h('input', { placeholder: 'search the library', style: 'width:14em' }), found = h('select', {});
  found.addEventListener('change', () => { other.value = found.value; });
  let tm = null; find.addEventListener('input', () => { clearTimeout(tm); tm = setTimeout(async () => {
    const r = await safe(J('/api/v1/items?per_page=15&q=' + encodeURIComponent(find.value))); if (!r) return;
    found.replaceChildren(...r.items.map(i => h('option', { value: i.id }, `${i.id}  ${i.title} - ${i.artist || ''}`)));
    if (r.items.length) { other.value = r.items[0].id; }
  }, 250); });
  const plsel = h('select', {}, h('option', { value: '' }, 'neighbor from a playlist'));
  const pnb = h('div', { class: 'row' });
  J('/api/v1/playlists').then(r => r.playlists.forEach(p => plsel.append(h('option', { value: p.id }, p.name))));
  plsel.addEventListener('change', async () => {
    pnb.replaceChildren(); if (!plsel.value) return;
    const p = await safe(J('/api/v1/playlists/' + plsel.value)); if (!p) return;
    const ids = p.entries.filter(e => e.type === 'item').map(e => e.item_id), at = ids.indexOf(+id);
    if (at < 0) return pnb.append(h('span', { class: 'dim' }, 'This item is not in that playlist; pick from the list above.'));
    const mk = (label, j, d) => ids[j] != null ? h('button', { onclick: () => { other.value = ids[j]; dir.value = d; } }, label + ' (item ' + ids[j] + ')') : null;
    pnb.append(mk('Previous in playlist', at - 1, 'before'), mk('Next in playlist', at + 1, 'after'));
  });
  const patch = h('textarea', { rows: 2, placeholder: 'optional JSON patch for this one boundary, e.g. {"overlap_max_s": 4}', style: 'width:100%' });
  const pv = h('div'), pa = h('audio', { controls: true, style: 'width:100%;margin-top:8px' });
  const wa = h('canvas', { class: 'wave', width: 1400, height: 150, style: 'height:90px;margin-top:8px' }), wb = h('canvas', { class: 'wave', width: 1400, height: 150, style: 'height:90px;margin-top:4px' });
  const drawSide = async (cv, side) => {
    const c = cv.getContext('2d'), W = cv.width, H = cv.height; c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H);
    const wf = await J(`/api/v1/items/${side.item}/waveform`).catch(() => null); if (!wf || !side.voice) return;
    const X = ms => ms / wf.file_ms * W, v = side.voice, rate = meta_.rate;
    const ms = f => v.file_start_ms + (f - v.start_frame) * 1000 / rate;
    const n = wf.min.length, mid = H / 2;
    for (let i = 0; i < n; i++) { c.fillStyle = '#35507a'; c.fillRect(X(i * wf.hop_ms), mid - wf.max[i] / 127 * mid * .9, Math.max(1, W / n), Math.max(1, (wf.max[i] - wf.min[i]) / 127 * mid * .9)); }
    // the part the preview renders
    const lo = ms(meta_.crop_from), hi = ms(meta_.crop_from + meta_.frames); c.fillStyle = '#ffffff12'; c.fillRect(X(lo), 0, X(hi) - X(lo), H);
    // the overlap: from where B starts to where A ends
    const aV = meta_.a.voice, bV = meta_.b.voice, s_b = bV.start_frame, a_end = aV.ended_frame ?? aV.cue_out_frame;
    c.font = '18px sans-serif';
    if (side === meta_.a) { c.fillStyle = '#f5a62333'; c.fillRect(X(ms(s_b)), 0, X(ms(a_end)) - X(ms(s_b)), H); c.fillStyle = '#f5a623'; c.fillRect(X(ms(s_b)) - 1, 0, 2, H); c.fillText('B starts', X(ms(s_b)) + 6, 20); }
    else { c.fillStyle = '#f5a62333'; c.fillRect(X(v.file_start_ms), 0, X(ms(a_end)) - X(v.file_start_ms), H); c.fillStyle = '#f5a623'; c.fillRect(X(v.file_start_ms) - 1, 0, 2, H); c.fillText('starts here', X(v.file_start_ms) + 6, 20); }
    // the fades and ramps sent for this voice, as the gain they draw
    for (const r of v.ramps) { c.strokeStyle = '#ff8a5c'; c.lineWidth = 2; c.beginPath(); c.moveTo(X(ms(r.start)), H - 8 - r.from * (H - 16)); c.lineTo(X(ms(r.end)), H - 8 - r.to * (H - 16)); c.stroke(); }
    if (v.stop) { c.strokeStyle = '#ff5c5c'; c.lineWidth = 3; c.beginPath(); c.moveTo(X(ms(v.stop.at)), 0); c.lineTo(X(ms(v.stop.at)), H); c.stroke(); c.fillStyle = '#ff5c5c'; c.fillText('stop (' + v.stop.curve + ' ' + Math.round(v.stop.frames * 1000 / rate) + ' ms)', X(ms(v.stop.at)) + 6, 40); }
    for (const [k, col] of [['cue_in', '#3ecf8e'], ['attack', '#5aa9ff'], ['segue', '#f5a623'], ['cue_out', '#ff5c5c']]) { const m = side.markers[k]; if (m != null) { c.fillStyle = col + '99'; c.fillRect(X(m) - 1, H - 14, 2, 14); } }
    c.fillStyle = '#dde'; c.font = 'bold 20px sans-serif'; c.fillText(side.title, 6, H - 18);
  };
  let meta_ = null;
  async function preview(presetName) {
    const o = +other.value; if (!o) return toast('Give the other item (search, or the id)', true);
    const [fr, to] = dir.value === 'after' ? [id, o] : [o, id];
    const p = new URLSearchParams({ from: fr, to, preset: presetName, params: patch.value.trim(), pre_s: 10, post_s: 10 });
    pv.replaceChildren(h('span', { class: 'dim' }, 'Rendering')); wa.getContext('2d').clearRect(0, 0, 9999, 9999); wb.getContext('2d').clearRect(0, 0, 9999, 9999);
    meta_ = await safe(J('/api/v1/preview/transition?format=json&' + p), 'Preview'); if (!meta_) return pv.replaceChildren();
    const b = meta_.boundary || {};
    pv.replaceChildren(h('div', { class: 'mono dim' }, `${meta_.a.title}  ->  ${meta_.b.title}   preset ${meta_.preset} (${b.preset_a || ''}${b.preset_b && b.preset_b !== b.preset_a ? ' / ' + b.preset_b : ''}), ${b.kind || ''}, overlap ${(b.overlap_frames / meta_.rate).toFixed(2)} s, ${(meta_.frames / meta_.rate).toFixed(1)} s rendered`));
    pa.src = '/api/v1/preview/transition?' + p; pa.play().catch(() => { });
    await Promise.all([drawSide(wa, meta_.a), drawSide(wb, meta_.b)]);
  }
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Preview transition', helpLink('item', 'preview-transition')),
    h('div', { class: 'row' }, dir, other, find, found), h('div', { class: 'row', style: 'margin-top:6px' }, plsel, pnb),
    h('div', { class: 'row', style: 'margin-top:8px' }, h('span', { class: 'dim' }, 'Preset'), psel, h('button', { class: 'primary', onclick: () => preview(psel.value) }, 'Preview transition'),
      h('span', { class: 'dim' }, 'or one click each:'), ['station', ...presets.map(p => p.name)].map(n => h('button', { onclick: () => { psel.value = n; preview(n); } }, n === 'station' ? 'as station' : n))),
    patch, pv, pa, h('div', { class: 'dim', style: 'margin-top:8px' }, 'Top: the first item. Bottom: the second. The lit band is what the preview renders; orange is the overlap, the orange line where the second item starts, the red line where the first is stopped, the coral lines the fades sent to each.'), wa, wb));
  return root;
};

// ================= Playlists =================
routes.playlists = async ([sel]) => {
  const root = h('div'); root.append(h('h1', {}, 'Playlists', helpLink('playlists')));
  const lists = (await J('/api/v1/playlists')).playlists;
  const left = h('div', { class: 'card' }, h('h3', {}, 'Playlists'));
  const name = h('input', { placeholder: 'New playlist name' });
  left.append(...lists.map(p => h('div', { class: 'qrow', style: 'cursor:pointer', onclick: () => location.hash = '#/playlists/' + p.id }, h('div', { class: 'meta' }, h('div', { style: sel == p.id ? 'color:var(--amber)' : '' }, p.name), h('div', { class: 'dim' }, `${p.mode}, ${p.entries} entries`)))),
    h('div', { class: 'row', style: 'margin-top:10px' }, name, h('button', { onclick: async () => { if (!name.value.trim()) return; const r = await safe(jsend('/api/v1/playlists', { name: name.value.trim() }), 'Create'); if (r) location.hash = '#/playlists/' + r.id; } }, 'Create')));
  const right = h('div');
  const leftCol = h('div', {}, left); const exc = await examplesCard(lists); if (exc) leftCol.append(exc);
  root.append(h('div', { class: 'plsplit' }, leftCol, right));
  if (sel) right.append(await playlistEditor(+sel));
  else right.append(h('div', { class: 'dim' }, 'Choose a playlist, or create one.'));
  return root;
};
async function playlistEditor(id) {
  let p = await J('/api/v1/playlists/' + id);
  const box = h('div', { style: 'display:grid;gap:14px' });
  const head = h('div', { class: 'card' }), ent = h('div', { class: 'card' });
  const nm = h('input', { value: p.name }), mode = h('select', {}, ['shuffle', 'sequential'].map(m => h('option', { value: m, selected: p.mode === m }, m)));
  const nr = h('input', { type: 'number', value: p.no_repeat_minutes, min: 0 });
  const rot = h('div', { class: 'notice' }, 'No repeat: reading the station');
  const enErr = h('div', { class: 'notice bad enableerrors', style: 'display:none;margin-top:8px' });
  const enBox = h('input', { type: 'checkbox', checked: p.enabled });
  enBox.addEventListener('change', async () => {
    const want = enBox.checked; enErr.style.display = 'none'; enErr.replaceChildren();
    try { await jsend('/api/v1/playlists/' + id, { enabled: want }, 'PATCH'); toast(want ? 'Enabled' : 'Disabled'); }
    catch (e) {
      enBox.checked = !want;
      // Say which checks block it: the validator's errors, not only the one-line refusal.
      const v = await jsend(`/api/v1/playlists/${id}/validate`, {}).catch(() => ({ problems: [] }));
      const errs = (v.problems || []).filter(x => x.severity === 'error').map(x => x.message);
      enErr.replaceChildren(h('div', {}, 'Not enabled: the playlist has errors that block it.'), ...(errs.length ? errs : [e.message]).map(m => h('div', { class: 'dim' }, m)));
      enErr.style.display = '';
    }
  });
  const sep = p.separation || {};
  const sepIn = ['sep_title_minutes', 'sep_artist_minutes', 'sep_album_minutes'].map(k => [k, h('input', { type: 'number', value: sep[k] ?? '', min: 0 })]);
  // Copy: a name, and optionally a start date and time that makes the copy a one-off with its own start.
  const copyName = h('input', { value: p.name + ' (copy)', style: 'min-width:16em' });
  const copyStart = h('input', { type: 'datetime-local', class: 'copy-start' });
  const copyBox = h('div', { class: 'notice copybox', style: 'display:none;margin-top:8px' },
    h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Name of the copy'), copyName, h('span', { class: 'dim' }, 'One-off start (optional)'), copyStart),
    h('div', { class: 'dim', style: 'margin-top:6px' }, 'With a start date, the copy starts on that date only and has no weekly start. Leave it empty to copy the entries and blocks as they are.'),
    h('div', { class: 'row', style: 'margin-top:8px' }, h('button', { class: 'primary', onclick: async () => {
      const body = { name: copyName.value.trim() || undefined };
      if (copyStart.value) body.start = copyStart.value.length === 16 ? copyStart.value + ':00' : copyStart.value;
      const r = await safe(jsend(`/api/v1/playlists/${id}/copy`, body), 'Copy'); if (r) location.hash = '#/playlists/' + r.id; } }, 'Make the copy'),
      h('button', { onclick: () => { copyBox.style.display = 'none'; } }, 'Cancel')));
  // a program made for one station: the others refuse it, and the Stream Deck lists it only for that station
  const stSel = h('select', { class: 'pl-station' }, h('option', { value: '' }, '(any station)'), (S.stations || []).map(s => h('option', { value: s.id, selected: p.station_id === s.id }, s.display_name || s.id)));
  if (p.station_id && !(S.stations || []).some(s => s.id === p.station_id)) stSel.append(h('option', { value: p.station_id, selected: true }, p.station_id));
  head.append(h('h3', {}, 'Playlist', helpLink('playlists', 'playlist-settings')), h('div', { class: 'form' }, h('label', {}, 'Name'), nm, h('label', {}, 'Mode'), mode, h('label', {}, 'No-repeat (minutes)'), nr, h('label', { title: 'A program for one station is refused by the others' }, 'Station'), stSel,
    ...sepIn.flatMap(([k, i]) => [h('label', {}, k.replace('sep_', 'Advanced: ').replace('_minutes', ' (min)')), i])), rot, enErr,
    h('div', { class: 'row', style: 'margin-top:10px' }, h('button', { class: 'primary', onclick: async () => {
      const separation = {}; for (const [k, i] of sepIn) if (i.value !== '') separation[k] = +i.value;
      const r = await safe(jsend('/api/v1/playlists/' + id, { name: nm.value, mode: mode.value, no_repeat_minutes: +nr.value, separation, station_id: stSel.value || null }, 'PATCH'), 'Save'); if (r) { toast('Saved'); route(); } }, }, 'Save'),
      h('label', { class: 'enabledbox', title: 'A disabled playlist has no scheduled starts. Enabling is refused while the checks report errors.' }, enBox, ' Enabled'),
      h('button', { onclick: async () => { const v = await safe(jsend(`/api/v1/playlists/${id}/validate`, {}), 'Validate'); if (v) toast(v.ok ? 'Valid' + (v.problems.length ? ` (${v.problems.length} warnings)` : '') : v.problems.map(x => x.message).join('; '), !v.ok); } }, 'Validate'),
      S.station ? h('button', { onclick: async () => { const r = await safe(jsend(stApi('transport/play'), { playlist_id: id }), 'Start'); if (r) toast('Started on ' + S.station); } }, 'Start on ' + S.station) : null,
      h('button', { onclick: () => { copyBox.style.display = copyBox.style.display === 'none' ? '' : 'none'; } }, 'Copy'),
      h('button', { class: 'danger', onclick: async () => { if (confirm('Delete this playlist?')) { await safe(J('/api/v1/playlists/' + id, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); location.hash = '#/playlists'; } } }, 'Delete')), copyBox);
  if (S.station) J(stApi('rotation?playlist_id=' + id)).then(r => { rot.textContent = rotationText(r) + (r.message ? '. ' + r.message : ''); if (r.no_repeat_reason) rot.title = r.no_repeat_reason; rot.className = 'notice' + (r.capped ? '' : ' ok'); }).catch(() => { rot.textContent = 'The station has not loaded this playlist yet.'; });
  // entries
  const [cats, plists, templates, validation] = await Promise.all([J('/api/v1/categories').then(r => r.categories).catch(() => []), J('/api/v1/playlists').then(r => r.playlists).catch(() => []), J('/api/v1/block-templates').catch(() => []), jsend(`/api/v1/playlists/${id}/validate`, {}).catch(() => ({ problems: [] }))]);
  let problems = validation.problems;
  const open = new Set();
  const rows = h('div', { class: 'entries' });
  const title = h('h3', {});
  const ctx = {
    id, cats, lists: plists, templates, open,
    get entries() { return p.entries; }, get sequential() { return p.mode === 'sequential'; },
    reload: () => reload(),
  };
  async function reload() {
    const [np, v] = await Promise.all([J('/api/v1/playlists/' + id), jsend(`/api/v1/playlists/${id}/validate`, {}).catch(() => ({ problems: [] }))]);
    p = np; problems = v.problems; draw();
  }
  // Drop a dragged row or palette block so that it lands before position `at` (the list length is the end).
  async function onDrop(data, at) {
    if (data.startsWith('palette:')) return addBlock(ctx, data.slice(8), at);
    if (!data.startsWith('row:')) return;
    const eid = +data.slice(4), ids = p.entries.map(x => x.id), from = ids.indexOf(eid);
    if (from < 0) return;
    ids.splice(from, 1); ids.splice(at > from ? at - 1 : at, 0, eid);
    if (ids.every((x, i) => x === p.entries[i].id)) return;
    await safe(jsend(`/api/v1/playlists/${id}/entries/order`, { ids }, 'PUT'), 'Order'); await reload();
  }
  const dropOn = (el, at) => {
    el.addEventListener('dragover', ev => { ev.preventDefault(); el.classList.add('dropbefore'); });
    el.addEventListener('dragleave', () => el.classList.remove('dropbefore'));
    el.addEventListener('drop', ev => { ev.preventDefault(); ev.stopPropagation(); el.classList.remove('dropbefore'); onDrop(ev.dataTransfer.getData('text/plain'), at()); });
  };
  function draw() {
    title.replaceChildren(`Entries (${p.entries.length})`, helpLink('playlists', 'entries'));
    const out = p.entries.map((e, idx) => {
      if (e.type === 'block') {
        const card = blockCard(ctx, e, idx, (problems || []).filter(x => x.block === e.block.id));
        dropOn(card, () => p.entries.findIndex(x => x.id === e.id));
        return card;
      }
      let label, sub, thumb = null;
      if (e.type === 'item') { label = e.item?.title || 'item ' + e.item_id; sub = e.item?.artist || ''; thumb = artImg(e.item?.art_url); }
      else { label = 'Album: ' + (e.album || e.album_key); sub = `${e.tracks} tracks, ${e.continuity}`; }
      const row = h('div', { class: 'qrow', draggable: true, 'data-entry': e.id }, thumb, h('div', { class: 'meta' }, h('div', {}, label), h('div', { class: 'dim' }, sub)),
        h('button', { onclick: async () => { await safe(J(`/api/v1/playlists/${id}/entries/${e.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); reload(); } }, 'x'));
      row.addEventListener('dragstart', ev => ev.dataTransfer.setData('text/plain', 'row:' + e.id));
      dropOn(row, () => p.entries.findIndex(x => x.id === e.id));
      return row;
    });
    const end = h('div', { class: 'dropend' }, p.entries.length ? 'Drop here to add at the end' : 'Drag a block from the palette, or add songs with the search below');
    dropOn(end, () => p.entries.length);
    rows.replaceChildren(...out, end);
  }
  draw();
  const q = h('input', { placeholder: 'Add items: search the library', style: 'width:100%' }), res = h('div');
  let t; q.addEventListener('input', () => { clearTimeout(t); t = setTimeout(async () => {
    if (!q.value.trim()) return res.replaceChildren();
    const r = await safe(J('/api/v1/items?per_page=8&q=' + encodeURIComponent(q.value))); if (!r) return;
    res.replaceChildren(...r.items.map(i => h('div', { class: 'qrow' }, artImg(i.art_url), h('div', { class: 'meta' }, h('div', {}, i.title), h('div', { class: 'dim' }, i.artist + (i.album ? ' - ' + i.album : ''))),
      h('button', { onclick: async () => { await safe(jsend(`/api/v1/playlists/${id}/entries`, { item_id: i.id })); reload(); } }, 'Add track'),
      i.album_key ? h('button', { onclick: async () => { await safe(jsend(`/api/v1/playlists/${id}/entries`, { album_key: i.album_key, continuity: 'gapless' })); reload(); } }, 'Add album') : null)));
  }, 200); });
  const sbOn = h('input', { type: 'checkbox', checked: p.station_blocks });
  sbOn.addEventListener('change', async () => { await safe(jsend('/api/v1/playlists/' + id, { station_blocks: sbOn.checked }, 'PATCH'), 'Save'); toast(sbOn.checked ? 'Station blocks apply while this plays' : 'Station blocks are off while this plays'); });
  // The palette: each chip is dragged into the list or clicked to add the block at the end.
  const palette = h('div', { class: 'palette' }, PALETTE.map(([kind, label, tip]) => {
    const chip = h('span', { class: 'pchip', draggable: true, 'data-kind': kind, role: 'button', tabindex: 0, title: tip, onclick: () => addBlock(ctx, kind, null) }, label);
    chip.addEventListener('dragstart', ev => ev.dataTransfer.setData('text/plain', 'palette:' + kind));
    return chip;
  }));
  const dry = dryRunCard(id);
  ent.append(title, h('div', { class: 'dim', style: 'margin-bottom:6px' }, 'Blocks: drag one into the list below, or click it to add it at the end.', helpLink('playlists', 'the-block-palette')), palette, rows, q, res, insertForm(id),
    h('div', { style: 'margin-top:14px' }, h('h3', { style: 'color:var(--dim);font-size:13px;text-transform:uppercase' }, 'Add a timed block', helpLink('timed-blocks')), await timedBlockForm(id, null, cats, plists)),
    h('label', { class: 'row', style: 'margin-top:10px' }, sbOn, 'The station blocks (the hourly ID, station-wide liners) apply while this playlist runs'));
  box.append(head, ent, dry);
  return box;
}

// ---- the block palette and the block editor (milestone M5) ----
// [kind, label, tip]. Wait and Loop run in sequence only; Start is the playlist's one start block.
const PALETTE = [
  ['start', 'Start', 'When the program is expected, and how it starts'],
  ['play', 'Play', 'Play a playlist, a category or one item'],
  ['wait', 'Wait', 'Hold the program until a time, a length of time or the operator'],
  ['loop', 'Loop', 'Go back to an earlier row'],
  ['fade_stop', 'Fade out', 'Fade the whole output to silence and stop'],
  ['stop', 'Stop', 'Stop the station'],
  ['end', 'End', 'End this playlist'],
  ['start_playlist', 'Start playlist', 'Switch the program to another playlist'],
  ['insert', 'Liner or ID', 'Put one item from a category into the queue'],
  ['osc_out', 'OSC message', 'Send one OSC message (a mixing console, for example)'],
  ['midi_out', 'MIDI message', 'Send one MIDI message'],
  ['http_request', 'Web request', 'Send one HTTP request'],
  ['set_gain', 'Gain', 'Lower the master gain'],
  ['set_mute', 'Mute', 'Mute or unmute the output'],
];
const BADGE = { start: 'START', play: 'PLAY', wait: 'WAIT', loop: 'LOOP', fade_stop: 'FADE', stop: 'STOP', end: 'END', start_playlist: 'SWITCH', insert: 'ID', insert_every: 'RULE', osc_out: 'OSC', midi_out: 'MIDI', http_request: 'HTTP', set_gain: 'GAIN', set_mute: 'MUTE' };
const BLOCK_NAME = Object.fromEntries(PALETTE.map(([k, l]) => [k, l]));
const SEQ_ONLY = ['wait', 'loop'];
const BOTH = ['play', 'fade_stop', 'stop', 'end', 'start_playlist', 'insert', 'osc_out', 'midi_out', 'http_request', 'set_gain', 'set_mute'];

const num = (v, o = {}) => h('input', { type: 'number', value: v ?? '', style: 'width:6.5em', ...o });
const txt = (v, o = {}) => h('input', { type: 'text', value: v ?? '', ...o });
const line = (label, ...c) => h('div', { class: 'bline' }, h('span', { class: 'dim blabel' }, label), h('span', { class: 'row' }, c));
function selOf(opts, cur, o = {}) { const s = h('select', o, opts.map(([v, l]) => h('option', { value: v }, l))); if (cur != null) s.value = String(cur); return s; }
const need = (v, what) => { if (v === '' || v == null || Number.isNaN(+v)) throw new Error(what + ' needs a number'); return +v; };

/** The defaults of a new block of this kind (they must read, or the library refuses the block). */
function blockDefaults(kind, ctx) {
  const others = ctx.lists.filter(l => l.id !== ctx.id);
  const cat = (ctx.cats.find(c => c.name === 'id') || ctx.cats[0]);
  const rel0 = { kind: 'relative', offset_ms: 15 * 60000 };
  const tmpl = k => (ctx.templates.find(t => t.kind === k) || {}).params;
  const src = () => others[0] ? { playlist_id: others[0].id } : cat ? { category_id: cat.id } : null;
  let params, trigger = null;
  switch (kind) {
    case 'start': params = { schedule: [{ cron: '0 18 * * 1' }], anchor: 'manual', arm_window_min: 60, fallback_start: { enabled: false } }; trigger = 'first'; break;
    case 'play': { const s = src(); if (!s) throw new Error('Make another playlist or a category first: a Play block needs something to play'); params = { source: s, until: 'next' }; break; }
    case 'wait': params = { until: { duration_s: 60 } }; break;
    case 'loop': params = { times: 2 }; break;
    case 'fade_stop': params = { seconds: 7, curve: 'cosine' }; break;
    case 'stop': params = { how: 'now', fade_ms: 2000 }; break;
    case 'end': params = {}; break;
    case 'start_playlist': if (!others[0]) throw new Error('Make another playlist first'); params = { playlist_id: others[0].id, how: 'segue' }; break;
    case 'insert': if (!cat) throw new Error('There is no category to take the item from'); params = { source: { category_id: cat.id } }; break;
    case 'osc_out': params = JSON.parse(JSON.stringify(tmpl('osc_out') || { host: '127.0.0.1', port: 2223, address: '/ch/1/mute', args: [{ type: 'i', value: 1 }] })); break;
    case 'midi_out': params = JSON.parse(JSON.stringify(tmpl('midi_out') || { port: 'hw:1,0', message: { type: 'cc', channel: 1, data1: 7, data2: 100 } })); break;
    case 'http_request': params = JSON.parse(JSON.stringify(tmpl('http_request') || { method: 'POST', url: 'http://127.0.0.1:8080/hook', headers: {}, body: '', timeout_s: 5 })); break;
    case 'set_gain': params = { gain_db: -6, ramp_ms: 0 }; break;
    case 'set_mute': params = { on: true, ramp_ms: 0 }; break;
    default: throw new Error('unknown block ' + kind);
  }
  if (!trigger) trigger = SEQ_ONLY.includes(kind) || ctx.sequential ? 'sequence' : 'timed';
  if (trigger === 'timed') params.when = rel0;
  return { name: BLOCK_NAME[kind] || kind, trigger, params };
}
/** Create a block with its defaults, put it before position `at` (null: at the end), open its form. */
async function addBlock(ctx, kind, at) {
  if (kind === 'start' && ctx.entries.some(e => e.type === 'block' && e.block.kind === 'start')) return toast('This playlist already has a Start block', true);
  if (SEQ_ONLY.includes(kind) && !ctx.sequential) return toast('Wait and Loop run in sequence, and a shuffle playlist has no order. Make the playlist sequential first.', true);
  let def; try { def = blockDefaults(kind, ctx); } catch (e) { return toast(e.message, true); }
  const before = ctx.entries.map(e => e.id);
  const r = await safe(jsend(`/api/v1/playlists/${ctx.id}/entries`, { block: { kind, name: def.name, trigger: def.trigger, params: def.params } }), 'Add');
  if (!r) return;
  const made = r.entries.find(e => !before.includes(e.id));
  if (made) {
    ctx.open.add(made.id);
    if (at != null && at < before.length) {
      const ids = before.slice(); ids.splice(at, 0, made.id);
      await safe(jsend(`/api/v1/playlists/${ctx.id}/entries/order`, { ids }, 'PUT'), 'Order');
    }
  }
  await ctx.reload();
}

const srcText = (s, ctx) => !s ? 'nothing' : s.playlist_id != null ? `playlist "${ctx.lists.find(l => l.id === s.playlist_id)?.name ?? s.playlist_id}"` : s.category_id != null ? `category "${ctx.cats.find(c => c.id === s.category_id)?.name ?? s.category_id}"` : s.item_id != null ? 'item ' + s.item_id : 'a source';
const DAYN = { 0: 'Sun', 1: 'Mon', 2: 'Tue', 3: 'Wed', 4: 'Thu', 5: 'Fri', 6: 'Sat', 7: 'Sun' };
function cronText(c) {
  const m = String(c).match(/^(\d+) (\d+) \* \* ([0-7](?:,[0-7])*|\*)$/);
  return m ? `${m[3] === '*' ? 'every day' : m[3].split(',').map(d => DAYN[d]).join(', ')} ${pad2(+m[2])}:${pad2(+m[1])}` : c;
}
const untilText = u => u === 'next' || u == null ? 'until the next block' : u === 'forever' ? 'for ever' : u.at ? 'until ' + describeWhen(u.at) : u.duration_s != null ? `for ${u.duration_s} s` : u.count != null ? `for ${u.count} songs` : '';
function blockSummary(b, ctx) {
  const pr = b.params || {};
  let s;
  switch (b.kind) {
    case 'start': s = (pr.schedule || []).map(x => x.cron ? cronText(x.cron) : x.date).join('; ') + (pr.anchor === 'absolute' ? ', starts itself' : ', expected, started by hand'); break;
    case 'play': s = `${srcText(pr.source, ctx)}, ${untilText(pr.until)}` + (pr.end === 'natural' ? ', back-timed' : pr.end === 'fade' ? ', fade' : ''); break;
    case 'wait': s = pr.until === 'operator' ? 'until the operator presses Continue' : pr.until?.duration_s != null ? `${pr.until.duration_s} s` : pr.until?.at ? 'until ' + describeWhen(pr.until.at) : ''; break;
    case 'loop': s = `back to ${pr.to ? 'row ' + pr.to : 'the first entry'}, ${pr.times === 'forever' ? 'for ever' : (pr.times ?? 1) + ' times'}`; break;
    case 'fade_stop': s = `${pr.seconds ?? 7} s`; break;
    case 'stop': s = `${pr.how || 'now'}, fade ${pr.fade_ms ?? 2000} ms`; break;
    case 'start_playlist': s = `to "${ctx.lists.find(l => l.id === pr.playlist_id)?.name ?? pr.playlist_id}", ${pr.how || 'segue'}`; break;
    case 'insert': s = srcText(pr.source, ctx); break;
    case 'osc_out': s = `${pr.host}:${pr.port} ${pr.address} ${(pr.args || []).map(a => a.value).join(' ')}`; break;
    case 'midi_out': s = `${pr.port} ${pr.message?.type || ''} channel ${pr.message?.channel ?? ''}`; break;
    case 'http_request': s = `${pr.method || 'POST'} ${pr.url || ''}`; break;
    case 'set_gain': s = `${pr.gain_db} dB over ${pr.ramp_ms ?? 0} ms`; break;
    case 'set_mute': s = `${pr.on === false ? 'unmute' : 'mute'} over ${pr.ramp_ms ?? 0} ms`; break;
    case 'insert_every': s = `every ${pr.n} songs, ${srcText(pr.source, ctx)}`; break;
    default: s = JSON.stringify(pr);
  }
  const when = b.trigger === 'timed' ? describeWhen(pr.when) + (pr.late_policy ? `, late: ${pr.late_policy}` : '') : b.trigger === 'sequence' ? 'in sequence' : '';
  return (when ? when + '. ' : '') + s;
}

/** One block of a playlist as a card: a summary line, and its form when opened. */
function blockCard(ctx, e, idx, probs) {
  const b = e.block;
  const handle = h('span', { class: 'bhandle', draggable: true, title: 'Drag to reorder' }, '\u2630');
  const opened = ctx.open.has(e.id);
  const card = h('div', { class: 'bcard' + (b.enabled === false ? ' off' : ''), 'data-entry': e.id, 'data-kind': b.kind });
  handle.addEventListener('dragstart', ev => { ev.dataTransfer.setData('text/plain', 'row:' + e.id); try { ev.dataTransfer.setDragImage(card, 0, 0); } catch { } });
  const body = h('div', { class: 'bbody' });
  const toggle = h('button', { class: 'bedit', onclick: () => { if (ctx.open.has(e.id)) { ctx.open.delete(e.id); body.replaceChildren(); toggle.textContent = 'Edit'; } else { ctx.open.add(e.id); body.replaceChildren(blockForm(ctx, e, idx)); toggle.textContent = 'Close'; } } }, opened ? 'Close' : 'Edit');
  if (opened) body.append(blockForm(ctx, e, idx));
  const known = b.kind in BADGE;
  card.append(h('div', { class: 'bhead' }, handle, h('span', { class: 'bdg', 'data-b': BADGE[b.kind] || '?' }, BADGE[b.kind] || b.kind),
    h('div', { class: 'meta' }, h('div', { class: 'bname' }, b.name, h('span', { class: 'dim' }, ` (row ${idx + 1})`)), h('div', { class: 'dim bsum' }, blockSummary(b, ctx)),
      ...probs.map(x => h('div', { class: x.severity === 'error' ? 'chip crit' : 'chip warn', style: 'white-space:normal' }, h('code', {}, x.code), ' ', x.message))),
    b.enabled === false ? h('span', { class: 'chip' }, 'off') : null,
    known && b.kind !== 'insert_every' ? toggle : (b.kind === 'insert_every' ? h('span', { class: 'chip warn' }, 'rule') : null),
    h('button', { title: 'Remove', onclick: async () => { ctx.open.delete(e.id); await safe(J(`/api/v1/playlists/${ctx.id}/entries/${e.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); ctx.reload(); } }, 'x')), body);
  return card;
}

/** The form of one block: name, trigger, a time, the block's own fields, Save. No JSON. */
function blockForm(ctx, e, idx) {
  const b = e.block, pr = b.params || {};
  const kf = KIND_FORMS[b.kind] ? KIND_FORMS[b.kind](pr, ctx, idx, b) : null;
  const wrap = h('div', { class: 'bform', 'data-entry': e.id });
  if (!kf) return h('div', { class: 'dim' }, 'The editor does not know this block type. It is kept as it is.');
  const name = txt(b.name, { class: 'f-name', style: 'min-width:14em' });
  const en = h('input', { type: 'checkbox', checked: b.enabled !== false, class: 'f-enabled' });
  const both = BOTH.includes(b.kind) || b.kind === 'insert';
  const trig = selOf([['sequence', 'In sequence'], ['timed', 'At a time']], b.trigger, { class: 'f-trigger' });
  const wf = whenField(pr.when || { kind: 'relative', offset_ms: 0 });
  const lateSel = selOf([['default', 'Default (run if at most 60 s late; Fade out, Stop and End are skipped)'], ['skip', 'Skip it'], ['run_now', 'Run it now'], ['run_if_within', 'Run it if it is at most this late']], pr.late_policy || 'default', { class: 'f-late' });
  const lateWin = num(pr.late_window_s ?? 60, { class: 'f-latewin', min: 0 });
  const timedBox = h('div', {}, line('At', wf.el), line('If it is already past', lateSel, h('span', { class: 'lw' }, lateWin, ' s')));
  // the song-change window (play, liner or ID, start playlist at a time)
  const winOn = h('input', { type: 'checkbox', checked: pr.mode != null || pr.window_early_s != null || pr.window_late_s != null, class: 'f-winon' });
  const wMode = selOf(['soft_then_hard', 'soft', 'hard', 'hard_wait'].map(x => [x, x]), pr.mode || 'soft_then_hard', { class: 'f-mode' });
  const wEarly = num(pr.window_early_s ?? (b.kind === 'insert' ? 60 : 0), { min: 0, class: 'f-early' }), wLate = num(pr.window_late_s ?? 30, { min: 0, class: 'f-wlate' });
  const winBox = h('div', {}, line('Song-change window', h('label', {}, winOn, ' set it'), wMode, h('span', { class: 'dim' }, 'from'), wEarly, h('span', { class: 'dim' }, 's before to'), wLate, h('span', { class: 'dim' }, 's after')));
  const syncT = () => {
    const timed = b.trigger === 'first' ? false : (both ? trig.value === 'timed' : b.trigger === 'timed');
    timedBox.style.display = timed ? '' : 'none';
    winBox.style.display = timed && ['play', 'insert', 'start_playlist'].includes(b.kind) ? '' : 'none';
    lateSel.nextSibling.style.display = lateSel.value === 'run_if_within' ? '' : 'none';
  };
  trig.addEventListener('change', syncT); lateSel.addEventListener('change', syncT); syncT();
  const save = h('button', { class: 'primary f-save', onclick: async () => {
    let params;
    try {
      params = kf.get();
      const timed = both ? trig.value === 'timed' : b.trigger === 'timed';
      if (timed) {
        params.when = wf.get();
        if (lateSel.value !== 'default') { params.late_policy = lateSel.value; if (lateSel.value === 'run_if_within') params.late_window_s = need(lateWin.value, 'The late window'); }
        if (winBox.style.display !== 'none' && winOn.checked) { params.mode = wMode.value; params.window_early_s = need(wEarly.value, 'The window'); params.window_late_s = need(wLate.value, 'The window'); }
        if (pr.fill) params.fill = pr.fill;
      }
    } catch (err) { toast(err.message, true); return; }
    const body = { name: name.value.trim() || BLOCK_NAME[b.kind] || b.kind, enabled: en.checked, params };
    if (both) body.trigger = trig.value;
    const r = await safe(jsend(`/api/v1/playlists/${ctx.id}/entries/${e.id}`, body, 'PATCH'), 'Save');
    if (r) { toast('Saved'); ctx.reload(); }
  } }, 'Save');
  wrap.append(h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Name'), name, both ? [h('span', { class: 'dim' }, 'Runs'), trig] : null, h('label', {}, en, ' Enabled')), timedBox, kf.el, winBox, h('div', { class: 'row', style: 'margin-top:10px' }, save));
  return wrap;
}

/** The `when` of a block as one text box that also reads what a person types: "T0 + 14:40", "00:30 next day", a recurrence. */
function parseWhenText(t) {
  t = String(t).trim(); let m;
  if ((m = t.match(/^T0\s*\+\s*(.+)$/i))) { const ms = parseOffset(m[1]); return ms == null ? null : { kind: 'relative', offset_ms: ms }; }
  if ((m = t.match(/^(\d{1,2}):(\d{2})(?::\d{2})?\s*(next day|tomorrow)$/i))) return { kind: 'local', at: pad2(+m[1]) + ':' + m[2], day_offset: 1 };
  if (/^\d{4}-\d\d-\d\dT\d\d:\d\d(:\d\d)?$/.test(t)) return { kind: 'date', local: t };
  if (/^\S+ \S+ \S+ \S+ \S+$/.test(t) && /^[\d*/,-]+( [\d*/,A-Za-z-]+){4}$/.test(t)) return { kind: 'recurrence', cron: t };
  return null;
}

/** The pickers a block form is built from. Each returns { el, get() }. */
function sourceField(src, ctx, kinds = ['playlist', 'category', 'item']) {
  const names = { playlist: 'a playlist', category: 'a category', item: 'one item' };
  const cur = src?.playlist_id != null ? 'playlist' : src?.item_id != null ? 'item' : src?.category_id != null ? 'category' : kinds[0];
  const kind = selOf(kinds.map(k => [k, names[k]]), cur, { class: 'f-srckind' });
  const pl = selOf(ctx.lists.filter(l => l.id !== ctx.id || src?.playlist_id === l.id).map(l => [l.id, l.name]), src?.playlist_id, { class: 'f-srcplaylist' });
  const cat = selOf(ctx.cats.map(c => [c.id, c.name]), src?.category_id, { class: 'f-srccategory' });
  const q = txt('', { placeholder: 'Search the library', class: 'f-srcq', style: 'width:12em' });
  const found = h('select', { class: 'f-srcitem' }, src?.item_id != null ? h('option', { value: src.item_id }, 'item ' + src.item_id) : null);
  if (src?.item_id != null) J('/api/v1/items/' + src.item_id).then(i => { if (i?.title) found.options[0].textContent = i.title + (i.artist ? ' - ' + i.artist : ''); }).catch(() => { });
  let t; q.addEventListener('input', () => { clearTimeout(t); t = setTimeout(async () => {
    if (!q.value.trim()) return;
    const r = await safe(J('/api/v1/items?per_page=8&q=' + encodeURIComponent(q.value))); if (!r) return;
    found.replaceChildren(...r.items.map(i => h('option', { value: i.id }, `${i.title} - ${i.artist}`)));
  }, 200); });
  const sync = () => { pl.style.display = kind.value === 'playlist' ? '' : 'none'; cat.style.display = kind.value === 'category' ? '' : 'none'; q.style.display = found.style.display = kind.value === 'item' ? '' : 'none'; };
  kind.addEventListener('change', sync); sync();
  const get = () => {
    if (kind.value === 'playlist') { if (pl.value === '') throw new Error('Choose a playlist to play'); return { playlist_id: +pl.value }; }
    if (kind.value === 'category') { if (cat.value === '') throw new Error('Choose a category'); return { category_id: +cat.value }; }
    if (found.value === '') throw new Error('Search for an item and choose it'); return { item_id: +found.value };
  };
  return { el: h('span', { class: 'row' }, kind, pl, cat, q, found), get };
}
function untilField(u, opts = ['next', 'forever', 'at', 'duration', 'count']) {
  const names = { next: 'Until the next block takes over', forever: 'For ever (until a block at a time ends it)', at: 'Until a time', duration: 'For a length of time', count: 'For a number of songs', operator: 'Until the operator presses Continue' };
  const cur = u === 'next' || u == null ? 'next' : u === 'forever' ? 'forever' : u === 'operator' ? 'operator' : u.at ? 'at' : u.duration_s != null ? 'duration' : u.count != null ? 'count' : opts[0];
  const kind = selOf(opts.map(k => [k, names[k]]), cur, { class: 'f-until' });
  const wf = whenField(u?.at || { kind: 'relative', offset_ms: 0 });
  const dur = num(u?.duration_s ?? 60, { min: 1, class: 'f-duration' }), cnt = num(u?.count ?? 3, { min: 1, class: 'f-count' });
  const durL = h('span', { class: 'row' }, dur, h('span', { class: 'dim' }, 's')), cntL = h('span', { class: 'row' }, cnt, h('span', { class: 'dim' }, 'songs'));
  const sync = () => { wf.el.style.display = kind.value === 'at' ? '' : 'none'; durL.style.display = kind.value === 'duration' ? '' : 'none'; cntL.style.display = kind.value === 'count' ? '' : 'none'; };
  kind.addEventListener('change', sync); sync();
  const get = () => {
    switch (kind.value) {
      case 'at': return { at: wf.get() };
      case 'duration': return { duration_s: need(dur.value, 'The length') };
      case 'count': return { count: need(cnt.value, 'The number of songs') };
      default: return kind.value;
    }
  };
  return { el: h('span', { class: 'row' }, kind, wf.el, durL, cntL), get, kind };
}
function templatePick(kind, ctx, fill) {
  const ts = ctx.templates.filter(t => t.kind === kind);
  if (!ts.length) return null;
  const s = h('select', { class: 'f-template', title: ts.map(t => t.notes).join(' ') }, h('option', { value: '' }, 'Fill in from a template'), ts.map(t => h('option', { value: t.id }, t.title)));
  const note = h('span', { class: 'dim', style: 'font-size:12px' });
  s.addEventListener('change', () => { const t = ts.find(x => x.id === s.value); if (t) { fill(JSON.parse(JSON.stringify(t.params))); note.textContent = t.notes || ''; } });
  return h('div', {}, line('Template', s), h('div', { class: 'dim', style: 'margin:2px 0 0 9.5em;font-size:12px' }, note));
}

const KIND_FORMS = {
  start(pr, ctx) {
    const skip = pr.skip_dates || [];
    const rows = h('div', { class: 'sched' });
    const addWeek = (cron, preset) => {
      const m = cron != null ? String(cron).match(/^(\d+) (\d+) \* \* ([0-7](?:,[0-7])*|\*)$/) : null;
      if (cron != null && !m) return addCustom(cron);
      const days = m ? (m[3] === '*' ? [0, 1, 2, 3, 4, 5, 6] : m[3].split(',').map(d => +d % 7)) : [1];
      const tm = h('input', { type: 'time', class: 'f-time', value: m ? `${pad2(+m[2])}:${pad2(+m[1])}` : '18:00' });
      const row = h('div', { class: 'row sched-row', 'data-type': 'week' }, h('span', { class: 'dim' }, 'Every'),
        ...[1, 2, 3, 4, 5, 6, 0].map(d => h('label', { class: 'daybox' }, h('input', { type: 'checkbox', class: 'f-day', 'data-day': d, checked: days.includes(d) }), ' ' + DAYN[d])),
        h('span', { class: 'dim' }, 'at'), tm, h('button', { onclick: () => row.remove() }, 'x'));
      row.get = () => {
        const ds = [...row.querySelectorAll('.f-day')].filter(x => x.checked).map(x => +x.dataset.day).sort((a, b) => a - b);
        if (!ds.length) throw new Error('Choose at least one day for the start');
        const [hh, mm] = tm.value.split(':').map(Number); if (Number.isNaN(hh)) throw new Error('Give a time for the start');
        return { cron: `${mm} ${hh} * * ${ds.length === 7 ? '*' : ds.join(',')}` };
      };
      rows.append(row);
    };
    const addCustom = cron => {
      const v = txt(cron || '0 18 * * 1', { class: 'f-cron', style: 'width:13em' });
      const row = h('div', { class: 'row sched-row', 'data-type': 'cron' }, h('span', { class: 'dim' }, 'Recurrence'), v, h('span', { class: 'dim' }, 'minute hour day month weekday'), h('button', { onclick: () => row.remove() }, 'x'));
      row.get = () => ({ cron: v.value.trim() }); rows.append(row);
    };
    const addDate = d => {
      const v = h('input', { type: 'datetime-local', class: 'f-date', value: (d || '').slice(0, 16) });
      const row = h('div', { class: 'row sched-row', 'data-type': 'date' }, h('span', { class: 'dim' }, 'On one date'), v, h('button', { onclick: () => row.remove() }, 'x'));
      row.get = () => { if (!v.value) throw new Error('Give the date of the start'); return { date: v.value.length === 16 ? v.value + ':00' : v.value }; }; rows.append(row);
    };
    for (const s of pr.schedule || []) { if (s.cron) addWeek(s.cron); else if (s.date) addDate(s.date); }
    const anchor = selOf([['manual', 'Expected: the operator starts it by hand'], ['absolute', 'Starts itself at that time']], pr.anchor || 'manual', { class: 'f-anchor' });
    const armWin = num(pr.arm_window_min ?? 60, { min: 0, class: 'f-armwin' });
    const fbOn = h('input', { type: 'checkbox', class: 'f-fallback', checked: !!pr.fallback_start?.enabled });
    const fbGrace = num(pr.fallback_start?.grace_s ?? 120, { min: 0, class: 'f-grace' });
    const how = selOf([['segue', 'At the next song change'], ['now', 'At once'], ['end', 'When the song on air ends']], pr.how || 'segue', { class: 'f-how' });
    const late = num(pr.window_late_s ?? 300, { min: 0, class: 'f-startlate' });
    const skipBox = h('div', { class: 'row skips' }, skip.map(d => skipChip(d)));
    function skipChip(d) { const c = h('span', { class: 'chip skipchip', 'data-date': d }, d + ' ', h('a', { href: '#', onclick: ev => { ev.preventDefault(); c.remove(); } }, 'x')); return c; }
    const skipIn = h('input', { type: 'date', class: 'f-skipdate' });
    const advanced = h('div', {}, line('How it starts', how), line('Late start window', late, h('span', { class: 'dim' }, 's after its time')));
    const sync = () => { advanced.style.display = anchor.value === 'absolute' ? '' : 'none'; fbGrace.style.display = fbOn.checked ? '' : 'none'; };
    anchor.addEventListener('change', sync); fbOn.addEventListener('change', sync); sync();
    const el = h('div', {}, line('When', h('div', {}, rows, h('div', { class: 'row' },
      h('button', { class: 'f-addweek', onclick: () => addWeek(null) }, 'Add a weekly time'), h('button', { onclick: () => addDate(null) }, 'Add a date'), h('button', { onclick: () => addCustom(null) }, 'Add a recurrence')))),
      line('Starts', anchor), line('Arm window', armWin, h('span', { class: 'dim' }, 'minutes: a hand start this close to the time counts as this show')),
      line('Fallback', h('label', {}, fbOn, ' Start by itself if nobody has started it, after'), fbGrace, h('span', { class: 'dim grace-s' }, 's')),
      advanced, line('Skip these dates', skipBox, skipIn, h('button', { onclick: () => { if (skipIn.value) { skipBox.append(skipChip(skipIn.value)); skipIn.value = ''; } } }, 'Add')));
    const get = () => {
      const schedule = [...rows.children].map(r => r.get());
      if (!schedule.length) throw new Error('Add at least one time to the Start block');
      const params = { schedule, anchor: anchor.value, arm_window_min: need(armWin.value, 'The arm window'), fallback_start: fbOn.checked ? { enabled: true, grace_s: need(fbGrace.value, 'The grace time') } : { enabled: false } };
      const sk = [...skipBox.querySelectorAll('.skipchip')].map(c => c.dataset.date); if (sk.length) params.skip_dates = sk;
      if (anchor.value === 'absolute') { if (how.value !== 'segue') params.how = how.value; if (late.value !== '' && +late.value !== 300) params.window_late_s = +late.value; }
      return params;
    };
    return { el, get };
  },
  play(pr, ctx) {
    const sf = sourceField(pr.source, ctx), uf = untilField(pr.until);
    const bt = h('input', { type: 'checkbox', class: 'f-backtime', checked: pr.end === 'natural' });
    const btMode = selOf([['best_fit', 'Best fit: the song that ends nearest the end time'], ['exact', 'Exact: start the last song so it ends on the end time']], pr.back_time?.mode || 'best_fit', { class: 'f-btmode' });
    const btTol = num(pr.back_time?.tolerance_s ?? 45, { min: 1, step: 'any', class: 'f-bttol' });
    const btBox = h('span', { class: 'row' }, btMode, h('span', { class: 'dim' }, 'tolerance'), btTol, h('span', { class: 'dim' }, 's'));
    const endSel = selOf([['boundary', 'At the first song change after the end time'], ['fade', 'Fade out']], pr.end === 'fade' ? 'fade' : 'boundary', { class: 'f-end' });
    const fade = num(pr.fade_s ?? 7, { min: 0, max: 120, step: 'any', class: 'f-fades' });
    const fadeL = h('span', { class: 'row' }, fade, h('span', { class: 'dim' }, 's'));
    const endRow = line('At the end time', endSel, fadeL), btRow = line('Back-timing', h('label', {}, bt, ' End so that a song finishes at the end time (back-timing)'), btBox);
    const sync = () => {
      const k = uf.kind.value, hasEnd = k === 'at' || k === 'duration';
      btRow.style.display = k === 'forever' || k === 'count' ? 'none' : '';
      btBox.style.display = bt.checked ? '' : 'none';
      endRow.style.display = hasEnd && !bt.checked ? '' : 'none'; fadeL.style.display = endSel.value === 'fade' ? '' : 'none';
    };
    [uf.kind, bt, endSel].forEach(x => x.addEventListener('change', sync)); sync();
    const get = () => {
      const params = { source: sf.get(), until: uf.get() };
      const k = uf.kind.value;
      if (bt.checked && k !== 'forever' && k !== 'count') { params.end = 'natural'; params.back_time = { mode: btMode.value, tolerance_s: need(btTol.value, 'The tolerance') }; }
      else if ((k === 'at' || k === 'duration') && endSel.value === 'fade') { params.end = 'fade'; params.fade_s = need(fade.value, 'The fade'); }
      return params;
    };
    return { el: h('div', {}, line('Play', sf.el), line('For how long', uf.el), btRow, endRow), get };
  },
  wait(pr) {
    const uf = untilField(pr.until, ['operator', 'duration', 'at']);
    return { el: line('Wait', uf.el), get: () => ({ until: uf.get() }) };
  },
  loop(pr, ctx, idx) {
    const to = selOf([['', 'The first entry after the Start block'], ...ctx.entries.slice(0, idx).map((e, i) => [i + 1, `Row ${i + 1}: ${e.type === 'block' ? e.block.name : e.type === 'item' ? (e.item?.title || 'item') : 'album'}`])], pr.to ?? '', { class: 'f-loopto' });
    const times = selOf([['n', 'This many times'], ['forever', 'For ever']], pr.times === 'forever' ? 'forever' : 'n', { class: 'f-looptimes' });
    const n = num(typeof pr.times === 'number' ? pr.times : 2, { min: 1, class: 'f-loopn' });
    const sync = () => { n.style.display = times.value === 'n' ? '' : 'none'; }; times.addEventListener('change', sync); sync();
    return { el: h('div', {}, line('Go back to', to), line('Repeat', times, n, h('span', { class: 'dim' }, 'passes of the part in between, counting the first'))), get: () => {
      const params = { times: times.value === 'forever' ? 'forever' : need(n.value, 'The number of times') };
      if (to.value !== '') params.to = +to.value; return params; } };
  },
  fade_stop(pr) {
    const s = num(pr.seconds ?? 7, { min: 0, step: 'any', class: 'f-seconds' });
    return { el: line('Fade over', s, h('span', { class: 'dim' }, 's, along a cosine curve, then stop')), get: () => ({ seconds: need(s.value, 'The fade'), curve: 'cosine' }) };
  },
  stop(pr) {
    const how = selOf([['now', 'Now, in the middle of the song'], ['boundary', 'At the next song change'], ['after_current', 'After the song on air ends']], pr.how || 'now', { class: 'f-stophow' });
    const fm = num(pr.fade_ms ?? 2000, { min: 0, class: 'f-fadems' });
    return { el: line('Stop', how, h('span', { class: 'dim' }, 'fade'), fm, h('span', { class: 'dim' }, 'ms')), get: () => ({ how: how.value, fade_ms: need(fm.value, 'The fade') }) };
  },
  end() { return { el: h('div', { class: 'dim' }, 'This block ends the playlist it is in. It has no settings.'), get: () => ({}) }; },
  start_playlist(pr, ctx) {
    const pl = selOf(ctx.lists.filter(l => l.id !== ctx.id || pr.playlist_id === l.id).map(l => [l.id, l.name]), pr.playlist_id, { class: 'f-switchto' });
    const how = selOf([['segue', 'At the next song change'], ['now', 'At once'], ['end', 'When the song on air ends']], pr.how || 'segue', { class: 'f-how' });
    return { el: line('Switch to', pl, how), get: () => { if (pl.value === '') throw new Error('Choose a playlist'); return { playlist_id: +pl.value, how: how.value }; } };
  },
  insert(pr, ctx) {
    const sf = sourceField(pr.source, ctx, ['category', 'item']);
    return { el: line('Take from', sf.el), get: () => ({ source: sf.get() }) };
  },
  osc_out(pr, ctx) {
    const host = txt(pr.host, { class: 'f-host', style: 'width:12em' }), port = num(pr.port ?? 2223, { min: 1, max: 65535, class: 'f-port' }), addr = txt(pr.address, { class: 'f-address', style: 'width:14em' });
    const args = h('div', { class: 'osc-args' });
    const addArg = (a = { type: 'i', value: 0 }) => {
      const t = selOf([['i', 'whole number'], ['f', 'number'], ['s', 'text']], a.type, { class: 'f-argtype' }), v = txt(a.value, { class: 'f-argvalue', style: 'width:10em' });
      const row = h('div', { class: 'row osc-arg' }, t, v, h('button', { onclick: () => row.remove() }, 'x'));
      row.get = () => {
        if (t.value === 'i') { if (!/^-?\d+$/.test(v.value.trim())) throw new Error('A whole-number argument reads like 1'); return { type: 'i', value: parseInt(v.value, 10) }; }
        if (t.value === 'f') return { type: 'f', value: need(v.value.trim(), 'A number argument') };
        return { type: 's', value: v.value };
      };
      args.append(row);
    };
    (pr.args || []).forEach(addArg);
    const tp = templatePick('osc_out', ctx, t => { host.value = t.host ?? host.value; port.value = t.port ?? port.value; addr.value = t.address ?? ''; args.replaceChildren(); (t.args || []).forEach(addArg); });
    return { el: h('div', {}, tp, line('Send to', host, h('span', { class: 'dim' }, 'port'), port), line('OSC address', addr), line('Arguments', h('div', {}, args, h('button', { class: 'f-addarg', onclick: () => addArg() }, 'Add an argument')))),
      get: () => ({ host: host.value.trim(), port: need(port.value, 'The port'), address: addr.value.trim(), args: [...args.children].map(r => r.get()) }) };
  },
  midi_out(pr, ctx) {
    const m = pr.message || {};
    const port = txt(pr.port, { class: 'f-port', style: 'width:12em' });
    const type = selOf([['cc', 'Control change'], ['note_on', 'Note on'], ['note_off', 'Note off'], ['pc', 'Program change'], ['sysex', 'System exclusive']], m.type || 'cc', { class: 'f-miditype' });
    const ch = num(m.channel ?? 1, { min: 1, max: 16, class: 'f-midichan' }), d1 = num(m.data1 ?? 0, { min: 0, max: 127, class: 'f-d1' }), d2 = num(m.data2 ?? 0, { min: 0, max: 127, class: 'f-d2' });
    const hex = txt((m.data || []).map(x => x.toString(16).toUpperCase().padStart(2, '0')).join(' '), { class: 'f-sysex', style: 'width:24em', placeholder: 'F0 7E 7F 09 01 F7' });
    const l1 = h('span', { class: 'dim' }), l2 = h('span', { class: 'dim' });
    const r1 = line('', l1, d1), r2 = line('', l2, d2), rch = line('Channel', ch), rhex = line('Bytes (hex)', hex);
    const sync = () => {
      const t = type.value;
      l1.textContent = t === 'cc' ? 'Controller number' : t === 'pc' ? 'Program number' : 'Note number'; l2.textContent = t === 'cc' ? 'Value' : 'Velocity';
      rhex.style.display = t === 'sysex' ? '' : 'none'; r1.style.display = t === 'sysex' ? 'none' : ''; r2.style.display = ['sysex', 'pc'].includes(t) ? 'none' : ''; rch.style.display = t === 'sysex' ? 'none' : '';
    };
    type.addEventListener('change', sync); sync();
    const tp = templatePick('midi_out', ctx, t => { port.value = t.port || ''; type.value = t.message?.type || 'cc'; ch.value = t.message?.channel ?? 1; d1.value = t.message?.data1 ?? 0; d2.value = t.message?.data2 ?? 0; sync(); });
    return { el: h('div', {}, tp, line('Port', port, h('span', { class: 'dim' }, 'a raw MIDI device of the box, such as hw:1,0')), line('Message', type), rch, r1, r2, rhex), get: () => {
      const t = type.value; const message = { type: t };
      if (t === 'sysex') { message.data = hex.value.trim().split(/[\s,]+/).filter(Boolean).map(x => { const v = parseInt(x, 16); if (Number.isNaN(v) || v < 0 || v > 255) throw new Error('A system exclusive byte is two hex digits, such as 7E'); return v; }); }
      else { message.channel = need(ch.value, 'The channel'); message.data1 = need(d1.value, 'The first value'); if (!['pc'].includes(t)) message.data2 = need(d2.value, 'The second value'); }
      return { port: port.value.trim(), message };
    } };
  },
  http_request(pr, ctx) {
    const method = selOf(['GET', 'POST', 'PUT', 'PATCH', 'DELETE'].map(x => [x, x]), (pr.method || 'POST').toUpperCase(), { class: 'f-method' });
    const url = txt(pr.url, { class: 'f-url', style: 'width:26em', placeholder: 'http://' });
    const body = h('textarea', { rows: 3, class: 'f-body', style: 'width:100%' }, pr.body || '');
    const to = num(pr.timeout_s ?? 5, { min: 1, max: 30, class: 'f-timeout' });
    const hdrs = h('div', { class: 'hdrs' });
    const addH = (k = '', v = '') => { const kk = txt(k, { class: 'f-hk', placeholder: 'Header', style: 'width:11em' }), vv = txt(v, { class: 'f-hv', placeholder: 'Value', style: 'width:16em' }); const row = h('div', { class: 'row' }, kk, vv, h('button', { onclick: () => row.remove() }, 'x')); row.get = () => [kk.value.trim(), vv.value]; hdrs.append(row); };
    Object.entries(pr.headers || {}).forEach(([k, v]) => addH(k, v));
    const tp = templatePick('http_request', ctx, t => { method.value = (t.method || 'POST').toUpperCase(); url.value = t.url || ''; body.value = t.body || ''; to.value = t.timeout_s ?? 5; hdrs.replaceChildren(); Object.entries(t.headers || {}).forEach(([k, v]) => addH(k, v)); });
    return { el: h('div', {}, tp, line('Request', method, url), line('Headers', h('div', {}, hdrs, h('button', { onclick: () => addH() }, 'Add a header'))), line('Body', h('div', { style: 'flex:1;min-width:20em' }, body)), line('Give up after', to, h('span', { class: 'dim' }, 's (1 to 30). Plain http only.'))),
      get: () => { const headers = {}; for (const r of hdrs.children) { const [k, v] = r.get(); if (k) headers[k] = v; } return { method: method.value, url: url.value.trim(), headers, body: body.value, timeout_s: need(to.value, 'The timeout') }; } };
  },
  set_gain(pr) {
    const g = num(pr.gain_db ?? 0, { max: 0, step: 'any', class: 'f-gain' }), r = num(pr.ramp_ms ?? 0, { min: 0, class: 'f-ramp' });
    return { el: line('Master gain', g, h('span', { class: 'dim' }, 'dB (0 or lower) over'), r, h('span', { class: 'dim' }, 'ms')), get: () => { const v = need(g.value, 'The gain'); if (v > 0) throw new Error('The gain can lower the level, not raise it: use 0 dB or less'); return { gain_db: v, ramp_ms: need(r.value, 'The ramp') }; } };
  },
  set_mute(pr) {
    const on = selOf([['on', 'Mute the output'], ['off', 'Unmute the output']], pr.on === false ? 'off' : 'on', { class: 'f-mute' }), r = num(pr.ramp_ms ?? 0, { min: 0, class: 'f-ramp' });
    return { el: line('Output', on, h('span', { class: 'dim' }, 'over'), r, h('span', { class: 'dim' }, 'ms')), get: () => ({ on: on.value === 'on', ramp_ms: need(r.value, 'The ramp') }) };
  },
};

/** The dry run: every block of a program with its wall time, the silent gaps, and what is already past. */
function dryRunCard(id) {
  const mode = selOf([['now', 'Now'], ['nominal', 'Expected start'], ['time', 'A time']], 'now', { class: 'f-dryt0' });
  const at = txt('', { class: 'f-dryat', placeholder: 'HH:MM or 2027-01-18T18:45:00', style: 'width:15em;display:none' });
  const out = h('div', { class: 'dryout' });
  mode.addEventListener('change', () => { at.style.display = mode.value === 'time' ? '' : 'none'; });
  const run = async () => {
    const t0 = mode.value === 'time' ? at.value.trim() : mode.value;
    if (!t0) return toast('Give a time for T0', true);
    const pv = await safe(J(`/api/v1/playlists/${id}/preview?t0=${encodeURIComponent(t0)}`), 'Dry run'); if (!pv) return;
    const when = s => s ? String(s).replace('T', ' ') : '';
    out.replaceChildren(
      h('div', { class: 'dim' }, `T0 ${when(pv.t0)} (${pv.time_zone}), now ${when(pv.now)}. Nothing fires in a dry run.`),
      pv.blocks.length ? h('table', { class: 'drytable' }, h('thead', {}, h('tr', {}, ['Row', 'Block', 'Type', 'Runs', 'At', 'Rule', 'Note'].map(x => h('th', {}, x)))),
        h('tbody', {}, pv.blocks.map(r => h('tr', {}, h('td', {}, r.position), h('td', { style: `padding-left:${8 + 16 * (r.depth || 0)}px` }, r.name), h('td', {}, h('span', { class: 'bdg', 'data-b': BADGE[r.type] || r.type }, BADGE[r.type] || r.type)), h('td', {}, r.trigger === 'timed' ? 'at a time' : r.trigger === 'sequence' ? 'in sequence' : r.trigger),
          h('td', { class: 'mono dryat' }, when(r.at)), h('td', {}, typeof r.when === 'string' ? r.when : r.when ? describeWhen(r.when) : ''), h('td', { class: 'dim' }, r.note || ''))))) : h('div', { class: 'dim' }, 'This playlist has no blocks to place.'),
      ...pv.silent_gaps.map(g => h('div', { class: 'notice dry-gap' }, `Silent for ${g.seconds} s before "${g.block}": from ${when(g.from)} to ${when(g.to)}.`)),
      pv.past.length ? h('div', {}, h('h3', { style: 'margin-top:12px' }, 'Already past at this T0'), h('table', { class: 'drypast' }, h('thead', {}, h('tr', {}, ['Block', 'Type', 'Was due', 'Late by', 'Late policy', 'What happens'].map(x => h('th', {}, x)))),
        h('tbody', {}, pv.past.map(r => h('tr', {}, h('td', {}, r.name), h('td', {}, BADGE[r.type] || r.type), h('td', { class: 'mono' }, when(r.at)), h('td', { class: 'mono' }, r.late_by_s + ' s'), h('td', {}, r.policy), h('td', {}, r.action)))))) : h('div', { class: 'dim', style: 'margin-top:8px' }, 'No block is already past at this T0.'),
      ...(pv.problems || []).map(x => h('div', { class: x.severity === 'error' ? 'chip crit' : 'chip warn', style: 'white-space:normal;display:block;margin-top:4px' }, h('code', {}, x.code), ' ', x.message)));
  };
  return h('div', { class: 'card dryrun' }, h('h3', {}, 'Dry run', helpLink('playlists', 'dry-run')),
    h('div', { class: 'row' }, h('span', { class: 'dim' }, 'T0 is'), mode, at, h('button', { class: 'primary f-dryrun', onclick: run }, 'Dry run')), out);
}

// The examples card of the Playlists page: a ready-made program built from the playlists you choose.
async function examplesCard(lists) {
  const ex = await J('/api/v1/examples').catch(() => []);
  const card = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Examples', helpLink('playlists', 'examples')));
  if (!ex.length) return null;
  for (const e of ex) {
    const fields = {};
    const form = h('div', { class: 'exform', style: 'display:none;margin-top:8px' });
    form.append(h('div', { class: 'dim', style: 'margin-bottom:6px' }, e.description), h('div', { class: 'form' }, e.needs.flatMap(n => {
      const inp = n.kind === 'playlist' ? selOf(lists.map(l => [l.id, l.name]), null, { class: 'ex-' + n.key }) : n.kind === 'number' ? num(n.default ?? '', { class: 'ex-' + n.key }) : txt(n.default ?? '', { class: 'ex-' + n.key });
      fields[n.key] = [n, inp]; return [h('label', {}, n.label), inp];
    }), h('label', {}, 'Name of the new playlist'), (fields.name = [{ kind: 'text', key: 'name' }, txt(e.title.replace(/ program$/, ''), { class: 'ex-name' })])[1],
      h('label', { title: 'The program runs on this station only' }, 'Station'), (fields.station_id = [{ kind: 'station', key: 'station_id' }, h('select', { class: 'ex-station' }, h('option', { value: '' }, '(any station)'), (S.stations || []).map(s => h('option', { value: s.id, selected: s.id === S.station }, s.display_name || s.id)))])[1]),
      h('div', { class: 'row', style: 'margin-top:8px' }, h('button', { class: 'primary ex-create', onclick: async () => {
        const body = {};
        for (const [k, [n, inp]] of Object.entries(fields)) { if (k === 'name') { if (inp.value.trim()) body.name = inp.value.trim(); continue; } if (k === 'station_id') { if (inp.value) body.station_id = inp.value; continue; } body[k] = n.kind === 'playlist' || n.kind === 'number' ? +inp.value : inp.value.trim(); if ((n.kind === 'playlist' || n.kind === 'number') && !inp.value) return toast(n.label + ' needs a value', true); }
        const r = await safe(jsend('/api/v1/examples/' + e.id, body), 'Create'); if (r) location.hash = '#/playlists/' + r.id; } }, 'Create the program')));
    card.append(h('div', {}, h('button', { class: 'ex-open', onclick: () => { form.style.display = form.style.display === 'none' ? '' : 'none'; } }, e.title), form));
  }
  return card;
}

// ---- timed blocks (4.4, 4.5): the block editor for insert, play, start_playlist, stop, fade_stop and end ----
const TIMED_TYPES = [['insert', 'Insert an item (an ID)'], ['play', 'Play a source'], ['start_playlist', 'Switch playlist'], ['stop', 'Stop'], ['fade_stop', 'Fade out and stop'], ['end', 'End the program']];
function parseOffset(t) {
  const m = String(t).trim().replace(/^T0\s*\+\s*/i, '').match(/^(?:(\d+):)?(\d+):(\d+)(?:\.(\d{1,3}))?$/);
  if (!m) return null;
  const ms = m[4] ? +m[4].padEnd(3, '0') : 0;
  return (((+(m[1] || 0)) * 60 + +m[2]) * 60 + +m[3]) * 1000 + ms;
}
function fmtOffset(ms) { const s = Math.floor(ms / 1000); return `${pad2(Math.floor(s / 3600))}:${pad2(Math.floor(s / 60) % 60)}:${pad2(s % 60)}`; }
/** The `when` field of a timed block: a time after T0, a local time, a recurrence or one date. */
function whenField(w) {
  const kind = h('select', { class: 'w-kind' }, [['recurrence', 'repeats'], ['relative', 'after T0'], ['local', 'at a time of day'], ['date', 'on one date']].map(([v, l]) => h('option', { value: v, selected: (w?.kind || 'recurrence') === v }, l)));
  const val = h('input', { class: 'w-val', style: 'width:13em' });
  const next = h('label', { class: 'dim' }, h('input', { type: 'checkbox', checked: (w?.day_offset || 0) > 0 }), ' next day');
  const hint = h('span', { class: 'dim' });
  const presets = h('span', {}, [['every hour', '0 * * * *'], ['every half hour', '*/30 * * * *']].map(([l, c]) => h('button', { onclick: () => { kind.value = 'recurrence'; val.value = c; sync(); } }, l)));
  const set = () => {
    if (!w) val.value = '0 * * * *';
    else if (w.kind === 'recurrence') val.value = w.cron;
    else if (w.kind === 'relative') val.value = fmtOffset(w.offset_ms);
    else if (w.kind === 'local') val.value = w.at;
    else val.value = w.local;
  };
  set();
  const sync = () => {
    const k = kind.value;
    hint.textContent = { recurrence: 'minute hour day month weekday, local time', relative: 'hh:mm:ss after the program starts (T0)', local: 'hh:mm in local time', date: 'YYYY-MM-DDThh:mm:ss, local time' }[k];
    next.style.display = k === 'local' ? '' : 'none'; presets.style.display = k === 'recurrence' ? '' : 'none';
  };
  // Typing "T0 + 14:40", "00:30 next day", a recurrence or a date picks the matching kind.
  const apply = sm => { kind.value = sm.kind; if (sm.kind === 'relative') val.value = fmtOffset(sm.offset_ms); else if (sm.kind === 'local') { val.value = sm.at; next.firstChild.checked = (sm.day_offset || 0) > 0; } else if (sm.kind === 'date') val.value = sm.local; else val.value = sm.cron; sync(); };
  val.addEventListener('input', () => { const sm = parseWhenText(val.value); if (sm && sm.kind !== kind.value) { kind.value = sm.kind; sync(); } if (sm?.kind === 'local') next.firstChild.checked = true; });
  val.addEventListener('change', () => { const sm = parseWhenText(val.value); if (sm) apply(sm); });
  kind.addEventListener('change', () => { w = null; if (kind.value === 'relative') val.value = '00:14:40'; else if (kind.value === 'local') val.value = '00:30'; else if (kind.value === 'date') val.value = new Date(Date.now() + 36e5).toISOString().slice(0, 19); else val.value = '0 * * * *'; sync(); });
  sync();
  const get = () => {
    const k = kind.value, v = val.value.trim();
    const sm = parseWhenText(v);
    if (sm) { if (sm.kind === 'recurrence') sm.skip_dates = w?.skip_dates || []; return sm; }
    if (k === 'recurrence') return { kind: 'recurrence', cron: v, skip_dates: w?.skip_dates || [] };
    if (k === 'relative') { const ms = parseOffset(v); if (ms == null) throw new Error('the time after T0 reads hh:mm:ss'); return { kind: 'relative', offset_ms: ms }; }
    if (k === 'local') return { kind: 'local', at: v, day_offset: next.firstChild.checked ? 1 : 0 };
    return { kind: 'date', local: v };
  };
  return { el: h('span', { class: 'row' }, kind, val, next, presets, hint), get };
}
function describeWhen(w) {
  if (!w) return '';
  return w.kind === 'recurrence' ? 'repeats ' + w.cron : w.kind === 'relative' ? 'T0 + ' + fmtOffset(w.offset_ms) : w.kind === 'local' ? w.at + ((w.day_offset || 0) > 0 ? ' next day' : '') : w.local;
}
async function timedBlockForm(playlistId, entry, cats, lists) {
  const b = entry?.block, pr = b?.params || {};
  const type = h('select', {}, TIMED_TYPES.map(([v, l]) => h('option', { value: v, selected: b?.kind === v }, l)));
  if (b) type.disabled = true;
  const name = h('input', { value: b?.name || '' });
  const wf = whenField(pr.when);
  const catSel = h('select', {}, cats.map(c => h('option', { value: c.id }, c.name)));
  const plSel = h('select', {}, lists.map(l => h('option', { value: l.id }, l.name)));
  const srcKind = h('select', {}, [['category', 'a category'], ['playlist', 'a playlist']].map(([v, l]) => h('option', { value: v }, l)));
  const src = pr.source || (pr.playlist_id ? { playlist_id: pr.playlist_id } : null);
  if (src?.playlist_id) { srcKind.value = 'playlist'; plSel.value = src.playlist_id; } else if (src?.category_id) catSel.value = src.category_id;
  const mode = h('select', {}, ['soft_then_hard', 'soft', 'hard', 'hard_wait'].map(x => h('option', { value: x, selected: (pr.mode || 'soft_then_hard') === x }, x)));
  const early = h('input', { type: 'number', min: 0, style: 'width:5em', value: pr.window_early_s ?? (b?.kind === 'insert' || !b ? 60 : 0) }), late = h('input', { type: 'number', min: 0, style: 'width:5em', value: pr.window_late_s ?? 30 });
  const fadeMs = h('input', { type: 'number', min: 0, style: 'width:6em', value: pr.fade_ms ?? 2000 }), secs = h('input', { type: 'number', min: 0, style: 'width:5em', value: pr.seconds ?? 7 });
  const how = h('select', {}, ['now', 'boundary', 'after_current'].map(x => h('option', { value: x, selected: (pr.how || 'now') === x }, x)));
  const rows = {
    source: h('span', { class: 'row' }, h('span', { class: 'dim' }, 'from'), srcKind, catSel, plSel),
    window: h('span', { class: 'row' }, mode, h('span', { class: 'dim' }, 'window from'), early, h('span', { class: 'dim' }, 's before to'), late, h('span', { class: 'dim' }, 's after')),
    stop: h('span', { class: 'row' }, how, h('span', { class: 'dim' }, 'fade'), fadeMs, h('span', { class: 'dim' }, 'ms')),
    fade: h('span', { class: 'row' }, h('span', { class: 'dim' }, 'fade over'), secs, h('span', { class: 'dim' }, 's')),
    pl: h('span', { class: 'row' }, h('span', { class: 'dim' }, 'switch to'), plSel.cloneNode(true)),
  };
  const sw = rows.pl.lastChild; if (pr.playlist_id) sw.value = pr.playlist_id;
  const show = () => {
    const t = type.value;
    rows.source.style.display = t === 'insert' || t === 'play' ? '' : 'none'; srcKind.style.display = t === 'play' ? '' : 'none';
    const pls = srcKind.value === 'playlist' && t === 'play'; plSel.style.display = pls ? '' : 'none'; catSel.style.display = pls ? 'none' : '';
    rows.window.style.display = ['insert', 'play', 'start_playlist'].includes(t) ? '' : 'none';
    rows.stop.style.display = t === 'stop' ? '' : 'none'; rows.fade.style.display = t === 'fade_stop' ? '' : 'none'; rows.pl.style.display = t === 'start_playlist' ? '' : 'none';
  };
  [type, srcKind].forEach(x => x.addEventListener('change', show)); show();
  const build = () => {
    const t = b?.kind || type.value; const params = { when: wf.get() };
    if (t === 'insert' || t === 'play') params.source = srcKind.value === 'playlist' && t === 'play' ? { playlist_id: +plSel.value } : { category_id: +catSel.value };
    if (t === 'play') params.until = pr.until || 'next';
    if (['insert', 'play', 'start_playlist'].includes(t)) { params.mode = mode.value; params.window_early_s = +early.value; params.window_late_s = +late.value; }
    if (t === 'start_playlist') { params.playlist_id = +sw.value; params.how = pr.how || 'segue'; }
    if (t === 'stop') { params.how = how.value; params.fade_ms = +fadeMs.value; }
    if (t === 'fade_stop') { params.seconds = +secs.value; params.curve = 'cosine'; }
    return { t, params };
  };
  const save = h('button', { class: 'primary', onclick: async () => {
    let r; try { r = build(); } catch (e) { toast(e.message, true); return; }
    const res = b ? await safe(jsend(`/api/v1/playlists/${playlistId}/entries/${entry.id}`, { name: name.value || r.t, params: r.params }, 'PATCH'), 'Save')
      : await safe(jsend(`/api/v1/playlists/${playlistId}/entries`, { block: { kind: r.t, name: name.value || r.t, trigger: 'timed', params: r.params } }), 'Add');
    if (res) { toast(b ? 'Saved' : 'Added'); route(); } } }, b ? 'Save' : 'Add block');
  return h('div', { class: 'card timedform', style: 'margin-top:8px;background:var(--panel2)' }, h('div', { class: 'row' }, b ? h('b', {}, b.kind) : h('b', {}, 'Add a timed block'), type, name),
    h('div', { class: 'row', style: 'margin-top:6px' }, h('span', { class: 'dim' }, 'When'), wf.el), h('div', { class: 'row', style: 'margin-top:6px' }, rows.source, rows.pl, rows.stop, rows.fade), h('div', { class: 'row', style: 'margin-top:6px' }, rows.window), h('div', { class: 'row', style: 'margin-top:8px' }, save));
}

function insertForm(playlistId) {
  const n = h('input', { type: 'number', value: 4, min: 1 }), cat = h('select', {}), nm = h('input', { value: 'Liner every 4' });
  J('/api/v1/categories').then(r => r.categories.forEach(c => cat.append(h('option', { value: c.name, selected: c.name === 'liner' }, c.name))));
  return h('div', { style: 'margin-top:14px' }, h('h3', { style: 'color:var(--dim);font-size:13px;text-transform:uppercase' }, 'Insert rule', helpLink('insert', 'playlist-rules')), h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Name'), nm, h('span', { class: 'dim' }, 'a'), cat, h('span', { class: 'dim' }, 'item every'), n, h('span', { class: 'dim' }, 'songs'),
    h('button', { onclick: async () => { const r = await safe(jsend(`/api/v1/playlists/${playlistId}/entries`, { block: { kind: 'insert_every', name: nm.value, n: +n.value, source: { category: cat.value } } }), 'Insert rule'); if (r) route(); } }, 'Add rule')));
}


// ================= Schedule =================
// The weekly grid of each station's scheduled starts (the recurring `start` blocks, 4.6), the station blocks
// (the hourly ID, station-wide liners), one-shot changes, and what will fire next with its predicted time.
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const DAY_ORDER = [1, 2, 3, 4, 5, 6, 0];
const HUES = [38, 200, 140, 300, 10, 80, 250, 170];
const hueOf = id => HUES[Math.abs(id) % HUES.length];
const pad2 = n => String(n).padStart(2, '0');
const hm = min => pad2(Math.floor(min / 60)) + ':' + pad2(min % 60);
/** The recurrences of a start block from its slots: one `m h * * days` per distinct time. */
function cronsOf(slots) {
  const by = new Map();
  for (const s of slots) { if (!by.has(s.minute_of_day)) by.set(s.minute_of_day, new Set()); by.get(s.minute_of_day).add(s.day); }
  return [...by.entries()].sort((a, b) => a[0] - b[0]).map(([m, days]) => {
    const d = [...days].sort((a, b) => a - b);
    return `${m % 60} ${Math.floor(m / 60)} * * ${d.length === 7 ? '*' : d.join(',')}`;
  });
}
async function startBlockOf(playlistId, blockId) {
  const p = await J('/api/v1/playlists/' + playlistId);
  const e = p.entries.find(x => x.type === 'block' && x.block.id === blockId);
  return { p, e };
}
routes.schedule = async () => {
  const root = h('div'); root.append(h('h1', {}, 'Schedule', helpLink('schedule')), pickStation(() => route()));
  if (!S.station) { root.append(h('div', { class: 'notice' }, 'No station is configured on this box.')); return root; }
  const week = await J('/api/v1/schedule/week?station=' + encodeURIComponent(S.station));
  root.append(h('div', { class: 'dim' }, `Times are ${week.time_zone} local time. A scheduled start switches at the first segue after its time and never cuts a song.`));
  // ---- the grid
  const grid = h('div', { class: 'wgrid', id: 'wgrid' });
  const cell = (day, hour) => h('div', { class: 'wcell', 'data-day': day, 'data-hour': hour });
  grid.append(h('div', { class: 'whead' }));
  for (const d of DAY_ORDER) grid.append(h('div', { class: 'whead' }, DAYS[d]));
  for (let hr = 0; hr < 24; hr++) {
    grid.append(h('div', { class: 'whour' }, pad2(hr) + ':00'));
    for (const d of DAY_ORDER) {
      const c = cell(d, hr);
      c.addEventListener('dragover', ev => { ev.preventDefault(); c.classList.add('over'); });
      c.addEventListener('dragleave', () => c.classList.remove('over'));
      c.addEventListener('drop', async ev => {
        ev.preventDefault(); c.classList.remove('over');
        let from; try { from = JSON.parse(ev.dataTransfer.getData('text/plain')); } catch { return; }
        // Dropped on a cell: the dragged day and time become this cell's day and the time under the pointer
        // (to the quarter hour).
        const rect = c.getBoundingClientRect(); const q = Math.min(3, Math.max(0, Math.floor((ev.clientY - rect.top) / rect.height * 4)));
        const minute = (+c.dataset.hour) * 60 + q * 15;
        const st = week.starts.find(x => x.block_id === from.block_id); if (!st) return;
        const slots = st.slots.filter(x => !(x.day === from.day && x.minute_of_day === from.minute_of_day));
        slots.push({ day: +c.dataset.day, minute_of_day: minute });
        const { e } = await startBlockOf(st.playlist_id, st.block_id);
        const params = { ...e.block.params, schedule: cronsOf(slots).map(cron => ({ cron })).concat((st.dates || []).map(date => ({ date }))) };
        const r = await safe(jsend(`/api/v1/playlists/${st.playlist_id}/entries/${e.id}`, { params }, 'PATCH'), 'Move');
        if (r) { toast(`${st.playlist} now starts ${DAYS[+c.dataset.day]} ${hm(minute)}`); route(); }
      });
      grid.append(c);
    }
  }
  const cellAt = (day, minute) => grid.querySelector(`.wcell[data-day="${day}"][data-hour="${Math.floor(minute / 60)}"]`);
  for (const st of week.starts) {
    for (const sl of st.slots) {
      const c = cellAt(sl.day, sl.minute_of_day); if (!c) continue;
      const chip = h('div', { class: 'wslot' + (st.anchor === 'manual' ? ' manual' : ''), draggable: true, style: `--hue:${hueOf(st.playlist_id)};top:${(sl.minute_of_day % 60) / 60 * 100}%`,
        title: `${st.playlist}, ${sl.time}, ${st.anchor === 'manual' ? 'expected (started by hand)' : st.how}` }, `${sl.time} ${st.playlist}`);
      chip.addEventListener('dragstart', ev => ev.dataTransfer.setData('text/plain', JSON.stringify({ block_id: st.block_id, day: sl.day, minute_of_day: sl.minute_of_day })));
      c.append(chip);
    }
  }
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Week', helpLink('schedule', 'week')), grid,
    week.starts.length ? null : h('div', { class: 'dim' }, 'No playlist has a scheduled start yet. Add one below.')));
  // ---- scheduled starts
  const lists = (await J('/api/v1/playlists')).playlists;
  const startCard = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Scheduled starts', helpLink('schedule', 'scheduled-starts')));
  for (const st of week.starts) {
    const { e } = await startBlockOf(st.playlist_id, st.block_id);
    // One row per distinct time of the start: its time and the days it applies to. A start made by dragging
    // in the week grid has several; Save writes every row back (one recurrence each).
    const times = h('div'), addRow = (minute, dayList) => {
      const row = h('div', { class: 'row trow', style: 'margin-top:6px' }, h('input', { type: 'time', value: hm(minute) }),
        ...DAY_ORDER.map(d => h('label', { class: 'dayck' }, h('input', { type: 'checkbox', 'data-d': d, checked: dayList.includes(d) }), DAYS[d])),
        h('button', { title: 'Remove this time', onclick: () => row.remove() }, 'Remove time'));
      times.append(row);
    };
    const byMinute = new Map();
    for (const sl of st.slots) { if (!byMinute.has(sl.minute_of_day)) byMinute.set(sl.minute_of_day, []); byMinute.get(sl.minute_of_day).push(sl.day); }
    [...byMinute.entries()].sort((a, b) => a[0] - b[0]).forEach(([m, d]) => addRow(m, d));
    if (!byMinute.size) addRow(18 * 60, []);
    const how = h('select', {}, ['segue', 'now', 'end'].map(x => h('option', { value: x, selected: st.how === x }, x)));
    const anchor = h('select', {}, [['absolute', 'starts itself'], ['manual', 'expected, started by hand']].map(([v, l]) => h('option', { value: v, selected: st.anchor === v }, l)));
    const late = h('input', { type: 'number', value: st.window_late_s, min: 0, style: 'width:5em' });
    const skip = h('input', { value: (st.skip_dates || []).join(', '), placeholder: 'skip dates, YYYY-MM-DD' });
    const once = h('input', { type: 'datetime-local' });
    startCard.append(h('div', { class: 'card startedit', style: 'margin-top:8px;background:var(--panel2)' },
      h('div', { class: 'row' }, h('b', {}, st.playlist), h('span', { class: 'chip' }, st.anchor), h('span', { class: 'dim' }, `late window ${st.window_late_s} s`)),
      times, h('div', { class: 'row', style: 'margin-top:6px' }, h('button', { onclick: () => addRow(18 * 60, []) }, 'Add a time')),
      h('div', { class: 'row', style: 'margin-top:6px' }, h('span', { class: 'dim' }, 'How'), how, anchor, h('span', { class: 'dim' }, 'may start up to'), late, h('span', { class: 'dim' }, 's late')),
      h('div', { class: 'row', style: 'margin-top:6px' }, skip, h('span', { class: 'dim' }, 'one-off:'), once, h('span', { class: 'dim' }, (st.dates || []).join(', '))),
      h('div', { class: 'row', style: 'margin-top:8px' }, h('button', { class: 'primary', onclick: async () => {
        const schedule = [];
        for (const row of times.children) {
          const ds = [...row.querySelectorAll('input[type=checkbox]')].filter(c => c.checked).map(c => +c.dataset.d);
          if (!row.querySelector('input[type=time]').value) { toast('Every time row needs a time', true); return; }
          const [hh, mm] = row.querySelector('input[type=time]').value.split(':').map(Number);
          if (ds.length) schedule.push({ cron: `${mm} ${hh} * * ${ds.length === 7 ? '*' : ds.join(',')}` });
        }
        for (const d of st.dates || []) schedule.push({ date: d });
        if (once.value) schedule.push({ date: once.value.length === 16 ? once.value + ':00' : once.value });
        const params = { ...e.block.params, schedule, how: how.value, anchor: anchor.value, window_late_s: +late.value, skip_dates: skip.value.split(',').map(x => x.trim()).filter(Boolean) };
        delete params.mode;
        const r = await safe(jsend(`/api/v1/playlists/${st.playlist_id}/entries/${e.id}`, { params }, 'PATCH'), 'Save'); if (r) { toast('Saved'); route(); } } }, 'Save'),
        h('button', { class: 'danger', onclick: async () => { if (confirm('Remove the scheduled start of ' + st.playlist + '?')) { await safe(J(`/api/v1/playlists/${st.playlist_id}/entries/${e.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } } }, 'Remove'))));
  }
  // add
  const addPl = h('select', {}, lists.map(l => h('option', { value: l.id }, l.name)));
  const addDays = DAY_ORDER.map(d => h('label', { class: 'dayck' }, h('input', { type: 'checkbox', 'data-d': d, checked: d >= 1 && d <= 5 }), DAYS[d]));
  const addTime = h('input', { type: 'time', value: '18:00' });
  const addHow = h('select', {}, ['segue', 'now', 'end'].map(x => h('option', { value: x }, x)));
  startCard.append(h('div', { class: 'row', style: 'margin-top:14px' }, h('b', {}, 'Add a start'), addPl, addDays, addTime, addHow, h('button', { onclick: async () => {
    const ds = addDays.filter(l => l.firstChild.checked).map(l => +l.firstChild.dataset.d); if (!ds.length) { toast('Pick at least one day', true); return; }
    const [hh, mm] = addTime.value.split(':').map(Number);
    const r = await safe(jsend(`/api/v1/playlists/${addPl.value}/entries`, { block: { kind: 'start', name: 'start', trigger: 'first', params: { schedule: [{ cron: `${mm} ${hh} * * ${ds.length === 7 ? '*' : ds.join(',')}` }], anchor: 'absolute', how: addHow.value, window_late_s: 300 } } }), 'Add');
    if (r) { toast('Added'); route(); } } }, 'Add')));
  root.append(startCard);
  // ---- station blocks
  const cats = (await J('/api/v1/categories')).categories;
  const blocks = (await J(`/api/v1/stations/${S.station}/blocks`)).blocks;
  const sbCard = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Station blocks', helpLink('schedule', 'station-blocks')),
    h('div', { class: 'dim' }, 'These apply under every program (a playlist can turn them off while it runs). The top-of-hour ID plays at the first segue between a minute before the hour and half a minute after; if none comes it cuts in at the end of the window.'));
  for (const b of blocks.filter(x => x.params && x.params.when)) {
    const w = b.params.when, cron = w.kind === 'recurrence' ? w.cron : '';
    const nm = h('input', { value: b.name }), cr = h('input', { value: cron, style: 'width:9em', title: 'minute hour day month weekday' });
    const early = h('input', { type: 'number', value: b.params.window_early_s ?? 60, style: 'width:5em', min: 0 }), late = h('input', { type: 'number', value: b.params.window_late_s ?? 30, style: 'width:5em', min: 0 });
    const mode = h('select', {}, ['soft_then_hard', 'soft', 'hard', 'hard_wait'].map(x => h('option', { value: x, selected: (b.params.mode || 'soft_then_hard') === x }, x)));
    const en = h('input', { type: 'checkbox', checked: b.enabled });
    sbCard.append(h('div', { class: 'qrow', style: 'flex-wrap:wrap' }, h('span', { class: 'chip' }, b.kind), nm, cr, h('span', { class: 'dim' }, 'from'), early, h('span', { class: 'dim' }, 's before to'), late, h('span', { class: 'dim' }, 's after'), mode, h('label', {}, en, ' on'),
      h('button', { onclick: async () => {
        const params = { ...b.params, when: { ...w, kind: 'recurrence', cron: cr.value }, window_early_s: +early.value, window_late_s: +late.value, mode: mode.value };
        const r = await safe(jsend(`/api/v1/stations/${S.station}/blocks/${b.id}`, { name: nm.value, params, enabled: en.checked }, 'PATCH'), 'Save'); if (r) { toast('Saved'); route(); } } }, 'Save'),
      h('button', { onclick: async () => { await safe(J(`/api/v1/stations/${S.station}/blocks/${b.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } }, 'Remove')));
  }
  if (!blocks.some(x => x.params && x.params.when)) sbCard.append(h('div', { class: 'dim', style: 'margin-top:8px' }, 'No timed station block yet.'));
  const idCat = h('select', {}, cats.map(c => h('option', { value: c.id, selected: c.name === 'id' }, c.name)));
  const idCron = h('input', { value: '0 * * * *', style: 'width:9em' });
  sbCard.append(h('div', { class: 'row', style: 'margin-top:12px' }, h('b', {}, 'Add an ID'), idCat, h('span', { class: 'dim' }, 'at'), idCron, h('span', { class: 'dim' }, '(every hour on the hour)'), h('button', { onclick: async () => {
    const r = await safe(jsend(`/api/v1/stations/${S.station}/blocks`, { kind: 'insert', name: 'Top of the hour', trigger: 'timed', params: { source: { category_id: +idCat.value }, when: { kind: 'recurrence', cron: idCron.value }, mode: 'soft_then_hard', window_early_s: 60, window_late_s: 30 } }), 'Add');
    if (r) { toast('Added'); route(); } } }, 'Add')));
  root.append(sbCard);
  // ---- one-shot change
  const up = await J(stApi('schedule/upcoming?hours=48'));
  const oneCard = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'One-shot change', helpLink('schedule', 'one-shot-change')));
  const osPl = h('select', {}, lists.map(l => h('option', { value: l.id }, l.name))), osAt = h('input', { type: 'datetime-local' }), osHow = h('select', {}, ['segue', 'now', 'end'].map(x => h('option', { value: x }, x)));
  oneCard.append(h('div', { class: 'row' }, 'Switch to', osPl, 'at', osAt, osHow, h('button', { onclick: async () => {
    if (!osAt.value) { toast('Pick a time', true); return; }
    const r = await safe(jsend(stApi('schedule/changes'), { playlist_id: +osPl.value, at: new Date(osAt.value).toISOString(), how: osHow.value }), 'Schedule change'); if (r) { toast('Scheduled'); route(); } } }, 'Schedule')));
  for (const u of up.events.filter(x => x.origin === 'oneshot')) oneCard.append(h('div', { class: 'qrow' }, h('div', { class: 'meta' }, h('div', {}, u.name), h('div', { class: 'dim' }, new Date(u.target).toLocaleString())),
    h('button', { onclick: async () => { await safe(J(stApi('schedule/changes/' + (-u.block_id)), { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } }, 'Cancel')));
  root.append(oneCard);
  // ---- what fires next
  const nx = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'What fires next', helpLink('schedule', 'what-fires-next')),
    up.events.length ? h('table', {}, h('thead', {}, h('tr', {}, ['Block', 'Type', 'Target', 'Expected', 'Window', 'Mode', 'Planned'].map(x => h('th', {}, x)))),
      h('tbody', {}, up.events.slice(0, 40).map(u => h('tr', {}, h('td', {}, u.name), h('td', {}, u.type), h('td', { class: 'mono' }, new Date(u.target).toLocaleString()), h('td', { class: 'mono' }, new Date(u.predicted_at).toLocaleTimeString()),
        h('td', { class: 'mono' }, new Date(u.window.from).toLocaleTimeString() + ' to ' + new Date(u.window.to).toLocaleTimeString()), h('td', {}, u.mode), h('td', {}, u.planned ? 'yes' : '')))))
      : h('div', { class: 'dim' }, 'Nothing is due in the next 48 hours.'));
  root.append(nx);
  // ---- recent firings
  const log = await J(stApi('blocks/log?from=' + new Date(Date.now() - 48 * 3600e3).toISOString()));
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Recent firings', helpLink('schedule', 'recent-firings')),
    log.rows.length ? h('table', {}, h('thead', {}, h('tr', {}, ['Block', 'Target', 'Fired', 'How', 'Deviation'].map(x => h('th', {}, x)))),
      h('tbody', {}, log.rows.slice(-60).reverse().map(r => h('tr', {}, h('td', {}, r.name), h('td', { class: 'mono' }, new Date(r.target).toLocaleString()), h('td', { class: 'mono' }, r.fired_at ? new Date(r.fired_at).toLocaleTimeString() : ''), h('td', {}, r.how), h('td', { class: 'mono' }, r.deviation_ms != null ? (r.deviation_ms / 1000).toFixed(1) + ' s' : ''))))) : h('div', { class: 'dim' }, 'Nothing has fired yet.')));
  return root;
};

// ================= Insert rules =================
routes.insert = async () => {
  const root = h('div'); root.append(h('h1', {}, 'Insert rules', helpLink('insert')), h('div', { class: 'dim' }, 'A standing rule plays an item from a category after every N songs. Rules can belong to one playlist or to the whole station.'));
  const lists = (await J('/api/v1/playlists')).playlists;
  const card = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Playlist rules', helpLink('insert', 'playlist-rules')));
  for (const l of lists) {
    const p = await J('/api/v1/playlists/' + l.id);
    for (const e of p.entries.filter(x => x.type === 'block' && x.block.kind === 'insert_every')) {
      card.append(h('div', { class: 'qrow' }, h('div', { class: 'meta' }, h('div', {}, e.block.name), h('div', { class: 'dim' }, `in ${p.name}: every ${e.block.params.n} songs from ${JSON.stringify(e.block.params.source)}, at least ${e.block.params.min_gap_minutes} min apart`)),
        h('button', { onclick: async () => { await safe(J(`/api/v1/playlists/${l.id}/entries/${e.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } }, 'Remove')));
    }
  }
  if (card.children.length === 1) card.append(h('div', { class: 'dim' }, 'No playlist has an insert rule.'));
  root.append(card);
  const sc = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Station rules', helpLink('insert', 'station-rules')));
  if (S.station) {
    const blocks = (await J(`/api/v1/stations/${S.station}/blocks`)).blocks;
    sc.append(...blocks.map(b => h('div', { class: 'qrow' }, h('div', { class: 'meta' }, h('div', {}, b.name), h('div', { class: 'dim' }, b.kind + ' ' + JSON.stringify(b.params))), h('button', { onclick: async () => { await safe(J(`/api/v1/stations/${S.station}/blocks/${b.id}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } }, 'Remove'))));
    const n = h('input', { type: 'number', value: 3, min: 1 }), cat = h('select', {}), nm = h('input', { value: 'ID every 3' });
    (await J('/api/v1/categories')).categories.forEach(c => cat.append(h('option', { value: c.name, selected: c.name === 'id' }, c.name)));
    sc.append(h('div', { class: 'row', style: 'margin-top:10px' }, nm, cat, h('span', { class: 'dim' }, 'every'), n, h('span', { class: 'dim' }, 'songs'), h('button', { onclick: async () => { const r = await safe(jsend(`/api/v1/stations/${S.station}/blocks`, { kind: 'insert_every', name: nm.value, n: +n.value, source: { category: cat.value } }), 'Add'); if (r) route(); } }, 'Add station rule')));
  }
  root.append(sc);
  return root;
};

// ================= History =================
routes.history = async () => {
  const root = h('div'); root.append(h('h1', {}, 'History', helpLink('history')), pickStation(() => route()));
  if (!S.station) return root;
  const r = await J(stApi('history?limit=200'));
  root.append(h('div', { class: 'row', style: 'margin-bottom:10px' }, h('a', { href: stApi('history.csv'), download: 'as-run.csv' }, 'Export CSV')));
  root.append(h('table', {}, h('thead', {}, h('tr', {}, ['Started', 'Title', 'Artist', 'Category', 'Origin', 'Ended by', 'Played', 'Deviation'].map(x => h('th', {}, x)))),
    h('tbody', {}, r.rows.map(x => h('tr', {}, h('td', { class: 'mono' }, new Date(x.started_at).toLocaleString()), h('td', {}, x.title), h('td', {}, x.artist), h('td', {}, x.category), h('td', {}, x.origin), h('td', {}, x.end_reason || 'on air'), h('td', { class: 'mono' }, x.played_ms != null ? mmss(x.played_ms) : ''), h('td', { class: 'mono' }, x.deviation_ms != null ? (x.deviation_ms / 1000).toFixed(2) + ' s' : ''))))));
  return root;
};

// ================= Notifications =================
routes.notify = async () => {
  const root = h('div'); root.append(h('h1', {}, 'Notifications', helpLink('notify')));
  const n = await J('/api/v1/notify');
  const ch = h('div', { class: 'card' }, h('h3', {}, 'Email channels', helpLink('notify', 'email-channels')));
  const chanForm = c => {
    const cfg = c?.config || {};
    const i = { name: h('input', { value: c?.name || 'Email' }), host: h('input', { value: cfg.host || '' }), port: h('input', { type: 'number', value: cfg.port || 587 }),
      tls: h('select', {}, ['starttls', 'tls', 'none'].map(v => h('option', { value: v, selected: (cfg.tls || 'starttls') === v }, v))),
      username: h('input', { value: cfg.username || '' }), password: h('input', { type: 'password', placeholder: c?.password_set ? 'set (leave empty to keep)' : 'not set' }),
      from: h('input', { value: cfg.from || '' }), to: h('input', { value: (cfg.to || []).join(', ') }), enabled: h('input', { type: 'checkbox', checked: c ? c.enabled : true }) };
    const out = h('div', { class: 'dim', style: 'margin-top:6px' }, c?.last_test_at ? `Last test ${c.last_test_at}: ${c.last_test_result}` : '');
    return h('div', { class: 'card', style: 'margin-top:10px;background:var(--panel2)' }, h('div', { class: 'form' },
      h('label', {}, 'Name'), i.name, h('label', {}, 'Server'), h('div', { class: 'row' }, i.host, h('span', { class: 'dim' }, 'port'), i.port, i.tls), h('label', {}, 'Username'), i.username, h('label', {}, 'Password'), i.password, h('label', {}, 'From'), i.from, h('label', {}, 'To (comma separated)'), i.to, h('label', {}, 'Enabled'), i.enabled),
      h('div', { class: 'row', style: 'margin-top:10px' }, h('button', { class: 'primary', onclick: async () => {
        const body = { name: i.name.value, enabled: i.enabled.checked, config: { host: i.host.value, port: +i.port.value, tls: i.tls.value, username: i.username.value, from: i.from.value, to: i.to.value.split(',').map(s => s.trim()).filter(Boolean) } };
        if (i.password.value) body.password = i.password.value;
        const r = await safe(jsend(c ? `/api/v1/notify/channels/${c.id}` : '/api/v1/notify/channels', body, c ? 'PUT' : 'POST'), 'Save'); if (r) { toast('Saved'); route(); } } }, 'Save channel'),
        c ? h('button', { onclick: async () => { out.textContent = 'Sending'; const r = await safe(jsend(`/api/v1/notify/channels/${c.id}/test`, {}), 'Test'); if (r) { out.textContent = r.ok ? 'Server said: ' + r.reply : 'Failed: ' + r.error; out.style.color = r.ok ? 'var(--green)' : 'var(--red)'; } } }, 'Send test') : null,
        c ? h('button', { class: 'danger', onclick: async () => { if (confirm('Delete this channel?')) { await safe(J('/api/v1/notify/channels/' + c.id, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } })); route(); } } }, 'Delete') : null), out);
  };
  ch.append(...n.channels.map(chanForm), chanForm(null));
  const rules = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'When something happens', helpLink('notify', 'rules')));
  const digestOut = h('span', { class: 'dim' });
  const sendDigest = bucket => async () => { const r = await safe(jsend('/api/v1/notify/digest', { bucket }), 'Digest'); if (r) digestOut.textContent = r.events ? (r.sent ? `Sent ${r.events} event(s).` : `${r.events} event(s) waiting; the send failed, see Recent messages.`) : 'Nothing is waiting.'; };
  rules.append(h('div', { class: 'row' }, h('button', { onclick: sendDigest('hourly_digest') }, 'Send the hourly digest now'), h('button', { onclick: sendDigest('daily_digest') }, 'Send the daily digest now'), digestOut),
    h('table', {}, h('thead', {}, h('tr', {}, ['Event', 'On', 'Severity', 'Channels', 'Delivery', 'Limit (min)', 'Send on clear'].map(x => h('th', {}, x)))), h('tbody', {}, n.rules.map(r => {
    const en = h('input', { type: 'checkbox', checked: r.enabled }), del = h('select', {}, ['immediate', 'hourly_digest', 'daily_digest', 'off'].map(v => h('option', { value: v, selected: r.delivery === v }, v))), lim = h('input', { type: 'number', value: r.rate_limit_min, style: 'width:5em' }), clr = h('input', { type: 'checkbox', checked: r.notify_on_clear });
    const sev = h('select', {}, h('option', { value: '' }, 'as raised'), ['info', 'warning', 'critical'].map(v => h('option', { value: v, selected: r.severity === v }, v)));
    const chs = h('select', { multiple: true, size: Math.min(3, Math.max(1, n.channels.length)), title: 'none selected means every enabled channel' }, n.channels.map(c => h('option', { value: c.id, selected: (r.channels || []).includes(c.id) }, c.name)));
    const save = () => safe(jsend('/api/v1/notify/rules/' + r.code, { enabled: en.checked, severity: sev.value || null, channels: [...chs.selectedOptions].map(o => +o.value), delivery: del.value, rate_limit_min: +lim.value, notify_on_clear: clr.checked }, 'PUT'), 'Save rule');
    [en, sev, chs, del, lim, clr].forEach(x => x.addEventListener('change', save));
    return h('tr', {}, h('td', { class: 'mono' }, r.code), h('td', {}, en), h('td', {}, sev), h('td', {}, chs), h('td', {}, del), h('td', {}, lim), h('td', {}, clr));
  }))));
  const log = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Recent messages', helpLink('notify', 'recent-messages')), ...(n.log.length ? n.log.map(l => h('div', { class: 'row', style: 'font-size:13px' }, h('span', { class: 'mono dim' }, l.at), h('span', { class: 'chip' }, l.kind), l.code, h('span', { class: 'dim' }, l.result))) : [h('div', { class: 'dim' }, 'Nothing sent yet')]));
  root.append(ch, rules, log);
  return root;
};

// ================= Loudness controller (architecture 6.1) =================
// One effect of the broadcast chain, with its presets the way a plug-in has them: shipped ones (read only),
// the user's (Save as, Update, Rename, Delete, Export, Import), and a "modified" mark when the settings no
// longer match the selected preset. Every setting is here, with a line saying what it changes in the sound.
const LOUD_FIELDS = [
  ['target_lufs', 'Target, LUFS', 0.5, -24, -10, 'The loudness the stream is steered to; YouTube and Twitch want about -14. Lower is quieter.'],
  ['true_peak_dbtp', 'True-peak ceiling, dBTP', 0.5, -3, -0.5, 'The highest peak that leaves the station, measured between samples; the limiter holds it. Lower leaves room for lossy encoders.'],
  ['window_s', 'Window, s', 30, 60, 900, 'How much recent program the level is judged over. Longer reacts later and more smoothly.'],
  ['max_slew_db_per_min', 'Fastest move, dB per minute', 0.25, 0.25, 3, 'How fast the volume may change. Low is a slow hand on the fader; high follows the music closely.'],
  ['deadband_lu', 'Dead band, LU', 0.05, 0, 1, 'The gain does not move while it is this close to where it should be. Larger means fewer small corrections.'],
  ['gain_min_db', 'Lowest gain, dB', 1, -24, 0, 'The most the controller will turn the program down.'],
  ['gain_max_db', 'Highest gain, dB', 1, 0, 18, 'The most the controller will turn the program up; at the limit it raises a warning after 10 minutes.'],
  ['min_valid_s', 'Program needed, s', 5, 1, 900, 'Seconds of measurable program (not silence, not a gated category) before the gain moves at all.'],
  ['trim_tau_s', 'Trim time constant, s', 30, 60, 3600, 'How slowly the final correction (what the limiter takes off) is learned. Longer is steadier.'],
  ['trim_range_db', 'Trim range, dB', 0.5, 0, 6, 'The most that correction may add or take off.'],
  ['robust_items', 'Items compared', 1, 1, 20, 'With "hold": the program level is the median of this many latest items, so one odd item (a very quiet track) barely moves the volume, while several in a row do. 1 follows each item.'],
  ['item_cap_s', 'Longest an item counts, s', 30, 30, 1800, 'With "hold": a long item (a 20-minute ambient piece) counts for at most this long against the others.'],
  ['item_memory_s', 'Item memory, s', 30, 60, 3600, 'With "hold": how long an item remembers its own loudness; shorter re-bases sooner when a track really changes level.'],
  ['max_limiter_gr_db', 'Most limiting, dB', 0.5, 0, 24, 'The gain is held down so the loudest peak of the window never needs more than this much limiting. Low keeps the sound open; 24 turns the cap off and lets the limiter work.'],
  ['peak_allowance_db', 'Peak allowance, dB', 0.5, 0, 12, 'How far normalization may push a track\'s peaks into the limiter while the controller is on. 0 keeps the limiter out of the way; more is louder and denser.'],
];
const LOUD_SELECTS = { within_item: ['Within an item', [['hold', 'hold: judge each item as a whole'], ['follow', 'follow: track the loudness continuously']], 'Hold leaves a quiet passage inside a track alone; follow lifts it. Hold keeps the dynamics, follow evens them out.'] };
function sameParams(a, b) {
  const keys = new Set([...Object.keys(a || {}), ...Object.keys(b || {})]);
  for (const k of keys) {
    const x = a?.[k], y = b?.[k];
    if (Array.isArray(x) || Array.isArray(y)) { if (JSON.stringify([...(x || [])].sort()) !== JSON.stringify([...(y || [])].sort())) return false; }
    else if (typeof x === 'number' && typeof y === 'number') { if (Math.abs(x - y) > 1e-9) return false; }
    else if (x !== y) return false;
  }
  return true;
}
async function loudnessCard(s) {
  const url = `/api/v1/stations/${s.id}/loudness`;
  const pUrl = '/api/v1/processing-presets/loudness';
  const card = h('div', { class: 'pcard loud has-adv', 'data-block': 'loudness' });
  let hist = null;                 // the gain history lives across repaints of the card
  let histSpan = 900;              // seconds shown: 60 or 900
  let refs = null;                 // the live elements of the current paint
  const gains = [];                // [t, gain] for the last 90 s: the rate of the gain's move
  let ctl = null, ctlAt = 0;       // the controller's own numbers (valid program seconds, target gain), polled every 2 s
  let cats = [];
  try { cats = (await J('/api/v1/categories')).categories.map(c => c.name); } catch { }
  // ---- live: one call per meter frame (about 10 a second) from the Processing page ----
  // The display follows the Level Rider dock: a state pill and the gain in numbers, the loudness bars against the target,
  // the gain bar centred on 0 with the gain it is heading for, the gain history with a state strip, and one line of
  // limiter, trim and parameters. The words under it say what the controller is doing and why.
  const distClass = d => d == null ? '' : d < 0.5 ? 'ok' : d < 1.5 ? 'warn' : 'bad';
  const signedDb = x => (x > 0 ? '+' : '') + x.toFixed(1);
  const stateOf = f => {
    const { cfg, j } = refs;
    const st = f ? ['off', 'converging', 'on_target', 'frozen', 'at_limit'][f.lstate] : j.state;
    if (!cfg.enabled || st === 'off') return 'off';
    if (st === 'frozen') { const have = ctl ? ctl.valid_s : null; return have != null && have < cfg.min_valid_s ? 'acquiring' : 'holding'; }
    return st;   // converging, on_target, at_limit
  };
  const PILL = { off: ['OFF', ''], acquiring: ['ACQUIRING', 'info'], holding: ['HOLDING', 'dim'], converging: ['RIDING', 'ok'], on_target: ['ON TARGET', 'ok'], at_limit: ['AT LIMIT', 'warn'] };
  const STRIP = { off: null, acquiring: '#4f7fe0', holding: '#4a566c', converging: '#2f9e6e', on_target: '#2f9e6e', at_limit: '#f5a623' };
  const statusText = (f, rate) => {
    const { cfg, j } = refs;
    const g = f ? f.gain : j.gain_db, w = f ? f.window : j.window, tg = f && f.target != null ? f.target : (j.target ?? cfg.target_lufs);
    const st = stateOf(f);
    const err = w != null && tg != null ? tg - w : null;
    const under = e => Math.abs(e).toFixed(1) + ' LU ' + (e > 0 ? 'under' : 'over') + ' target';
    const r = rate == null ? '' : `, moving ${signedDb(rate)} dB/min`;
    if (st === 'off') return 'Off: the controller is not changing the level.';
    if (st === 'acquiring') return `Warming up: ${Math.round(ctl.valid_s)} s of program measured, needs ${cfg.min_valid_s} s.`;
    if (st === 'holding') return `Frozen: silence or gated content, so the gain holds at ${signedDb(g)} dB.`;
    if (st === 'at_limit') return `At limit: the gain is held at ${signedDb(g)} dB (its range is ${cfg.gain_min_db} to ${cfg.gain_max_db} dB)` + (err != null ? `; the program is ${under(err)}.` : '.');
    if (st === 'converging') {
      const up = ctl && ctl.target_gain_db != null ? ctl.target_gain_db > g : (err || 0) > 0;
      return `${up ? 'Raising' : 'Lowering'}: ` + (err != null ? `program ${under(err)}` : 'the gain is moving') + r + '.';
    }
    return `Holding: on target` + (w != null ? ` (W ${w.toFixed(1)} against ${tg})` : '') + `, gain ${signedDb(g)} dB.`;
  };
  function paintLive(f) {
    if (!refs) return;
    const { bars, gainM, statusEl, pill, big, head, limLine, cfg, j } = refs;
    const tg = f && f.target != null ? f.target : (j.target ?? cfg.target_lufs);
    const g = f ? f.gain : j.gain_db;
    const vals = { in: ctl ? ctl.window_in : null, S: f ? f.shortTerm : j.short_term, W: f ? f.window : j.window, I: f ? f.integrated : j.integrated };
    for (const [k, m] of Object.entries(bars)) {
      m.set(vals[k]);
      const d = k === 'S' || vals[k] == null || tg == null ? null : Math.abs(vals[k] - tg);
      m.el.querySelector('.pv').dataset.dist = cfg.enabled ? distClass(d) : '';
    }
    if (f) gainM.set(cfg.enabled ? f.gain : null);
    const want = cfg.enabled && ctl && ctl.target_gain_db != null ? ctl.target_gain_db : null;
    gainM.heading(want);
    const st = stateOf(f);
    setText(pill.label, PILL[st][0]); pill.className = 'chip lr-pill fixw ' + PILL[st][1];
    setText(big, cfg.enabled && g != null ? signedDb(g) + ' dB' : '--');
    setText(head, want != null && cfg.enabled ? '\u2192 ' + signedDb(want) : '');
    let rate = null;
    if (f && gains.length > 1) { const [t0, g0] = gains[0], [t1, g1] = gains[gains.length - 1]; if (t1 - t0 >= 20000) rate = (g1 - g0) / ((t1 - t0) / 60000); }
    const pct = lim.length ? Math.round(100 * lim.filter(Boolean).length / lim.length) : 0;
    setText(limLine, `GR ${limNow.toFixed(1)} dB \u00b7 trim ${ctl ? signedDb(ctl.trim_db) : '--'} dB \u00b7 ${pct} % limiting`);
    setText(statusEl, statusText(f, rate));
  }
  let limNow = 0; const lim = [];
  card._status = (f, rate) => statusText(f, rate);
  card._live = f => {
    const t = performance.now();
    const on = refs && refs.cfg.enabled;
    if (on) {
      hist.push({ gr: f.gain, strip: STRIP[stateOf(f)] });
      gains.push([t, f.gain]); while (gains.length && t - gains[0][0] > 90000) gains.shift();
      const gr = Math.abs(f.stages[3].gr || 0); limNow = gr; lim.push(gr > 0.1); while (lim.length > 600) lim.shift();
    } else if (hist) hist.push({ gr: null, strip: null });
    if (t - ctlAt > 2000) { ctlAt = t; J(url).then(r => { ctl = r.controller; if (refs) refs.j = r; }).catch(() => { }); }
    paintLive(f);
  };
  let sendTimer = null, inflight = false;   // an edit waiting to be sent, or on its way: the card must not be rebuilt then
  const paint = async (periodic) => {
    let j, pres;
    if (periodic && (sendTimer || inflight)) return;
    try { [j, pres] = await Promise.all([J(url), J(pUrl)]); } catch (e) { card.replaceChildren(h('h3', {}, 'Loudness controller, ' + s.display_name), h('div', { class: 'notice bad' }, e.message)); return; }
    if (periodic && (sendTimer || inflight)) return;   // an edit was made while the reply was on its way
    const cfg = j.settings, c = j.controller, items = pres.items;
    ctl = c;
    const selName = cfg.user_preset || (cfg.preset !== 'custom' ? cfg.preset : '');
    const sel = items.find(p => p.name === selName);
    const en = h('input', { type: 'checkbox', checked: cfg.enabled, 'data-k': 'enabled', class: 'sr-only', 'aria-label': 'Loudness controller on' });
    const power = h('label', { class: 'pwr' + (cfg.enabled ? ' on' : ''), title: cfg.enabled ? 'On. Click to switch the loudness controller off (saves at once).' : 'Off. Click to switch it on (saves at once).' }, en, procPowerIcon(), h('span', { class: 'pwr-t' }, cfg.enabled ? 'ON' : 'OFF'));
    // a knob in the style of the Processing cards; the card reads and writes its value through `.value` like an input
    const num = ([k, label, step, min, max, tip], small) => {
      const [name, unit] = label.split(/,\s*(.*)/);
      const w = PW.knob({ min, max, step, int: step >= 1 && Number.isInteger(min) && Number.isInteger(max) && step === 1, log: false, unit: unit || '', label: name === 'True-peak ceiling' ? 'TP ceiling' : name, help: tip, value: cfg[k], key: k, small: small === true, onChange: () => w.el.dispatchEvent(new Event('input', { bubbles: true })) });
      return w.el;
    };
    const selects = Object.entries(LOUD_SELECTS).map(([k, [, opts]]) => h('select', { 'data-k': k }, opts.map(([v, l]) => h('option', { value: v, selected: cfg[k] === v }, l))));
    const allCats = [...new Set([...cats, ...cfg.gate_categories])];
    const gate = h('select', { multiple: true, size: Math.min(8, Math.max(3, allCats.length)), 'data-k': 'gate_categories', style: 'min-width:180px' }, allCats.map(n => h('option', { value: n, selected: cfg.gate_categories.includes(n) }, n)));
    const read = () => {
      const o = {};
      card.querySelectorAll('[data-k]').forEach(e => {
        const k = e.dataset.k;
        o[k] = e.type === 'checkbox' ? e.checked : e.multiple ? [...e.selectedOptions].map(x => x.value) : e.tagName === 'SELECT' ? e.value : parseFloat(e.value);
      });
      return o;
    };
    const settingsOnly = () => { const o = read(); delete o.enabled; return o; };
    const modChip = h('span', { class: 'chip warn', id: 'loud-modified', title: 'The settings below differ from the selected preset' }, 'modified');
    const check = () => { modChip.style.display = sel && !sameParams(sel.params, settingsOnly()) ? '' : 'none'; };
    // Every setting applies as it is changed, like the other blocks: a change is sent at most four times a second while it
    // goes on (the last one always follows), the station applies it and writes its config. Nothing waits for a Save.
    const sendNow = async () => {
      sendTimer = null;
      const m = sel && !sameParams(sel.params, settingsOnly());
      inflight = true;
      const r = await safe(jsend(url, { ...read(), preset: cfg.preset === 'custom' || m ? 'custom' : cfg.preset }, 'PUT'), 'Not applied');
      inflight = false;
      if (!r) { if (!sendTimer) paint(); return; }   // refused: show what the station really has
      Object.assign(cfg, r.settings); ctl = r.controller; refs.j = r;
      setText(foot, footText(cfg));
      setText(applied, 'applied');
      setTimeout(() => setText(applied, ''), 1500);
    };
    const edited = ev => {
      check();
      if (ev && ev.target && ev.target.dataset && ev.target.dataset.k === 'enabled') return;   // the power switch sends itself
      if (!sendTimer) sendTimer = setTimeout(sendNow, 250);
    };
    card.oninput = edited; card.onchange = edited;
    const modified = sel && !sameParams(sel.params, settingsOnly());
    const stateChip = h('span', { class: 'chip ' + ({ on_target: 'ok', converging: '', frozen: 'warn', at_limit: 'warn', off: '' })[j.state], title: 'The controller\'s state' }, (j.state || 'off').replace('_', ' '));
    const fmt = v => v == null ? '--' : v.toFixed(1);
    // The tip under the card (neutral, not a warning): the controller's gain has stood far from 0 dB for minutes, so the
    // chain's makeup could do its work. "Move to makeup" shifts the gain into the chain's output makeup in one gliding step.
    const mk = j.makeup;
    const signed = x => (x > 0 ? '+' : '') + x.toFixed(1);
    const moveToMakeup = async () => {
      const secs = Math.max(1, Math.round(mk.glide_s));
      const ok = confirm(`Move ${signed(mk.move_db)} dB from the loudness controller into the chain's makeup?\n\n`
        + `Output makeup (before the clipper): ${mk.makeup_db.toFixed(1)} dB  ->  ${mk.new_makeup_db.toFixed(1)} dB\n`
        + `Loudness controller gain: ${signed(mk.gain_db)} dB  ->  about ${signed(mk.gain_db - mk.move_db)} dB\n\n`
        + `The two glide together over about ${secs} s, so the program level does not change. The chain then shows as modified, and the new makeup is saved to the station's config.`);
      if (!ok) return;
      const r = await safe(jsend(url + '/move-to-makeup', {}), 'Not moved');
      if (r) { toast(`Moved ${signed(r.moved_db)} dB into the makeup; gliding for about ${secs} s`); setTimeout(paint, 1500); }
    };
    const tipEl = h('div', { class: 'dim pnote ptip', 'data-loud-tip': '' }, j.note, ' ',
      mk && mk.available ? h('button', { 'data-move-makeup': '', onclick: moveToMakeup, title: 'Raise the chain\'s output makeup by the controller\'s gain and bring the gain back toward 0 dB, without a change in level' }, 'Move to makeup') : null);
    const save = async body => { clearTimeout(sendTimer); sendTimer = null; const r = await safe(jsend(url, body, 'PUT'), 'Not saved'); if (r) { toast('Saved'); paint(); } };
    const pick = h('select', { onchange: ev => {
      const p = items.find(x => x.name === ev.target.value);
      if (!p) return;
      save({ ...p.params, ...(p.builtin ? { preset: p.name, user_preset: '' } : { user_preset: p.name }) });
    } }, h('option', { value: '' }, selName ? '(choose a preset)' : (cfg.preset === 'custom' ? 'custom (no preset)' : '(choose a preset)')),
      h('optgroup', { label: 'Shipped' }, items.filter(p => p.builtin).map(p => h('option', { value: p.name, selected: p.name === selName }, p.name))),
      items.some(p => !p.builtin) ? h('optgroup', { label: 'Yours' }, items.filter(p => !p.builtin).map(p => h('option', { value: p.name, selected: p.name === selName }, p.name))) : null);
    const files = h('input', { type: 'file', accept: '.json,application/json', style: 'display:none', onchange: async ev => {
      const f = ev.target.files[0]; if (!f) return;
      let file; try { file = JSON.parse(await f.text()); } catch { toast('Not a preset file', true); return; }
      let r = await jsend(pUrl + '/import', { file }).catch(e => e);
      if (r instanceof Error && /already a preset/.test(r.message) && confirm(r.message + '. Replace it?')) r = await jsend(pUrl + '/import', { file, overwrite: true }).catch(e => e);
      if (r instanceof Error) { toast('Import failed: ' + r.message, true); return; }
      toast('Imported ' + r.name); paint();
    } });
    const askName = (msg, dflt) => { const n = prompt(msg, dflt || ''); return n && n.trim() ? n.trim() : null; };
    const user = sel && !sel.builtin;
    const help = t => h('div', { class: 'dim', style: 'font-size:12px;margin:0 0 4px 0' }, t);
    en.addEventListener('change', () => save({ ...read(), preset: cfg.preset === 'custom' || modified ? 'custom' : cfg.preset }));
    const main = LOUD_FIELDS.slice(0, 4), rest = LOUD_FIELDS.slice(4);
    // The faceplate of the other cards: nameplate (power, name; preset and Advanced), the main knobs, and the meter dock
    // at the right: short-term, window and integrated loudness (LUFS, the white line is the target) and the gain.
    const tgt = j.target ?? cfg.target_lufs;   // the target line is drawn whether or not the controller is on
    const mkBar = (k, l, v, t) => { const m = PW.level(l, tgt); m.set(v); m.el.title = t + '. The white line is the target.'; return [k, m]; };
    const bars = Object.fromEntries([
      mkBar('in', 'In', ctl ? ctl.window_in : null, 'Loudness of the program entering the gain, over the controller\'s window (what it judges), LUFS'),
      mkBar('S', 'S', j.short_term, 'Short-term loudness of the output (3 s), LUFS'),
      mkBar('W', 'W', j.window, 'The output\'s loudness over the controller\'s window, LUFS'),
      mkBar('I', 'I', j.integrated, 'Integrated loudness since the last reset, LUFS')]);
    const lv = Object.values(bars).map(m => m.el);
    const gainM = PW.gr('gain', Math.max(1, Math.ceil(Math.max(Math.abs(cfg.gain_min_db), Math.abs(cfg.gain_max_db)))), true); gainM.set(j.enabled && j.gain_db != null ? j.gain_db : null); gainM.el.title = 'The gain the controller applies now, dB; the amber tick is where it is heading';
    if (!hist) hist = PW.history({ kind: 'gain', seconds: 900, keep: 900, range: Math.max(1, Math.ceil(Math.max(Math.abs(cfg.gain_min_db), Math.abs(cfg.gain_max_db)))), label: 'Gain history', help: 'The gain the loudness controller applies: above the centre line it turns the program up, below it down. The scale is the controller\'s gain range. The strip along the bottom is its state: green riding or on target, blue warming up, grey holding, amber at a limit.' });
    const span = PW.segmented({ label: 'History', value: String(histSpan), options: [{ value: '60', label: '1 min' }, { value: '900', label: '15 min' }], onChange: v => { histSpan = Number(v); hist.setSeconds(histSpan); } });
    hist.setSeconds(histSpan);
    const pill = fixedChip(Object.values(PILL).map(p => p[0]), { class: 'chip lr-pill', 'data-loud-pill': '' });
    const big = h('span', { class: 'lr-big mono' }, '--'), head = h('span', { class: 'lr-head mono dim' }, '');
    const limLine = h('div', { class: 'lr-lim mono dim', 'data-loud-lim': '' }, '');
    const statusEl = h('div', { class: 'pstat', 'data-loud-status': '', role: 'status', 'aria-live': 'off' }, '');
    const footText = c => `win ${c.window_s} s \u00b7 ${c.within_item === 'hold' ? 'hold' : 'follow'} \u00b7 gain ${c.gain_min_db}/+${c.gain_max_db} dB \u00b7 slew ${c.max_slew_db_per_min} dB/min \u00b7 dead band ${c.deadband_lu} LU`;
    const foot = h('div', { class: 'lr-foot dim' }, footText(cfg));
    const applied = h('span', { class: 'chip ok', 'data-loud-applied': '', role: 'status' }, '');
    refs = { bars, gainM, statusEl, pill, big, head, limLine, cfg, j };
    paintLive(null);
    const dock = h('div', { class: 'pmeterc', title: `Loudness now: S ${fmt(j.short_term)}, W ${fmt(j.window)}, I ${fmt(j.integrated)} LUFS, target ${fmt(j.target)}` }, h('div', { class: 'pmeters-v' }, ...lv, gainM.el), h('div', { class: 'pflags' }, stateChip));
    const menu = h('details', { class: 'pmenu' }, h('summary', { title: 'Save, update, rename, delete, export and import presets' }, 'Presets'), h('div', { class: 'pmenu-body' },
        h('button', { onclick: async () => { const n = askName('Save the settings below as a preset named', user ? sel.name + ' copy' : (selName ? selName + ' copy' : '')); if (!n) return; const r = await safe(jsend(`${pUrl}/${encodeURIComponent(n)}`, { params: settingsOnly(), description: '' }, 'PUT'), 'Not saved'); if (r) { await save({ ...settingsOnly(), user_preset: n }); } } }, 'Save as…'),
        h('button', { disabled: !user, title: user ? 'Replace the selected preset with the settings below' : 'Shipped presets cannot be changed: use Save as', onclick: async () => { const r = await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}`, { params: settingsOnly(), description: sel.description }, 'PUT'), 'Not saved'); if (r) { toast('Updated ' + sel.name); paint(); } } }, 'Update'),
        h('button', { disabled: !user, onclick: async () => { const n = askName('Rename to', sel.name); if (!n || n === sel.name) return; const r = await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}/rename`, { to: n }), 'Not renamed'); if (r) { await save({ user_preset: n }); } } }, 'Rename…'),
        h('button', { disabled: !user, onclick: async () => { if (!confirm('Delete the preset "' + sel.name + '"?')) return; const r = await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}`, {}, 'DELETE'), 'Not deleted'); await save({ user_preset: '' }); } }, 'Delete'),
        h('a', { class: sel ? '' : 'dim', href: sel ? `${pUrl}/${encodeURIComponent(sel.name)}/export` : null, download: true }, h('button', { disabled: !sel }, 'Export')),
        h('button', { onclick: () => files.click() }, 'Import…'), files));
    pick.title = sel?.description || 'Presets';
    card.replaceChildren(...[h('div', { class: 'phead' }, h('span', { class: 'pidx', 'aria-hidden': 'true' }, card.dataset.idx || ''), power, h('div', { class: 'ptitle' }, h('span', { class: 'ptitle-t' }, 'Loudness controller'), helpLink('output-and-level', 'loudness-controller'))),
      h('div', { class: 'pprow' }, h('div', { class: 'row pbar' }, pick, modChip, menu)),
      h('div', { class: 'pbody' }, h('div', { class: 'pface' },
        h('div', { class: 'pfrow' }, h('div', { class: 'pctls pspread' }, ...main.map(f => num(f))), h('div', { class: 'phist-box pelastic' }, hist.el, span.el), dock),
        h('div', { class: 'lr-top' }, pill, big, head, limLine),
        statusEl, foot,
        h('details', { class: 'padv', open: !!card._advOpen, ontoggle: ev => { card._advOpen = ev.target.open; } }, h('summary', { title: 'The other settings of the loudness controller' }, 'Advanced'),
          h('div', { class: 'padv-body' }, ...rest.map(f => num(f, true))),
          h('div', { style: 'display:grid;grid-template-columns:auto auto;gap:8px 14px;margin-top:10px;align-items:start;justify-content:start' },
            ...Object.entries(LOUD_SELECTS).flatMap(([k, [label, , tip]], i) => [h('div', {}, h('div', { class: 'dim' }, label), help(tip)), selects[i]]),
            h('div', {}, h('div', { class: 'dim' }, 'Left out of the measurement'), help('Categories (and their kinds) that are not counted while they play: voices, IDs and promos should not steer the music\'s level. Applies to items loaded after saving.')), gate)),
        sel?.description ? h('div', { class: 'dim pnote' }, sel.description) : null,
        c ? h('div', { class: 'dim pnote' }, `Window of the signal entering the gain ${fmt(c.window_in)} LUFS, valid program ${Math.round(c.valid_s)} s, trim ${c.trim_db.toFixed(2)} dB.`) : null,
        j.note ? tipEl : null,
        h('div', { class: 'row pfoot' },
          h('button', { title: 'Forget the integrated reading and the measured window, and put the gain back at its starting estimate. The settings are not changed.', onclick: async () => { const r = await safe(jsend(url + '/reset', {}), 'Reset failed'); if (r) { toast('Integrated reading and gain reset'); setTimeout(paint, 700); } } }, 'Reset readings'),
          applied,
          h('span', { class: 'dim pnote' }, 'Changes apply to the running station at once and are written to its config.'))))].filter(Boolean));
    check();
  };
  await paint();
  const t = setInterval(() => { if ((!card.contains(document.activeElement) || document.activeElement === document.body) && !card.matches(':hover')) paint(true); }, 5000);   // never rebuild what the pointer is on: the browser's own tooltip would be left behind at the old place
  onCleanup(() => clearInterval(t));
  return card;
}

// ================= System =================
// The station's output trim (architecture 2.4): one gain on the sound card and the stream, in dB, applied at once
// with a short ramp and kept across restarts. It replaces alsamixer for matching the card to the next device.
function trimRow(id) {
  const url = `/api/v1/stations/${id}/output-trim`;
  const inp = h('input', { type: 'number', step: '0.5', min: '-40', max: '6', style: 'width:6em' });
  const now = h('span', { class: 'dim' }, '');
  const show = r => { const v = r.result?.output_trim_db ?? r.output_trim_db; if (v != null) { inp.value = v; now.textContent = `now ${v > 0 ? '+' : ''}${v} dB`; } };
  safe(J(url), 'Output trim').then(r => r && show(r));
  const set = async () => { const db = parseFloat(inp.value); if (!isFinite(db)) { toast('Enter a number of dB', true); return; } const r = await safe(jsend(url, { db }), 'Output trim'); if (r) { show(r); toast('Output trim set'); } };
  return h('div', { class: 'row', style: 'margin-top:8px' }, h('label', { title: 'A gain after the safety limiter, on the sound card and the stream alike. -40 to +6 dB; 0 is unity.' }, 'Output trim (dB) '), helpLink('output-and-level', 'output-trim'), inp, h('button', { onclick: set }, 'Set'), now);
}

routes.system = async () => {
  const root = h('div'); root.append(h('h1', {}, 'System', helpLink('system')));
  const lib = await J('/health');
  root.append(h('div', { class: 'card' }, h('h3', {}, 'Library service', helpLink('system', 'library-service')), h('div', { class: 'row' }, h('span', { class: 'chip ' + (lib.status === 'ok' ? 'ok' : 'crit') }, lib.status), `${lib.items} items, revision ${lib.revision}, up ${mmss(lib.uptime_s * 1000)}, ${lib.disk?.free_gb ?? '?'} GB free, ${lib.notifications_sent} messages sent`)));
  for (const s of S.stations) {
    const hh = s.health;
    root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Station ' + s.display_name, helpLink('system', 'stations')), h('div', { class: 'row' }, h('span', { class: 'chip ' + (hh?.http === 200 ? 'ok' : 'crit') }, hh?.status || 'not answering'), hh ? (hh.state || '') : '', h('span', { class: 'dim' }, s.listen)),
      trimRow(s.id),
      h('div', { style: 'margin-top:8px', 'data-xfade-system': s.id }), h('div', { class: 'row', style: 'margin-top:8px' }, h('a', { href: '#/settings' }, 'All settings of this station'), h('a', { href: '#/processing/' + s.id }, 'Processing chain'))));
    crossfadePicker(s.id).then(n => $(`[data-xfade-system="${s.id}"]`)?.replaceChildren(n));
  }
  const q = await J('/api/v1/items?flag=quarantined&per_page=100');
  root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, `Quarantine (${q.total})`, helpLink('system', 'quarantine')), ...(q.items.length ? q.items.map(i => h('div', { class: 'qrow' }, h('div', { class: 'meta' }, h('div', {}, h('a', { href: '#/item/' + i.id }, i.title)), h('div', { class: 'dim' }, i.quarantine_reason)), h('button', { title: 'Release from quarantine on the library and every station', onclick: () => releaseItem(i.id).then(ok => ok && route()) }, 'Release')))
    : [h('div', { class: 'dim' }, 'Nothing is quarantined')])));
  return root;
};

boot();
