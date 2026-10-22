// The Processing panel of the System page (architecture 6, 9): the broadcast chain as a rack of plug-in faceplates in
// chain order, the way a DAW shows its inserts. Every block has a power switch, its main controls as knobs, faders and
// buttons (the rest under Advanced), its own presets (shipped, yours) and live level and gain-reduction meters from the
// station's meter frame. "List" shows every block's settings as a plain list. Everything is built from the schema
// `GET /api/v1/stations/<id>/chain?schema=1` sends, so a setting added to the engine appears here by itself (under
// Advanced until it is named in PROC_MAIN). Loaded after app.js and procwidgets.js.

// The Processing page: one station's chain at a time, chosen with tabs like On Air's (each with the station's state),
// at #/processing/<station>. The tab last opened in this browser is kept (localStorage `procStation`).
NAV.splice(NAV.findIndex(n => n[0] === 'system'), 0, ['processing', 'Processing']);
routes.processing = async ([want]) => {
  const root = h('div', { class: 'procpage' });
  root.append(h('h1', {}, 'Processing', helpLink('processing')));
  const sl = await J('/api/v1/stations').catch(() => null);
  if (sl) S.stations = sl.stations;
  if (!S.stations.length) { root.append(h('div', { class: 'notice' }, 'No station is configured on this box.')); return root; }
  let kept = null; try { kept = localStorage.getItem('procStation'); } catch { }
  const has = id => S.stations.some(s => s.id === id);
  const id = has(want) ? want : has(kept) ? kept : has(S.station) ? S.station : S.stations[0].id;
  try { localStorage.setItem('procStation', id); } catch { }
  if (want !== id) history.replaceState(null, '', '#/processing/' + id);
  root.append(h('div', { class: 'tabs ptabs', role: 'tablist', 'aria-label': 'Station' }, S.stations.map(s => h('button', {
    class: s.id === id ? 'on' : '', role: 'tab', 'aria-selected': String(s.id === id), 'data-station': s.id, title: 'The processing chain of ' + s.display_name,
    onclick: () => { if (s.id !== id) location.hash = '#/processing/' + s.id; },
  }, s.display_name, ' ', h('span', { class: 'chip ' + (s.health?.http === 200 ? 'ok' : 'crit') }, s.health ? (s.health.status || 'down') : 'down')))));
  const s = S.stations.find(x => x.id === id);
  if (s.health?.http !== 200) root.append(h('div', { class: 'notice bad' }, `${s.display_name} is not answering, so its chain cannot be shown or changed. Its state is on the System page.`));
  else root.append(await processingPanel(s));
  return root;
};

const PROC_STAGE = { agc: 0, multiband: 1, wideband: 2, limiter: 3, clipper: 4 };
// The chain's default band layouts for each band count (a band-count change resamples the old values onto these).
const PROC_XOVERS = { 2: [300], 3: [200, 4000], 4: [150, 1500, 5000], 5: [120, 500, 2000, 6000], 6: [80, 300, 1000, 3000, 9000] };
const PROC_BAND_NAMES = { 2: ['LOW', 'HIGH'], 3: ['LOW', 'MID', 'HIGH'], 4: ['LOW', 'LOW-MID', 'HIGH-MID', 'HIGH'], 5: ['LOW', 'LOW-MID', 'MID', 'HIGH-MID', 'HIGH'], 6: ['1', '2', '3', '4', '5', '6'] };
// The settings a faceplate shows first, from broadcast practice; every other setting of the block is under Advanced.
const PROC_MAIN = {
  trim: ['gain_db'], makeup: ['gain_db'], highpass: ['freq_hz', 'order'], rotator: ['freq_hz', 'sections'], stereo: ['width'],
  agc: ['target_dbfs', 'max_gain_db', 'gate_db', 'down_db_per_s', 'up_db_per_s'],
  eq: [], bass: ['amount_pct', 'freq_hz'],
  wideband: ['threshold_dbfs', 'ratio', 'attack_ms', 'release_ms', 'makeup_db'], clipper: ['threshold_dbfs', 'softness_db'], limiter: ['ceiling_dbtp'],
};
const PROC_MB_MAIN = ['threshold_dbfs', 'ratio', 'makeup_db'];   // the multiband's faders per band; the rest is Advanced
const PROC_WIDE = ['multiband', 'eq'];

function procSame(a, b) {
  if (Array.isArray(a) || Array.isArray(b)) return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((v, i) => procSame(v, b[i]));
  if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) < 1e-9;
  return a === b;
}
function procSameParams(preset, now) {
  // Every key the preset names must match; keys it does not name (switches, names) are not compared.
  return Object.keys(preset || {}).every(k => k === 'enabled' || procSame(preset[k], now?.[k]));
}
function procStep(p) {
  if (p.kind === 'int') return 1;
  if (p.step > 0) return p.step;   // the schema names the resolution (the equalizer's 1 Hz, 0.1 dB, 0.01 Q)
  const r = p.max - p.min;
  return r <= 2 ? 0.01 : r <= 30 ? 0.1 : r <= 300 ? 0.5 : r <= 3000 ? 1 : 5;
}
function procFmt(v, p) { return v == null ? '' : p && p.kind === 'int' ? String(v) : (Math.round(v * 1000) / 1000).toString(); }

// The list view's horizontal level bar, -60 to 0 dB; `set(db)` moves it, `set(null)` empties it.
function procLevel(label, cls) {
  const fill = h('i'), tick = h('b');
  const el = h('div', { class: 'plevel ' + (cls || ''), title: label }, h('span', { class: 'dim' }, label), h('div', { class: 'bar' }, fill, tick), h('span', { class: 'mono pv' }, '--'));
  const pv = el.querySelector('.pv');
  return { el, set(db) { const w = db == null ? 0 : Math.max(0, Math.min(1, (db + 60) / 60)) * 100; fill.style.width = w + '%'; setText(pv, db == null ? '--' : db.toFixed(1)); } };
}
// The list view's gain-reduction bar: 0 to -24 dB, growing from the left; AGC gain is signed and drawn around the centre.
function procGr(label, signed) {
  const fill = h('i'), mid = signed ? h('b', { class: 'mid' }) : null;
  const el = h('div', { class: 'plevel gr', title: label }, h('span', { class: 'dim' }, label), h('div', { class: 'bar' }, fill, mid), h('span', { class: 'mono pv' }, '--'));
  const pv = el.querySelector('.pv');
  return {
    el, set(db) {
      if (db == null) { fill.style.width = '0'; fill.style.left = '0'; setText(pv, '--'); return; }
      if (signed) {
        const w = Math.min(12, Math.abs(db)) / 12 * 50;
        fill.style.left = db >= 0 ? '50%' : (50 - w) + '%'; fill.style.width = w + '%';
      } else { fill.style.left = '0'; fill.style.width = Math.min(24, Math.abs(db)) / 24 * 100 + '%'; }
      setText(pv, (signed && db > 0 ? '+' : '') + db.toFixed(1));
    },
  };
}
const procPowerIcon = () => { const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.setAttribute('viewBox', '0 0 24 24'); s.setAttribute('aria-hidden', 'true'); s.innerHTML = '<path d="M12 3v8M6.3 6.8a8 8 0 1 0 11.4 0" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"/>'; return s; };

async function processingPanel(s) {
  const base = `/api/v1/stations/${s.id}`;
  const root = h('div', { class: 'proc', 'data-proc': s.id });
  let chain;
  try { chain = await J(base + '/chain?schema=1'); } catch (e) { root.append(h('div', { class: 'notice bad' }, 'Processing chain: ' + e.message)); return root; }
  const schema = Object.fromEntries(chain.schema.map(b => [b.key, b]));
  const cache = {};   // effect -> items of the presets API
  const cards = {};   // block -> card element
  const meters = {};  // block -> { update(frame) }
  const openAdv = new Set();   // blocks whose Advanced fold the operator opened
  let curveTable = null;
  let latest = null;
  let agcNow = null;   // the AGC's last answer (GET /chain/agc), for the Rack view's strip
  const rackUnits = {};   // Rack view: block -> { unit, strip }
  let openKey = ''; try { openKey = localStorage.getItem('procOpen') || ''; } catch { }
  let lastWarn = '';
  // The view: Rack (the default), Faceplates, or List. The browser keeps the choice.
  const view = () => { try { const v = localStorage.getItem('procView'); return v === 'list' || v === 'face' ? v : 'rack'; } catch { return 'rack'; } };
  const setView = v => { try { localStorage.setItem('procView', v); } catch { } };
  const adopt = r => { chain = { ...r, schema: chain.schema }; };

  const loadPresets = async (effect, force) => { if (force || !cache[effect]) cache[effect] = (await J('/api/v1/processing-presets/' + effect)).items; return cache[effect]; };
  const askName = (msg, dflt) => { const n = prompt(msg, dflt || ''); return n && n.trim() ? n.trim() : null; };

  // ---- sending changes: during a drag one PUT per block every 100 ms at most, and one when the control is let go ----
  const pend = {}, timers = {}, running = {};
  const send = (block, patch, final) => {
    pend[block] = { ...(pend[block] || {}), ...patch };
    if (final) { clearTimeout(timers[block]); timers[block] = null; go(block); }
    else if (!timers[block]) timers[block] = setTimeout(() => { timers[block] = null; go(block); }, 100);
  };
  async function go(block) {
    if (running[block]) return;   // when it answers, what piled up meanwhile is sent
    const body = pend[block]; if (!body) return;
    pend[block] = null; running[block] = true;
    const r = await safe(jsend(`${base}/chain/${block}`, body, 'PUT'), schema[block].label);
    running[block] = false;
    if (r) {
      adopt(r);
      if (r.persist_warning && r.persist_warning !== lastWarn) toast(r.persist_warning, true);
      lastWarn = r.persist_warning || '';
    }
    if (pend[block]) { go(block); return; }
    if (timers[block]) return;
    after(block);
  }
  const settled = block => !pend[block] && !timers[block] && !running[block];
  // The station answered (or refused): show what it holds, in place where a control is being used.
  function after(block) {
    if (view() === 'list') { refresh(block); return; }
    syncBlock(block);
  }

  // ---- one preset bar, for a block or for the whole chain ----
  function presetBar(effect, cur) {
    // cur: { name(), base(), getParams(), sameAs(preset), apply(preset), named(name) }
    const wrap = h('div', { class: 'row pbar' });
    const draw = async () => {
      let items; try { items = await loadPresets(effect); } catch { return; }
      const sel = items.find(p => p.name === cur.name());
      const user = sel && !sel.builtin;
      const modified = sel ? !cur.sameAs(sel) : !!cur.base();
      const none = !sel || modified;
      // "custom (based on X)": the settings were changed from the preset X, or from the chain's shipped base
      const basis = sel ? sel.name : cur.base();
      const custom = sel ? (modified ? `custom (based on ${basis})` : '') : basis ? `custom (based on ${basis})` : 'custom (no preset)';
      const pick = h('select', { title: 'Presets', 'aria-label': 'Preset', onchange: ev => { const p = items.find(x => x.name === ev.target.value); if (p) cur.apply(p); } },
        h('option', { value: '', selected: none }, custom || '(choose a preset)'),
        h('optgroup', { label: 'Shipped' }, items.filter(p => p.builtin).map(p => h('option', { value: p.name, selected: !none && p.name === sel.name }, p.name))),
        items.some(p => !p.builtin) ? h('optgroup', { label: 'Yours' }, items.filter(p => !p.builtin).map(p => h('option', { value: p.name, selected: !none && p.name === sel.name }, p.name))) : null);
      const files = h('input', { type: 'file', accept: '.json,application/json', style: 'display:none', onchange: async ev => {
        const f = ev.target.files[0]; if (!f) return;
        let file; try { file = JSON.parse(await f.text()); } catch { toast('Not a preset file', true); return; }
        const pUrl = '/api/v1/processing-presets/' + effect;
        let r = await jsend(pUrl + '/import', { file }).catch(e => e);
        if (r instanceof Error && /already a preset/.test(r.message) && confirm(r.message + '. Replace it?')) r = await jsend(pUrl + '/import', { file, overwrite: true }).catch(e => e);
        if (r instanceof Error) { toast('Import failed: ' + r.message, true); return; }
        toast('Imported ' + r.name); await loadPresets(effect, true); draw();
      } });
      const pUrl = '/api/v1/processing-presets/' + effect;
      const saveAs = async () => {
        const n = askName('Save the settings as a preset named', user ? sel.name + ' copy' : (cur.name() ? cur.name() + ' copy' : ''));
        if (!n) return;
        if (items.some(p => p.name === n && !p.builtin) && !confirm('There is already a preset of yours named "' + n + '". Replace it?')) return;
        const r = await safe(jsend(`${pUrl}/${encodeURIComponent(n)}`, { params: cur.getParams(), description: '' }, 'PUT'), 'Not saved');
        if (r) { await loadPresets(effect, true); cur.named(n); }
      };
      const menu = h('details', { class: 'pmenu' }, h('summary', { title: 'Save, update, rename, delete, export and import presets' }, 'Presets'),
        h('div', { class: 'pmenu-body' },
          h('button', { title: 'Save the settings as a new preset of yours', onclick: saveAs }, 'Save as…'),
          h('button', { disabled: !user, title: user ? 'Replace the selected preset with the settings' : 'Shipped presets cannot be changed: use Save as', onclick: async () => { const r = await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}`, { params: cur.getParams(), description: sel.description }, 'PUT'), 'Not saved'); if (r) { toast('Updated ' + sel.name); await loadPresets(effect, true); draw(); } } }, 'Update'),
          h('button', { disabled: !user, onclick: async () => { const n = askName('Rename to', sel.name); if (!n || n === sel.name) return; const r = await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}/rename`, { to: n }), 'Not renamed'); if (r) { await loadPresets(effect, true); cur.named(n); } } }, 'Rename…'),
          h('button', { disabled: !user, onclick: async () => { if (!confirm('Delete the preset "' + sel.name + '"?')) return; await safe(jsend(`${pUrl}/${encodeURIComponent(sel.name)}`, {}, 'DELETE'), 'Not deleted'); await loadPresets(effect, true); cur.named(''); } }, 'Delete'),
          h('a', { href: sel ? `${pUrl}/${encodeURIComponent(sel.name)}/export` : null, download: true }, h('button', { disabled: !sel }, 'Export')),
          h('button', { onclick: () => files.click() }, 'Import…'), files));
      wrap.replaceChildren(...[pick, modified ? h('span', { class: 'chip warn', title: 'The settings differ from the preset' }, 'modified') : null, menu].filter(Boolean));
    };
    wrap.redraw = draw;
    draw();
    return wrap;
  }

  // ---- the list view's controls (every setting as a row: slider and number box) ----
  function control(block, p, value) {
    const key = p.key;
    const put = (v, now) => send(block, { [key]: v }, now);
    if (p.kind === 'bool') { const e = h('input', { type: 'checkbox', checked: !!value, onchange: () => put(e.checked, true) }); return field(p, e); }
    if (p.kind === 'choice') { const e = h('select', { onchange: () => put(e.value, true) }, p.options.map(o => h('option', { value: o, selected: o === value }, o))); return field(p, e); }
    const step = procStep(p);
    const num = h('input', { type: 'number', step, min: p.min, max: p.max, value: procFmt(value, p), class: 'pnum', 'data-k': key });
    const rng = h('input', { type: 'range', step, min: p.min, max: p.max, value, class: 'prng', 'aria-label': p.label });
    const commit = v => { if (!isFinite(v)) return; v = Math.min(p.max, Math.max(p.min, v)); num.value = procFmt(v, p); rng.value = v; put(v, true); };
    rng.addEventListener('input', () => { num.value = procFmt(+rng.value, p); });
    rng.addEventListener('change', () => commit(+rng.value));
    num.addEventListener('change', () => commit(parseFloat(num.value)));
    return field(p, h('span', { class: 'pctl' }, rng, num));
  }
  function field(p, el) {
    return h('div', { class: 'pfield', title: p.help }, h('label', {}, p.label, p.unit ? h('span', { class: 'dim' }, ' ' + p.unit) : null), el);
  }
  // the multiband's band table (list view)
  function multibandTable() {
    const def = schema.multiband, v = chain.blocks.multiband;
    const n = v.crossovers_hz.length + 1;
    const per = def.params.filter(p => p.kind === 'list' && p.per_band);
    const xo = def.params.find(p => p.key === 'crossovers_hz');
    const names = PROC_BAND_NAMES[n];
    const inputs = k => cards.multiband.querySelectorAll(`input[data-list="${k}"]`);
    const collect = () => {
      const out = { crossovers_hz: [...inputs('xo')].map(e => +e.value) };
      for (const p of per) out[p.key] = [...inputs(p.key)].map(e => +e.value);
      return out;
    };
    const commit = () => send('multiband', collect(), true);
    const cell = (k, i, p, val) => h('td', {}, h('input', { type: 'number', 'data-list': k, 'data-i': i, class: 'pnum', 'aria-label': p.label + ' ' + (i + 1), step: k === 'xo' ? 1 : procStep({ ...p, kind: 'float' }), min: p.min, max: p.max, value: procFmt(val), onchange: commit }));
    meters.multiband.bands = [];
    const rows = [
      h('tr', {}, h('th', {}, ''), ...names.map(nm => h('th', {}, nm))),
      h('tr', { title: xo.help }, h('td', { class: 'dim' }, 'Upper edge, Hz'), ...Array.from({ length: n }, (_, i) => i < n - 1 ? cell('xo', i, xo, v.crossovers_hz[i]) : h('td', { class: 'dim' }, 'top'))),
      ...per.map(p => h('tr', { title: p.help }, h('td', { class: 'dim' }, p.label + (p.unit ? ', ' + p.unit : '')), ...v[p.key].map((val, i) => cell(p.key, i, p, val)))),
      h('tr', { class: 'grrow' }, h('td', { class: 'dim' }, 'Reduction now, dB'), ...Array.from({ length: n }, (_, i) => { const g = procGr('band ' + (i + 1), false); meters.multiband.bands[i] = g; return h('td', {}, g.el); })),
    ];
    return h('div', { class: 'ptable' }, h('table', {}, h('tbody', {}, rows)));
  }
  function bandCountSelect() {
    const def = schema.multiband, v = chain.blocks.multiband, n = v.crossovers_hz.length + 1;
    const per = def.params.filter(p => p.kind === 'list' && p.per_band);
    return h('select', { class: 'pbands', title: 'Number of bands (2 to 6). Changing it resamples the values onto the default crossovers for that count.', 'aria-label': 'Number of bands', onchange: ev => {
      const m = +ev.target.value, out = { crossovers_hz: PROC_XOVERS[m] };
      for (const p of per) out[p.key] = Array.from({ length: m }, (_, i) => v[p.key][Math.round(i * (n - 1) / Math.max(1, m - 1))]);
      send('multiband', out, true);
    } }, [2, 3, 4, 5, 6].map(k => h('option', { value: k, selected: k === n }, k + ' bands')));
  }

  // ---- faceplate controls ----
  // One widget for a schema parameter; `ctl` collects them under their key so a station answer can set them in place.
  function widgetFor(block, p, value, ctl, opts = {}) {
    const put = (v, final) => send(block, { [p.key]: v }, final);
    let w;
    if (p.kind === 'bool') w = PW.toggle({ label: p.label, help: p.help, value, key: p.key, onChange: put });
    else if (p.kind === 'choice') w = PW.segmented({ label: p.label, help: p.help, value, key: p.key, slot: opts.slot, options: p.options.map(o => ({ value: o, label: o })), onChange: put });
    else if (p.kind === 'int' && p.max - p.min <= 5) w = PW.segmented({ label: p.label, help: p.help, value, key: p.key, slot: opts.slot, options: Array.from({ length: p.max - p.min + 1 }, (_, i) => ({ value: p.min + i, label: String(p.min + i) })), onChange: (v, f) => put(+v, f) });
    else w = PW.knob({ ...PW.spec(p), value, key: p.key, onChange: put, small: opts.small });
    ctl[p.key] = w;
    return w.el;
  }

  // ---- one block's card ----
  const sig = key => key === 'multiband' ? 'b' + (chain.blocks.multiband.crossovers_hz.length + 1) : key === 'eq' ? 'p' + chain.blocks.eq.position + 'n' + chain.blocks.eq.bands : '';
  function powerButton(key, def) {
    if (!def.switch) return h('span', { class: 'pwr fixed on', title: 'Always on: this block cannot be switched off' }, procPowerIcon(), h('span', { class: 'pwr-t' }, 'ALWAYS ON'));
    return h('button', { type: 'button', class: 'pwr', 'data-power': key, 'aria-pressed': 'false', onclick: () => send(key, { enabled: !chain.blocks[key].enabled }, true) }, procPowerIcon(), h('span', { class: 'pwr-t' }, ''));
  }
  const paintPower = (key, card) => {
    const on = !schema[key].switch || !!chain.blocks[key].enabled;
    card.classList.toggle('off', !on);
    const b = card.querySelector('[data-power]');
    if (b) {
      b.classList.toggle('on', on); b.setAttribute('aria-pressed', on);
      b.title = on ? 'On. Click to bypass this block (the delay stays constant, no click).' : 'Bypassed: the program passes this block unchanged. Its controls still work. Click to switch it in.';
      setText(b.querySelector('.pwr-t'), on ? 'ON' : 'BYPASSED');
      b.setAttribute('aria-label', schema[key].label + (on ? ': on, click to bypass' : ': bypassed, click to switch on'));
    }
  };

  function blockCard(key) {
    const def = schema[key], v = chain.blocks[key];
    const lay = view();
    const card = h('div', { class: 'pcard' + (PROC_WIDE.includes(key) && lay === 'face' ? ' wide' : ''), 'data-block': key, 'data-sig': sig(key) });
    const ctl = {};
    cards[key] = card;
    const m = meters[key] = { items: [] };
    const power = powerButton(key, def);
    const head = h('div', { class: 'phead' }, h('span', { class: 'pidx', 'aria-hidden': 'true' }, ''), power, h('div', { class: 'ptitle' }, h('span', { class: 'ptitle-t' }, def.label), helpLink('processing', `${key}`)));
    const body = h('div', { class: 'pbody' });
    const ribbon = h('div', { class: 'pclip', 'aria-hidden': 'true' }, h('div', { class: 'pribbon' }, 'BYPASSED'));

    const staged = key in PROC_STAGE;
    if (staged) m.stage = PROC_STAGE[key];
    const preset = key !== 'trim' && key !== 'makeup' ? presetBar(key, {
      name: () => chain.block_presets?.[key] || '',
      base: () => '',
      getParams: () => { const { enabled, ...rest } = chain.blocks[key]; return rest; },
      sameAs: pr => procSameParams(pr.params, chain.blocks[key]),
      apply: pr => send(key, { ...pr.params, user_preset: pr.name }, true),
      named: n => send(key, { user_preset: n }, true),
    }) : null;
    // the nameplate's second line: the block's presets (the input trim and the output makeup have none), and the Advanced switch at its right
    const prow = h('div', { class: 'pprow' }, preset || h('span', { class: 'dim pnone' }, 'No presets'));

    if (lay === 'list') {
      const dbMeters = h('div', { class: 'pmeters' });
      if (staged) {
        const lvIn = procLevel('in'), lvOut = procLevel('out');
        dbMeters.append(lvIn.el, lvOut.el);
        const gr = procGr(key === 'agc' ? 'gain' : 'reduction', key === 'agc');
        if (key !== 'multiband') dbMeters.append(gr.el);
        m.lvIn = lvIn; m.lvOut = lvOut; m.gr = gr;
        if (key === 'agc') { m.gate = h('span', { class: 'chip', title: 'The gate holds the gain in silence' }, 'gate'); dbMeters.append(m.gate); }
        if (key === 'clipper') { m.act = h('span', { class: 'chip', title: 'The clipper is shaving peaks' }, 'clipping'); dbMeters.append(m.act); }
        if (key === 'limiter') { m.act = h('span', { class: 'chip', title: 'The limiter is reducing the gain' }, 'limiting'); dbMeters.append(m.act); }
      }
      let extra = null;
      if (key === 'multiband') {
        extra = h('div', {}, h('div', { class: 'row' }, bandCountSelect(), h('span', { class: 'dim' }, def.help)), multibandTable());
        m.curve = h('canvas', { class: 'pcurve', width: 640, height: 120, title: 'The gain curve the compressor applies now: the sum of the bands at their current reduction (the model of 11.2a)' });
        extra.append(m.curve);
      }
      const grid = h('div', { class: 'pgrid' });
      for (const p of def.params) { if (key === 'multiband' && p.kind === 'list') continue; grid.append(control(key, p, v[p.key])); }
      body.append(...[dbMeters, extra, grid].filter(Boolean));
    } else {
      body.append(faceBody(key, def, v, ctl, m, card));
    }
    if (key === 'limiter') body.append(h('div', { class: 'dim pnote' }, 'The loudness controller turns the limiter on by itself, at its own ceiling, when it is enabled; the lower of the two ceilings holds. The lookahead (5 ms) and the release are fixed.'));
    if (key === 'eq' && lay === 'list') body.append(h('div', { class: 'dim pnote' }, v.position === 'post' ? 'Shown after the multiband compressor, where it runs.' : 'Runs before the multiband compressor.'));
    card.append(...(lay === 'rack' ? [] : [head]), prow, ribbon, body);   // in the Rack view the strip is the head
    card._ctl = ctl;
    paintPower(key, card);
    card._sync = () => {
      const nv = chain.blocks[key];
      paintPower(key, card);
      for (const [k, w] of Object.entries(ctl)) { if (k.includes(':')) continue; if (nv[k] != null && !Array.isArray(nv[k])) w.set(nv[k]); }
      card._syncLists?.(nv);
      if (card._eq && !card._eq.busy()) card._eq.paint();
      preset?.redraw();
    };
    return card;
  }

  // The meter dock at the right edge of a faceplate: input and output level, the block's gain reduction, and a line of
  // status lamps (gate, clipping, limiting) that is there, empty, on every dock so that all docks are the same height.
  function meterDock(key, m) {
    const dock = h('div', { class: 'pmeterc' });
    const bars = h('div', { class: 'pmeters-v' });
    m.lvIn = PW.level('in'); m.lvOut = PW.level('out');
    if (key !== 'agc') bars.append(m.lvIn.el, m.lvOut.el);   // the AGC shows its detectors instead (below)
    // The AGC shows its two detectors against its target line (the white line), like the loudness card's bars.
    if (key === 'agc') {
      const tg = chain.blocks.agc.target_dbfs;
      m.fast = PW.level('fast', tg); m.prog = PW.level('prog', tg);
      m.fast.el.title = 'The fast detector (400 ms): sees a fade and an onset. The white line is the AGC\'s target.';
      m.prog.el.title = 'The program detector (3 s): sets the long-term level. The white line is the AGC\'s target.';
      bars.append(m.fast.el, m.prog.el);
    }
    if (key !== 'multiband') { m.gr = PW.gr(key === 'agc' ? 'gain' : 'GR', key === 'agc' || key === 'clipper' || key === 'limiter' || key === 'safety' ? 12 : 18, key === 'agc'); bars.append(m.gr.el); }
    const flags = h('div', { class: 'pflags' });
    if (key === 'agc') { m.gate = h('span', { class: 'chip', title: 'The gate holds the gain in silence' }, 'gate'); flags.append(m.gate); }
    if (key === 'clipper') { m.act = h('span', { class: 'chip', title: 'The clipper is shaving peaks' }, 'clipping'); flags.append(m.act); }
    if (key === 'limiter') { m.act = h('span', { class: 'chip', title: 'The limiter is reducing the gain' }, 'limiting'); flags.append(m.act); }
    dock.append(bars, flags);
    return dock;
  }

  // The faceplate body of a block: one line of the main controls, a display where the block has one (the equalizer's
  // curve, the AGC's and the compressor's history, the multiband's strips and gain curve) taking the width the
  // controls leave, the meter dock at the right edge, and the Advanced fold below (its switch sits in the nameplate).
  function faceBody(key, def, v, ctl, m, card) {
    const out = h('div', { class: 'pface' });
    const mainRow = h('div', { class: 'pctls' });
    const adv = h('div', { class: 'padv-body' });
    let advCount = 0;
    const staged = key in PROC_STAGE;
    const line = h('div', { class: 'pfrow' });
    if (key === 'multiband') {
      multibandFace(def, v, ctl, m, card, line, adv);
      advCount = 1;
    } else {
      const main = PROC_MAIN[key] || def.params.map(p => p.key);
      if (key === 'eq') eqFace(def, v, ctl, card, line);
      else {
        for (const p of def.params) {
          const isMain = main.includes(p.key);
          (isMain ? mainRow : adv).append(widgetFor(key, p, v[p.key], ctl, { small: !isMain, slot: isMain }));
          if (!isMain) advCount++;
        }
        if (key === 'agc' || key === 'wideband') {
          m.hist = PW.history(key === 'agc'
            ? { kind: 'gain', seconds: 60, range: 12, label: 'Gain, last 60 s', help: 'The AGC\'s gain over the last minute: a boost above the centre line, a cut below.' }
            : { kind: 'comp', seconds: 20, range: 12, label: 'Last 20 s', help: 'The last 20 seconds: the input level (grey, from the bottom), the output level (green line) and the gain reduction (amber, hanging from the top).' });
          line.append(mainRow, m.hist.el);
        } else line.append(mainRow);
        mainRow.classList.toggle('pspread', !(key === 'agc' || key === 'wideband'));
      }
    }
    if (staged) line.append(meterDock(key, m));
    if (key === 'agc') {
      m.pill = fixedChip(['RIDING', 'GATED', 'FADE', 'CLAMPING', 'BYPASSED'], { class: 'chip lr-pill', 'data-agc-pill': '' });
      m.big = h('span', { class: 'lr-big mono' }, '--'); m.head = h('span', { class: 'lr-head mono dim' }, '');
      out.append(h('div', { class: 'lr-top' }, m.pill, m.big, m.head));
    }
    out.append(line);
    if (advCount) {
      const d = h('details', { class: 'padv', open: openAdv.has(key) }, h('summary', { title: 'The other settings of this block' }, 'Advanced'), adv);
      d.addEventListener('toggle', () => { d.open ? openAdv.add(key) : openAdv.delete(key); });
      out.append(d);
      card.classList.add('has-adv');
    }
    return out;
  }

  // The parametric equalizer: the curve with a point for each band that is on (drag: frequency and gain), and
  // under it a strip with a column for each band: on, type, frequency, gain and Q. Each number can be dragged, stepped
  // with the arrows, or typed exactly (4087 Hz, -1.7 dB, Q 1.57). "+ Band" adds a band (up to 8), the
  // cross on a column removes it and the bands after it move up. A band that is off keeps its settings.
  const EQ_BAND_KEYS = ['on', 'type', 'hz', 'db', 'q'];
  // The equalizer's analyzer: the card says which tap it wants (off, in, out); this connection subscribes to that topic
  // only (the station computes the spectrum only while somebody is subscribed), and not while the tab is hidden.
  let liveRef = null, anaTap = 'off';
  const anaSync = () => liveRef?.want(anaTap === 'off' || document.hidden ? [] : ['eqspec_' + anaTap]);
  const setAnaTap = t => { anaTap = t; anaSync(); };
  document.addEventListener('visibilitychange', anaSync);
  onCleanup(() => document.removeEventListener('visibilitychange', anaSync));
  function eqFace(def, v, ctl, card, line) {
    const P = Object.fromEntries(def.params.map(p => [p.key, p]));
    const N = Math.min(EqMath.MAX_BANDS, Math.max(1, v.bands | 0));
    const corner = widgetFor('eq', P.position, v.position, ctl, { small: true });
    const nowEq = () => chain.blocks.eq;
    const addBand = (hz, db) => {
      const e = nowEq(), n = (e.bands | 0) + 1;
      if (n > EqMath.MAX_BANDS) return;
      const patch = { bands: n, [`b${n}_on`]: true, [`b${n}_type`]: 'peak', [`b${n}_q`]: 1 };
      patch[`b${n}_hz`] = hz != null ? Math.min(P[`b${n}_hz`].max, Math.max(P[`b${n}_hz`].min, hz)) : P[`b${n}_hz`].default;
      patch[`b${n}_db`] = db != null ? db : 0;
      send('eq', patch, true);
    };
    const removeBand = k => {
      const e = nowEq(), n = e.bands | 0;
      if (n <= 1) return;
      const patch = { bands: n - 1 };
      for (let i = k; i < n; i++) for (const s of EQ_BAND_KEYS) patch[`b${i}_${s}`] = e[`b${i + 1}_${s}`];
      for (const s of EQ_BAND_KEYS) patch[`b${n}_${s}`] = P[`b${n}_${s}`].default;   // the freed slot goes back to its defaults
      send('eq', patch, true);
    };
    card._eq = PW.eqCurve({ params: P, corner, get: nowEq, onAnalyzer: setAnaTap, onChange: (patch, final) => send('eq', patch, final), onAdd: addBand });
    const strip = h('div', { class: 'eq-strip', role: 'group', 'aria-label': 'Equalizer bands' });
    for (let n = 1; n <= N; n++) {
      const k = s => `b${n}_${s}`;
      const col = h('div', { class: `eq-band eq-c${n}`, 'data-band': n });
      const onBtn = h('button', { type: 'button', class: 'eq-on', role: 'switch', 'aria-label': `Band ${n} on`, title: `Band ${n}: on or off. A band that is off does nothing and keeps its settings.`, onclick: () => send('eq', { [k('on')]: !nowEq()[k('on')] }, true) }, String(n));
      const rm = h('button', { type: 'button', class: 'eq-rm', 'aria-label': `Remove band ${n}`, title: `Remove band ${n}; the bands after it move up one`, disabled: N <= 1, onclick: () => removeBand(n) }, '×');
      const sel = h('select', { class: 'eq-type', 'aria-label': `Band ${n} type`, title: 'The type of filter: bell, low or high shelf, low-pass, high-pass, notch or band-pass', onchange: () => {
        const t = sel.value, e = nowEq(), was = e[k('type')], patch = { [k('type')]: t };
        // a bell's default Q is 1, a shelf's or a pass filter's 0.71: follow the type's own default unless Q was set by hand
        if (Math.abs(e[k('q')] - EqMath.defaultQ(was)) < 0.005) patch[k('q')] = EqMath.defaultQ(t);
        send('eq', patch, true);
      } }, EqMath.TYPES.map(t => h('option', { value: t[0], selected: t[0] === v[k('type')] }, t[1])));
      const num = (s, short) => PW.scrub({ ...PW.spec(P[k(s)]), value: v[k(s)], key: k(s), short, label: `Band ${n} ${P[k(s)].label.replace(/^Band \d+ /, '')}`, def: P[k(s)].default, onChange: (val, f) => send('eq', { [k(s)]: val }, f) });
      const fw = num('hz', 'Hz'), gw = num('db', 'dB'), qw = num('q', 'Q');
      const setDef = () => { gw.disable(!EqMath.hasGain(sel.value)); };
      ctl[k('on')] = { set: x => { col.classList.toggle('off', !x); onBtn.setAttribute('aria-checked', !!x); onBtn.classList.toggle('on', !!x); }, busy: () => false };
      ctl[k('type')] = { set: x => { if (sel.value !== x) sel.value = x; setDef(); }, busy: () => document.activeElement === sel };
      ctl[k('hz')] = fw; ctl[k('db')] = gw; ctl[k('q')] = qw;
      ctl[k('on')].set(v[k('on')]); setDef();
      col.append(h('div', { class: 'eq-bh' }, onBtn, rm), sel, fw.el, gw.el, qw.el);
      strip.append(col);
    }
    const add = h('button', { type: 'button', class: 'eq-add', disabled: N >= EqMath.MAX_BANDS, title: N >= EqMath.MAX_BANDS ? 'The equalizer has 8 bands, the most there are' : 'Add a band (a bell at 0 dB); double-click an empty place on the curve to add one there', onclick: () => addBand() }, '+ Band');
    strip.append(add);
    line.classList.add('eqrow');
    line.append(card._eq.el, strip);
  }

  // The multiband as a compact rack: the band count as a stacked switch at the left, then a channel strip per band
  // (its name and range, the band's gain reduction hanging from the top beside the threshold, ratio and makeup faders),
  // the live gain curve beneath the strips and exactly as wide as they are, the IN and OUT meters docked at the right.
  // The strips share the width the card has; crossovers, knees and times are under Advanced.
  function multibandFace(def, v, ctl, m, card, line, adv) {
    const n = v.crossovers_hz.length + 1;
    const names = PROC_BAND_NAMES[n];
    const per = def.params.filter(p => p.kind === 'list' && p.per_band);
    const xo = def.params.find(p => p.key === 'crossovers_hz');
    const loc = {};   // the lists as the operator has them, sent whole on a change
    for (const p of [xo, ...per]) loc[p.key] = [...v[p.key]];
    const putList = (key, i, val, final) => { loc[key][i] = val; send('multiband', { [key]: [...loc[key]] }, final); };
    const dflt = (p, i) => Array.isArray(p.default) ? (p.default[i] ?? p.default[p.default.length - 1]) : p.default;
    const lists = {};   // 'key:i' -> widget
    const mk = (p, i, opts) => {
      const sp = PW.spec({ ...p, kind: 'float' }, { def: dflt(p, i), label: opts.label || (opts.fader ? ({ Threshold: 'Thresh' })[p.label] || p.label : p.label) });
      const w = opts.fader
        ? PW.fader({ ...sp, value: v[p.key][i], key: p.key + ':' + i, unitInLabel: true, onChange: (val, f) => putList(p.key, i, val, f) })
        : PW.knob({ ...sp, value: v[p.key][i], key: p.key + ':' + i, small: true, onChange: (val, f) => putList(p.key, i, val, f) });
      lists[p.key + ':' + i] = w; ctl[p.key + ':' + i] = w;
      return w.el;
    };
    const fHz = x => x >= 1000 ? (Math.round(x / 100) / 10) + 'k' : String(Math.round(x));
    const range = i => i === 0 ? `below ${fHz(v.crossovers_hz[0])}` : i === n - 1 ? `above ${fHz(v.crossovers_hz[n - 2])}` : `${fHz(v.crossovers_hz[i - 1])} to ${fHz(v.crossovers_hz[i])}`;
    m.bands = [];
    const bandNo = PW.segmented({ label: 'Bands', vertical: true, value: n, options: [2, 3, 4, 5, 6].map(k => ({ value: k, label: String(k) })), onChange: val => {
      const k = +val, o = n, out = { crossovers_hz: PROC_XOVERS[k] };
      for (const p of per) out[p.key] = Array.from({ length: k }, (_, i) => v[p.key][Math.round(i * (o - 1) / Math.max(1, k - 1))]);
      send('multiband', out, true);
    } }).el;
    bandNo.classList.add('pbandsel');
    const strips = h('div', { class: 'pstrips' }, names.map((nm, i) => {
      const g = PW.gr('band ' + (i + 1), 18); m.bands[i] = g;
      return h('div', { class: 'pstrip', 'data-band': i }, h('div', { class: 'pstrip-n', title: 'The band and the frequencies it covers, Hz' }, h('b', {}, nm), h('span', { class: 'dim' }, range(i))),
        h('div', { class: 'pstrip-f' }, h('div', { class: 'pf pgr', title: 'The gain reduction of this band now (hangs from the top, 0 to -18 dB)' }, h('span', { class: 'pk-lab' }, 'GR'), g.el),
          PROC_MB_MAIN.map(k => mk(per.find(p => p.key === k), i, { fader: true }))));
    }));
    m.curve = h('canvas', { class: 'pcurve', role: 'img', 'aria-label': 'The gain curve the multiband compressor applies now', title: 'The gain curve the compressor applies now: the sum of the bands at their current reduction (the model of 11.2a)' });
    const curveBox = h('div', { class: 'pcurve-box' }, m.curve);
    new ResizeObserver(() => { const w = Math.round(curveBox.clientWidth), hh = Math.round(curveBox.clientHeight); if (w > 10 && (m.curve.width !== w || m.curve.height !== hh)) { m.curve.width = w; m.curve.height = hh; if (latest) drawCurve(m.curve, latest, true); } }).observe(curveBox);
    line.append(bandNo, h('div', { class: 'pmb' }, strips, curveBox));
    adv.append(h('div', { class: 'padv-row' }, h('span', { class: 'padv-h', title: xo.help }, 'Crossovers'), ...v.crossovers_hz.map((_, i) => mk(xo, i, { label: `${names[i]} | ${names[i + 1]}` }))));
    for (const p of per.filter(p => !PROC_MB_MAIN.includes(p.key))) adv.append(h('div', { class: 'padv-row' }, h('span', { class: 'padv-h', title: p.help }, p.label), ...v[p.key].map((_, i) => mk(p, i, { label: names[i] }))));
    card._syncLists = nv => {
      for (const p of [xo, ...per]) { loc[p.key] = [...nv[p.key]]; nv[p.key].forEach((x, i) => lists[p.key + ':' + i]?.set(x)); }
    };
  }

  function safetyCard() {
    const m = { stage: 5 };
    const rackMode = view() === 'rack';
    const card = h('div', { class: 'pcard safety', 'data-block': 'safety' }, rackMode ? null : h('div', { class: 'phead' }, h('span', { class: 'pidx', 'aria-hidden': 'true' }), h('span', { class: 'pwr fixed on', title: 'Always on: this block cannot be switched off' }, procPowerIcon(), h('span', { class: 'pwr-t' }, 'ALWAYS ON')), h('div', { class: 'ptitle' }, h('span', { class: 'ptitle-t' }, 'Safety limiter'), helpLink('processing', 'safety-limiter'))),
      h('div', { class: 'pprow' }, h('span', { class: 'dim pnone' }, 'Fixed settings')),
      h('div', { class: 'pbody' }, h('div', { class: 'pface' }, h('div', { class: 'pfrow' }, h('div', { class: 'dim pnote' }, 'Always on, last in the chain: 2 ms lookahead, -1.0 dBFS sample ceiling, 50 ms release. It cannot be switched off and should almost never act.'), meterDock('safety', m)))));
    meters.safety = m;
    cards.safety = card;
    return card;
  }
  // ---- the whole chain's bar ----
  function topCard() {
    const card = h('div', { class: 'card ptop' });
    const cur = {
      name: () => chain.user_preset || (!chain.custom ? chain.preset : ''),
      base: () => chain.custom && !chain.user_preset ? chain.preset : '',
      getParams: () => chain.blocks,
      sameAs: pr => pr.builtin ? !chain.custom : Object.entries(pr.params).every(([b, o]) => procSameParams(o, chain.blocks[b]) && (o.enabled == null || o.enabled === chain.blocks[b]?.enabled)),
      apply: pr => safe(jsend(`${base}/chain`, pr.builtin ? { preset: pr.name } : { chain: pr.params, user_preset: pr.name }, 'PUT'), 'Chain preset').then(r => { if (r) { adopt(r); repaintAll(); } }),
      named: n => safe(jsend(`${base}/chain`, { user_preset: n }, 'PUT'), 'Chain preset').then(r => { if (r) { adopt(r); repaintAll(); } }),
    };
    const bypass = h('button', { class: chain.bypass_override ? 'primary' : '', 'data-bypass': s.id, title: 'Take the whole chain out of the path and back, without changing the saved settings; the next start returns to the configured chain.', onclick: async () => {
      const r = await safe(jsend(`${base}/processing/bypass`, { on: !chain.bypass_override }), 'Bypass'); if (r) { adopt(r); repaintAll(); }
    } }, chain.bypass_override ? 'Chain bypassed: switch back in' : 'Bypass the chain');
    // The settings history (history.js): after a restore the chain and the loudness card are read again.
    const historyBtn = ConfigHistoryUI.button(s, async () => { try { adopt(await J(base + '/chain')); } catch { } loudEl = null; await repaintAll(); });
    const v = view();
    const seg = PW.segmented({ label: 'View', value: v, options: [{ value: 'rack', label: 'Rack' }, { value: 'face', label: 'Faceplates' }, { value: 'list', label: 'List', title: "Shows every block's settings as a plain list" }], onChange: nv => { setView(nv); repaintAll(); } });
    seg.el.classList.add('pviewseg');
    const chainWord = chain.user_preset ? chain.user_preset : chain.custom ? `custom (based on ${chain.preset})` : chain.preset;
    card.append(...[h('h3', {}, 'Processing chain, ' + s.display_name, helpLink('processing')),
      chain.persist_warning ? h('div', { class: 'notice', role: 'alert', 'data-persist-warning': '' }, h('b', {}, 'Not saved. '), chain.persist_warning) : null,
      h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Chain preset'), presetBar('chain', cur), bypass, historyBtn, h('div', { class: 'grow' }), seg.el),
      h('div', { class: 'dim', style: 'margin-top:6px' }, chain.bypassed ? 'Nothing in the chain is on: the program passes unchanged (the safety limiter and, when it is on, the loudness controller still act).' : `Preset ${chainWord}${chain.custom && chain.user_preset ? ', with changes' : ''}. A change is a 50 ms crossfade to the new chain, with the old chain's state carried over.`)].filter(Boolean));
    return card;
  }

  // ---- layout, in chain order ----
  const order = () => {
    const o = ['trim', 'highpass', 'rotator', 'stereo', 'agc', 'eq', 'bass', 'multiband', 'wideband', 'makeup', 'clipper', 'LOUDNESS', 'limiter', 'SAFETY'];
    if (chain.blocks.eq.position === 'post') { o.splice(o.indexOf('eq'), 1); o.splice(o.indexOf('multiband') + 1, 0, 'eq'); }
    return o;
  };
  let loudEl = null;
  const rack = h('div', { class: 'prack' });
  const flow = h('div', { class: 'dim', style: 'margin:6px 2px' });
  async function repaintAll() {
    for (const k of Object.keys(meters)) delete meters[k];
    for (const k of Object.keys(cards)) delete cards[k];
    const top = topCard();
    if (view() === 'rack') { await repaintRack(top); return; }
    rack.classList.remove('rackv'); root.classList.remove('rackw');
    const kids = [];
    for (const k of order()) {
      if (k === 'LOUDNESS') { if (!loudEl) loudEl = await loudnessCard(s); kids.push(loudEl); }
      else if (k === 'SAFETY') kids.push(safetyCard());
      else kids.push(blockCard(k));
    }
    kids.forEach((c, i) => { c.dataset.idx = String(i + 1); const n = c.querySelector('.pidx'); if (n) n.textContent = String(i + 1); });
    flow.textContent = 'Signal flows through the cards in the order of their numbers: ' + order().map(k => k === 'LOUDNESS' ? 'loudness controller' : k === 'SAFETY' ? 'safety limiter' : schema[k].label.toLowerCase()).join(' > ') + ' > master gain > output trim.';
    rack.replaceChildren(...kids);
    rack.classList.toggle('list', view() === 'list');
    root.replaceChildren(top, flow, rack);
    relayout();
  }

  // ---- the Rack view: a strip for each block, one block open at a time ----
  // The strip stays as the header of its unit; the block's faceplate card (the same builder as the Faceplates view,
  // without its own nameplate) is mounted under it while it is open. The block open is kept in this browser.
  const rackKey = k => k === 'LOUDNESS' ? 'loudness' : k === 'SAFETY' ? 'safety' : k;
  const loudToggle = () => {
    const en = loudEl && loudEl.querySelector('input[data-k="enabled"]');
    if (!en) return;
    en.checked = !en.checked;
    en.dispatchEvent(new Event('change', { bubbles: true }));   // the card saves it, as for its own switch
    rackUpdate();
  };
  const loudPill = () => {
    const p = loudEl && loudEl.querySelector('[data-loud-pill]');
    const en = !!(loudEl && loudEl.querySelector('input[data-k="enabled"]')?.checked);
    if (!en) return ['OFF', ''];
    const t = p ? (p.label || p).textContent : '', cls = p ? [...p.classList].filter(c => ['ok', 'warn', 'info', 'dim'].includes(c)).join(' ') : '';
    return [t || 'ON', cls];
  };
  // what the strips show that the chain does not hold: the AGC's and the loudness controller's states, gains, target
  function rackExtras(f) {
    const x = { agc: null, loud: null, eqTap: anaTap };
    if (agcNow) {
      const v = agcNow, en = v.enabled, p = AGC_PILL[v.state];
      x.agc = { pill: en ? [p[0], p[1]] : ['BYPASSED', ''], state: v.state, on: en, gain: en ? (f ? f.stages[0].gr : v.gain_db) : null };
    }
    const pill = loudPill(), on = pill[0] !== 'OFF';
    x.loud = { pill, on, gain: on && f ? f.gain : null, target: f && f.target != null ? f.target : null };
    return x;
  }
  function rackUpdate(f) {
    if (view() !== 'rack') return;
    const x = rackExtras(f || null);
    for (const u of Object.values(rackUnits)) u.strip.update(chain, x, f || null);
  }
  function closeUnit(rk) {
    const u = rackUnits[rk];
    if (!u) return;
    const card = rk === 'loudness' ? loudEl : cards[rk];
    card?.remove();
    if (rk !== 'loudness') { delete cards[rk]; delete meters[rk]; }
    u.unit.classList.remove('open'); u.strip.setOpen(false);
  }
  function openUnit(rk) {
    const u = rackUnits[rk];
    if (!u) return;
    const card = rk === 'loudness' ? loudEl : rk === 'safety' ? safetyCard() : blockCard(rk);
    u.unit.append(card);
    u.unit.classList.add('open'); u.strip.setOpen(true);
  }
  function toggleOpen(rk) {
    const prev = openKey;
    if (prev) closeUnit(prev);
    openKey = prev === rk ? '' : rk;
    try { localStorage.setItem('procOpen', openKey); } catch { }
    if (!openKey) return;
    openUnit(openKey);
    // scroll only when the header would be off the screen
    requestAnimationFrame(() => {
      const r = rackUnits[rk]?.strip.el.getBoundingClientRect();
      const top = document.getElementById('topbar')?.getBoundingClientRect().bottom || 0;
      if (r && (r.top < top + 4 || r.bottom > innerHeight)) rackUnits[rk].strip.el.scrollIntoView({ block: 'start' });
    });
  }
  async function repaintRack(top) {
    for (const k of Object.keys(rackUnits)) delete rackUnits[k];
    // the closed equalizer unit draws the analyzer too: subscribe to the tap the card last chose
    try { const t = localStorage.getItem('eqAnalyzer.tap'); if ((t === 'in' || t === 'out') && anaTap === 'off') setAnaTap(t); } catch { }
    if (!loudEl) loudEl = await loudnessCard(s);
    const units = [];
    order().forEach((k, i) => {
      const rk = rackKey(k);
      const label = rk === 'loudness' ? 'Loudness controller' : rk === 'safety' ? 'Safety limiter' : schema[rk].label;
      let power, help;
      if (rk === 'loudness') {
        power = h('button', { type: 'button', class: 'pwr', 'data-power': 'loudness', 'aria-pressed': 'false', onclick: loudToggle }, procPowerIcon(), h('span', { class: 'pwr-t' }, ''));
        help = helpLink('output-and-level', 'loudness-controller');
      } else if (rk === 'safety') {
        power = h('span', { class: 'pwr fixed on', title: 'Always on: this block cannot be switched off' }, procPowerIcon(), h('span', { class: 'pwr-t' }, 'ALWAYS ON'));
        help = helpLink('processing', 'safety-limiter');
      } else { power = powerButton(rk, schema[rk]); help = helpLink('processing', `${rk}`); }
      const paintPower2 = el => {
        if (rk === 'loudness') {
          const on = loudPill()[0] !== 'OFF', b = el.querySelector('[data-power]');
          el.classList.toggle('off', !on);
          b.classList.toggle('on', on); b.setAttribute('aria-pressed', on);
          setText(b.querySelector('.pwr-t'), on ? 'ON' : 'OFF');
          b.title = on ? 'On. Click to switch the loudness controller off (saves at once).' : 'Off. Click to switch it on (saves at once).';
          b.setAttribute('aria-label', 'Loudness controller' + (on ? ': on, click to switch off' : ': off, click to switch on'));
        } else if (rk !== 'safety') paintPower(rk, el);
      };
      const st = ProcRack.strip({ key: rk, idx: i + 1, label, power, help, schema, bandNames: n => PROC_BAND_NAMES[n], paintPower: paintPower2, onToggle: () => toggleOpen(rk) });
      const unit = h('div', { class: 'runit', 'data-unit': rk }, st.el);
      rackUnits[rk] = { unit, strip: st };
      units.push(unit);
    });
    flow.textContent = 'Signal flows through the strips in the order of their numbers: ' + order().map(k => k === 'LOUDNESS' ? 'loudness controller' : k === 'SAFETY' ? 'safety limiter' : schema[k].label.toLowerCase()).join(' > ') + ' > master gain > output trim.';
    rack.replaceChildren(...units);
    rack.classList.remove('list'); rack.classList.add('rackv'); root.classList.add('rackw');
    root.replaceChildren(top, flow, rack);
    if (openKey && rackUnits[openKey]) openUnit(openKey); else openKey = '';
    rackUpdate();
  }

  // ---- the rack's grid ----
  // The rack is a CSS grid of 4 columns from 1600 px wide, 3 from 1200, 2 from 800 and 1 below (style.css). Each card
  // spans as many columns as its controls need on one line (measured: the card laid out at its max-content width, the
  // displays at their minimum), so no card is wider than its content needs. The rows are then filled in chain order:
  // the columns a row has left go to a card with a display (the equalizer's curve, the AGC's and the wideband's history,
  // the multiband's strips), which uses them; a card without one is never stretched; what no display can take is left
  // as an empty end of the row. The order of the cards (the signal order) never changes.
  const ELASTIC = new Set(['agc', 'eq', 'multiband', 'wideband', 'loudness']);
  let laidFor = '';
  function layoutRack() {
    if (!rack.isConnected) return;
    const kids = [...rack.children];
    if (view() !== 'face') { kids.forEach(c => { c.style.gridColumn = ''; }); return; }
    const cs = getComputedStyle(rack);
    const C = rackCols();
    const gap = parseFloat(cs.columnGap) || 0;
    const colW = (rack.clientWidth - gap * (C - 1)) / C;
    rack.classList.add('measuring');
    const natural = kids.map(c => c.getBoundingClientRect().width);
    rack.classList.remove('measuring');
    rack.dataset.natural = natural.map(Math.round).join(' ');
    const items = kids.map((c, i) => {
      const min = Math.min(C, Math.max(1, Math.ceil((natural[i] + gap) / (colW + gap) - 0.001)));
      return { min, elastic: ELASTIC.has(c.dataset.block) };
    });
    // best[i]: the cheapest way to lay out the cards from i on, starting a new row. A row costs 2, a column left
    // empty 6, a column given to a display 1 (a display is happy to have it, but a tighter rack is better).
    const n = items.length, best = Array(n + 1).fill(Infinity), pick = Array(n + 1);
    best[n] = 0;
    for (let i = n - 1; i >= 0; i--) {
      let sum = 0;
      for (let j = i; j < n; j++) {
        sum += items[j].min;
        if (sum > C && j > i) break;
        const row = items.slice(i, j + 1), extra = Math.max(0, C - sum), stretch = row.some(r => r.elastic) ? extra : 0;
        const cost = 2 + stretch + (extra - stretch) * 6 + best[j + 1];
        if (cost < best[i]) { best[i] = cost; pick[i] = j + 1; }
      }
    }
    for (let i = 0; i < n; i = pick[i]) {
      const row = items.slice(i, pick[i]);
      const spans = row.map(r => r.min);
      let extra = C - spans.reduce((a, b) => a + b, 0);
      const el = row.map((r, k) => r.elastic ? k : -1).filter(k => k >= 0);
      for (let k = 0; extra > 0 && el.length; k++, extra--) spans[el[k % el.length]]++;
      row.forEach((r, k) => { kids[i + k].style.gridColumn = 'span ' + spans[k]; });
    }
    laidFor = rack.clientWidth + ':' + C;
  }
  let relayoutRaf = 0;
  function relayout() { if (!relayoutRaf) relayoutRaf = requestAnimationFrame(() => { relayoutRaf = 0; layoutRack(); }); }
  // the column count is the stylesheet's (--cols); the grid's own track list also counts the implicit tracks a span
  // laid out for more columns leaves behind
  const rackCols = () => parseInt(getComputedStyle(rack).getPropertyValue('--cols'), 10) || 1;
  const rackRo = new ResizeObserver(() => { if (laidFor !== rack.clientWidth + ':' + rackCols()) relayout(); });
  rackRo.observe(rack);
  // List view: repaint that card (keeping the focus where the operator was), and the top card.
  function refresh(block) {
    if (block === 'eq' || !cards[block]) { repaintAll(); return; }
    const old = cards[block], a = document.activeElement;
    let sel = null;
    if (a && old.contains(a)) sel = a.dataset.list ? `input[data-list="${a.dataset.list}"][data-i="${a.dataset.i}"]` : a.dataset.k ? `[data-k="${a.dataset.k}"]` : null;
    delete meters[block];
    const fresh = blockCard(block);
    fresh.querySelector('.pidx').textContent = old.querySelector('.pidx')?.textContent || '';
    old.replaceWith(fresh);
    relayout();
    if (sel) fresh.querySelector(sel)?.focus();
    root.firstChild.replaceWith(topCard());
  }
  // Faceplates: take the station's answer into the controls where they stand, so a knob in the hand is never rebuilt.
  function syncBlock(block) {
    const c = cards[block];
    if (!c) {
      if (view() === 'rack') { rackUpdate(); root.firstChild.replaceWith(topCard()); } else repaintAll();
      return;
    }
    if (c.dataset.sig !== sig(block)) {
      if (block === 'eq') { repaintAll(); return; }
      delete meters[block];
      const fresh = blockCard(block);
      const ix = fresh.querySelector('.pidx'); if (ix) ix.textContent = c.querySelector('.pidx')?.textContent || '';
      c.replaceWith(fresh);
      relayout();
    } else if (settled(block)) c._sync();
    rackUpdate();
    root.firstChild.replaceWith(topCard());
  }
  await repaintAll();

  // ---- live meters ----
  let wsTimer = null;
  const dbOrNull = x => x == null ? null : x;
  const paintFrame = () => {
    wsTimer = null;
    const f = latest; if (!f) return;
    loudEl?._live?.(f);
    for (const [k, m] of Object.entries(meters)) {
      if (m.stage == null) continue;
      const st = f.stages[m.stage], en = k === 'safety' ? true : (f.stageMask >> m.stage & 1) === 1;
      m.lvIn.set(en ? dbOrNull(st.in) : null); m.lvOut.set(en ? dbOrNull(st.out) : null);
      if (m.gr) m.gr.set(en ? dbOrNull(st.gr) : null);
      if (m.hist) m.hist.push(en ? { in: dbOrNull(st.in), out: dbOrNull(st.out), gr: dbOrNull(st.gr), strip: m.strip } : { in: null, out: null, gr: null });
      if (m.gate) m.gate.classList.toggle('warn', !!(f.flags & 2));
      if (m.act && k === 'clipper') m.act.classList.toggle('warn', !!(f.flags & 16));
      if (m.act && k === 'limiter') m.act.classList.toggle('warn', !!(f.flags & 4));
      if (k === 'safety') cards.safety?.classList?.toggle('act', !!(f.flags & 8));
      if (k === 'multiband' && m.bands) m.bands.forEach((g, i) => g.set(en && f.bandGr[i] != null ? f.bandGr[i] : null));
      if (k === 'multiband' && m.curve) drawCurve(m.curve, f, en);
    }
    rackUpdate(f);
  };
  function drawCurve(cv, f, en) {
    const v = chain.blocks.multiband, xo = v.crossovers_hz;
    if (!curveTable || curveTable.n !== xo.length + 1 || curveTable.sig !== xo.join()) { curveTable = new Curve.BandTable(xo, Curve.logFreqs(256)); curveTable.sig = xo.join(); }
    const c = cv.getContext('2d'), W = cv.width, H = cv.height, range = 12;
    c.clearRect(0, 0, W, H); c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H);
    c.strokeStyle = '#242c3a'; c.lineWidth = 1; c.fillStyle = '#97a1b2'; c.font = '11px sans-serif';
    for (const hz of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) { const x = Curve.xFrac(hz) * W; c.beginPath(); c.moveTo(x, 0); c.lineTo(x, H); c.stroke(); c.fillText(hz >= 1000 ? hz / 1000 + 'k' : String(hz), x + 2, H - 3); }
    c.strokeStyle = '#5a6478'; xo.forEach(fc => { const x = Curve.xFrac(fc) * W; c.setLineDash([3, 3]); c.beginPath(); c.moveTo(x, 0); c.lineTo(x, H); c.stroke(); c.setLineDash([]); });
    const gr = (f.bandGr || []).slice(0, xo.length + 1).map(x => en && x != null ? x : 0);
    const db = curveTable.curveDb(gr, []);
    c.strokeStyle = '#f5a623'; c.lineWidth = 2; c.beginPath();
    db.forEach((d, i) => { const x = Curve.xFrac(curveTable.freqs[i]) * W, y = Math.min(H, Math.max(0, -d / range * H)); i ? c.lineTo(x, y) : c.moveTo(x, y); });
    c.stroke();
    c.fillStyle = '#97a1b2'; c.fillText('0 dB', 3, 11); c.fillText('-' + range + ' dB', 3, H - 14);
  }
  // The AGC's state, detectors and heading (GET /chain/agc), four times a second: the pill, the FAST and PROG bars against the
  // target line, the tick on the gain bar where the gain is heading, and the colour of the history's state strip.
  const AGC_PILL = { riding: ['RIDING', 'ok', '#2f9e6e'], gated: ['GATED', 'dim', '#4a566c'], riding_fade: ['FADE', 'info', '#4f7fe0'], clamping: ['CLAMPING', 'warn', '#f5a623'] };
  const agcPoll = setInterval(async () => {
    const m = meters.agc;
    if (document.hidden || !(view() === 'rack' || (m && m.pill))) return;
    let v; try { v = await J(`${base}/chain/agc`); } catch { return; }
    agcNow = v;
    if (!m || !m.pill) return;
    const en = v.enabled, p = AGC_PILL[v.state];
    setText(m.pill.label, en ? p[0] : 'BYPASSED'); m.pill.className = 'chip lr-pill fixw ' + (en ? p[1] : '');
    m.strip = en ? p[2] : null;
    m.fast.set(en ? v.fast_dbfs : null); m.prog.set(en ? v.program_dbfs : null);
    m.fast.setMark(v.target_dbfs); m.prog.setMark(v.target_dbfs);
    m.gr?.heading(en ? v.desired_db : null);
    setText(m.big, en ? (v.gain_db > 0 ? '+' : '') + v.gain_db.toFixed(1) + ' dB' : '--');
    setText(m.head, en ? '\u2192 ' + (v.desired_db > 0 ? '+' : '') + v.desired_db.toFixed(1) : '');
  }, 250);
  onCleanup(() => clearInterval(agcPoll));
  const wsUrl = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${base}/ws?topics=meters,chain`;
  const live = new Live(wsUrl, {
    meters(f) { latest = f; if (!wsTimer) wsTimer = setTimeout(paintFrame, 50); },
    spectrum(u8) { cards.eq?._eq?.feed(u8); rackUnits.eq?.strip.spectrum(u8); },
    chain(d) {
      // Another client (or the Stream Deck) changed the chain: take it in unless one of our controls is in use.
      if (JSON.stringify(d.blocks) === JSON.stringify(chain.blocks)) return;
      const using = Object.keys(cards).some(k => !settled(k) || (cards[k]._ctl && Object.values(cards[k]._ctl).some(w => w.busy())) || cards[k]._eq?.busy());
      if (using || root.querySelector('.pk-input') || (view() === 'list' && root.contains(document.activeElement))) return;
      adopt(d);
      if (view() === 'list') { repaintAll(); return; }
      for (const k of Object.keys(cards)) if (k !== 'safety') { if (cards[k].dataset.sig !== sig(k)) { repaintAll(); return; } cards[k]._sync(); }
      rackUpdate();
      root.firstChild.replaceWith(topCard());
    },
  });
  liveRef = live; anaSync();
  onCleanup(() => { live.stop(); clearTimeout(wsTimer); rackRo.disconnect(); });
  return root;
}
