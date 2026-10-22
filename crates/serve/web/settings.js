// The Settings page and the crossfade-style pickers (architecture 2.6, 5.1, 9). Loaded after app.js: it adds its
// own route and nav entry and defines `crossfadePicker`, which app.js uses on On Air and System. Every control comes
// from the catalog the library service sends (`GET /api/v1/settings`), so a key added there appears here by itself.
// Authorization, when it exists, wraps /api/v1/settings*, /api/v1/system/restart and the station's live-settings
// and transition-preset endpoints (docs/BUILDLOG.md, "Transition preset pickers and Settings").
NAV.splice(NAV.length - 1, 0, ['settings', 'Settings']);

const APPLY = {
  live: ['live', 'ok', 'Takes effect at once.'],
  next_boundary: ['next boundary', 'ok', 'Takes effect from the next item planned; the one on air and the one already committed are not changed.'],
  station_restart: ['restart station', 'warn', 'The station reads this when it starts.'],
  library_restart: ['restart library', 'warn', 'The library service reads this when it starts.'],
  display_restart: ['restart display', 'warn', 'The status display reads this when it starts. Restarting it only blanks the screen for a second; the stations are not touched.'],
};
const GROUPS = [
  ['Identity', ['name', 'display_name', 'listen']],
  ['Sound output', ['device', 'rate', 'period_frames', 'periods', 'formats', 'rate_trim_ppm']],
  ['Loudness and crossfade', ['target_lufs', 'max_boost_db', 'peak_allowance_db', 'transition_preset', 'truncated_fade', 'truncated_fade_s', 'output_trim_db']],
  ['Playlists', ['default_playlist', 'emergency_playlist', 'stopped_is_normal', 'ram_loop_seconds']],
  ['Safety', ['lock_ahead_s', 'silence_dbfs', 'silence_s', 'restart_alert_clear_s', 'processing_preset']],
  ['Library', ['library']],
  ['Network stream', ['stream.enabled', 'stream.mount', 'stream.codec', 'stream.flac_level', 'stream.opus_kbps', 'stream.burst_ms', 'stream.max_listeners', 'stream.listener_lag_s', 'stream.expect_listener', 'stream.no_listener_alert_s', 'stream.send_icy_metadata']],
  ['Loudness controller', ['loudness.preset', 'loudness.enabled', 'loudness.target_lufs', 'loudness.true_peak_dbtp', 'loudness.window_s', 'loudness.max_slew_db_per_min', 'loudness.deadband_lu', 'loudness.gain_min_db', 'loudness.gain_max_db', 'loudness.min_valid_s', 'loudness.gate_categories', 'loudness.trim_tau_s', 'loudness.trim_range_db', 'loudness.within_item', 'loudness.item_memory_s', 'loudness.robust_items', 'loudness.item_cap_s', 'loudness.peak_allowance_db', 'loudness.max_limiter_gr_db']],
  ['Processing chain: Input trim', ['chain.trim.gain_db']],
  ['Processing chain: Input high-pass', ['chain.highpass.enabled', 'chain.highpass.freq_hz', 'chain.highpass.order']],
  ['Processing chain: Phase rotator', ['chain.rotator.enabled', 'chain.rotator.freq_hz', 'chain.rotator.sections']],
  ['Processing chain: Stereo enhancer', ['chain.stereo.enabled', 'chain.stereo.width']],
  ['Processing chain: Wideband AGC', ['chain.agc.enabled', 'chain.agc.target_dbfs', 'chain.agc.max_gain_db', 'chain.agc.max_reduction_db', 'chain.agc.down_db_per_s', 'chain.agc.up_db_per_s', 'chain.agc.detector_ms', 'chain.agc.program_ms', 'chain.agc.gate_db', 'chain.agc.fast_attack_ms', 'chain.agc.fast_trigger_db']],
  ['Processing chain: Equalizer', ['chain.eq.enabled', 'chain.eq.position', 'chain.eq.bands', 'chain.eq.b1_on', 'chain.eq.b1_type', 'chain.eq.b1_hz', 'chain.eq.b1_db', 'chain.eq.b1_q', 'chain.eq.b2_on', 'chain.eq.b2_type', 'chain.eq.b2_hz', 'chain.eq.b2_db', 'chain.eq.b2_q', 'chain.eq.b3_on', 'chain.eq.b3_type', 'chain.eq.b3_hz', 'chain.eq.b3_db', 'chain.eq.b3_q', 'chain.eq.b4_on', 'chain.eq.b4_type', 'chain.eq.b4_hz', 'chain.eq.b4_db', 'chain.eq.b4_q', 'chain.eq.b5_on', 'chain.eq.b5_type', 'chain.eq.b5_hz', 'chain.eq.b5_db', 'chain.eq.b5_q', 'chain.eq.b6_on', 'chain.eq.b6_type', 'chain.eq.b6_hz', 'chain.eq.b6_db', 'chain.eq.b6_q', 'chain.eq.b7_on', 'chain.eq.b7_type', 'chain.eq.b7_hz', 'chain.eq.b7_db', 'chain.eq.b7_q', 'chain.eq.b8_on', 'chain.eq.b8_type', 'chain.eq.b8_hz', 'chain.eq.b8_db', 'chain.eq.b8_q']],
  ['Processing chain: Bass enhancer', ['chain.bass.enabled', 'chain.bass.freq_hz', 'chain.bass.amount_pct', 'chain.bass.drive']],
  ['Processing chain: Multiband compressor', ['chain.multiband.enabled', 'chain.multiband.crossovers_hz', 'chain.multiband.threshold_dbfs', 'chain.multiband.ratio', 'chain.multiband.knee_db', 'chain.multiband.attack_ms', 'chain.multiband.release_ms', 'chain.multiband.makeup_db']],
  ['Processing chain: Wideband compressor', ['chain.wideband.enabled', 'chain.wideband.threshold_dbfs', 'chain.wideband.ratio', 'chain.wideband.knee_db', 'chain.wideband.attack_ms', 'chain.wideband.release_ms', 'chain.wideband.makeup_db']],
  ['Processing chain: Output makeup', ['chain.makeup.gain_db']],
  ['Processing chain: Clipper', ['chain.clipper.enabled', 'chain.clipper.threshold_dbfs', 'chain.clipper.softness_db']],
  ['Processing chain: True-peak limiter', ['chain.limiter.enabled', 'chain.limiter.ceiling_dbtp']],
  ['Global', ['data_dir', 'stations_dir', 'run_dir', 'time_zone', 'library.listen', 'brand.display_name', 'allowed_hosts', 'secrets_file']],
  ['Status display (the box\'s own monitor)', ['display.mode', 'display.diagonal_in', 'display.view_distance_cm', 'display.default_view', 'display.bar_product', 'display.bar_clock', 'display.bar_date', 'display.bar_cpu', 'display.bar_network', 'display.rotate_pin_on_alert', 'display.rotate_processing', 'display.proc_range_db', 'display.proc_spectrum', 'display.mini_processing', 'display.processing_view', 'display.spectrum_source', 'display.bypassed_stages', 'display.curve_range_db', 'display.curve_include_makeup', 'display.connector', 'display.device']],
];
const fmtDefault = v => v === null ? '(none)' : Array.isArray(v) ? (v.join(', ') || '(none)') : String(v);
const CROSSFADE_WORDS = { basic: 'plain crossfade', plexamp: 'Plexamp-style', sam: 'SAM-style', fm: 'FM-style', none: 'gapless, no overlap' };

// A select of the built-in crossfade styles for one station, with one line of help under it.
async function crossfadePicker(stationId, opts = {}) {
  const info = await safe(J(`/api/v1/stations/${stationId}/transition-preset`));
  if (!info) return h('div', { class: 'dim' }, 'Crossfade style: the station does not answer');
  const help = h('div', { class: 'dim', style: 'font-size:12px;margin-top:4px' });
  const sel = h('select', { 'data-crossfade': stationId }, info.presets.map(p => h('option', { value: p.name, selected: p.name === info.preset }, `${p.name} (${CROSSFADE_WORDS[p.name] || ''})`)));
  const paint = () => { const p = info.presets.find(x => x.name === sel.value); help.textContent = (p ? p.description : '') + (info.overridden || sel.value !== info.config_preset ? ` The station file says ${info.config_preset}.` : ''); };
  paint();
  sel.addEventListener('change', async () => {
    paint();
    const r = await safe(jsend(`/api/v1/settings/station/${stationId}`, { values: { transition_preset: sel.value } }, 'PUT'), 'Crossfade style');
    if (!r) return;
    info.config_preset = sel.value;
    paint();
    if (r.warning) toast(r.warning, true); else toast(`Crossfade style: ${sel.value}. It applies from the next boundary that is not already committed.`);
    if (opts.onchange) opts.onchange(sel.value);
  });
  return h('div', {}, h('div', { class: 'row' }, h('span', { class: 'dim' }, 'Crossfade style'), helpLink('crossfades', 'choosing-a-style'), sel), help);
}

routes.settings = async args => {
  const data = await J('/api/v1/settings');
  const root = h('div');
  root.append(h('h1', {}, 'Settings', helpLink('settings')));
  const tabs = [...data.stations.map(s => ({ id: s.id, label: 'Station ' + s.id, scope: 'station', block: s })), { id: '__global', label: 'This box', scope: 'global', block: data.global }];
  // `#/settings/<station or __global>#<anchor>`: a deep link (from On Air) opens that tab and points at one setting
  const [argTab, argAnchor] = String(args?.[0] || '').split('#');
  let cur = tabs.find(t => t.id === argTab) || tabs.find(t => t.id === S.station) || tabs[0];
  if (argTab && cur.scope === 'station') { S.station = cur.id; storeStation(cur.id); }
  let anchor = argAnchor || null;
  const body = h('div');
  const tabBar = h('div', { class: 'tabs' });
  const paintTabs = () => tabBar.replaceChildren(...tabs.map(t => h('button', { class: t === cur ? 'on' : '', onclick: () => { cur = t; if (t.scope === 'station') { S.station = t.id; storeStation(t.id); } paintTabs(); paint(); } }, t.label)));
  root.append(tabBar, body);
  paintTabs();
  paint();
  // the deep link: `transition_preset` outlines that row, `meters` the Meter ballistics card; both are scrolled into view
  // once the page is on the screen. Only the first paint does it (a Save repaints the page).
  if (anchor) {
    const pick = anchor === 'meters' ? '[data-card=meters]' : `[data-key="${anchor.replace(/[^a-z0-9_.]/gi, '')}"]`;
    anchor = null;
    setTimeout(() => { const el = $(pick, body); if (el) { el.classList.add('deeplink'); el.scrollIntoView({ block: 'center' }); } }, 60);
  }
  return root;

  function paint(summaryNodes) {
    const t = cur;
    const b = t.block;
    body.replaceChildren();
    if (b.error) { body.append(h('div', { class: 'notice' }, b.error)); return; }
    const defs = Object.fromEntries(data.catalog[t.scope].map(d => [d.key, d]));
    const controls = {};
    const removed = new Set();
    if (t.scope === 'station') body.append(h('div', { class: 'row', style: 'margin:6px 0' }, ConfigHistoryUI.button({ id: t.id, display_name: t.id }, () => route())));
    body.append(h('div', { class: 'dim', style: 'margin:6px 0' }, 'File ', h('span', { class: 'mono' }, b.file), b.writable ? '' : h('span', { class: 'chip crit', style: 'margin-left:8px' }, 'read only: the service cannot write it (see deploy.md, Settings)')));
    if (b.file_errors) body.append(h('div', { class: 'notice' }, 'The file does not pass the parser now: ', b.file_errors.map(e => `${e.key}: ${e.message}`).join('; ')));
    const result = h('div', {}, summaryNodes || []);
    const save = h('button', { class: 'primary', 'data-save': t.id }, 'Save');
    const nStation = GROUPS.findIndex(g => g[0] === 'Global');
    const groups = t.scope === 'station' ? GROUPS.slice(0, nStation) : GROUPS.slice(nStation);
    // A key of the catalog that no group names still shows up (the Help's settings reference does the same).
    { const named = new Set(groups.flatMap(g => g[1])), left = data.catalog[t.scope].map(d => d.key).filter(k => !named.has(k)); if (left.length) groups.push(['Other', left]); }
    for (const [title, keys] of groups) {
      const card = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, title, helpLink('settings-reference', `${t.scope}-${title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')}`)));
      for (const k of keys) if (defs[k]) card.append(fieldRow(defs[k]));
      body.append(card);
    }
    if (t.scope === 'global') body.append(meterBallisticsCard(data.stations.map(x => x.id)));
    const pendingKeys = t.scope === 'global' ? data.pending.display : data.pending.station;
    if (pendingKeys.length) body.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Keys that arrive with other milestones'),
      h('div', { class: 'dim' }, 'In the design and not in this build; each appears here when its code is installed: ' + pendingKeys.join(', '))));
    body.append(h('div', { class: 'row', style: 'margin-top:14px' }, save, h('span', { class: 'dim' }, 'The file keeps its comments; the previous one is kept as a .bak beside it.')), result);
    if (t.scope === 'station') categoriesCard();

    function fieldRow(d) {
      const explicit = d.key in b.values;
      const eff = b.effective[d.key];
      const row = h('div', { class: 'srow', 'data-key': d.key });
      const label = h('div', {}, h('div', { class: 'mono' }, d.key), h('div', {}, applyChip(d.apply), explicit ? '' : h('span', { class: 'dim', style: 'margin-left:6px' }, 'default')));
      const err = h('div', { class: 'dim', style: 'color:var(--red)' });
      const c = control(d, eff);
      const reset = h('button', { title: 'Remove the key from the file; the default applies', onclick: () => { removed.add(d.key); c.set(d.default); row.classList.add('dirty'); } }, 'default');
      controls[d.key] = { c, err, d, initial: eff, explicit };
      row.addEventListener('input', () => { removed.delete(d.key); row.classList.add('dirty'); });
      row.addEventListener('change', () => { removed.delete(d.key); row.classList.add('dirty'); });
      row.append(label, h('div', {}, h('div', { class: 'row' }, c.el, d.kind === 'readonly' ? null : reset), err),
        h('div', { class: 'dim' }, d.help, h('div', { style: 'font-size:12px' }, 'Default: ', fmtDefault(d.default), (d.kind === 'int' || d.kind === 'float') && d.max ? `  (${d.min} to ${d.max})` : '')));
      return row;
    }
    function applyChip(a) { const [txt, cls, tip] = APPLY[a] || [a, '', '']; return h('span', { class: 'chip ' + cls, title: tip }, txt); }
    function control(d, v) {
      switch (d.kind) {
        case 'readonly': return { el: h('span', { class: 'mono' }, b.values[d.key] ?? t.id), get: () => undefined, set: () => { } };
        case 'bool': { const e = h('input', { type: 'checkbox', checked: !!v }); return { el: e, get: () => e.checked, set: x => { e.checked = !!x; } }; }
        case 'int': case 'float': {
          if (d.options.length) { const e = h('select', {}, d.options.map(o => h('option', { value: o, selected: +o === +v }, o))); return { el: e, get: () => +e.value, set: x => { e.value = String(x); } }; }
          const e = h('input', { type: 'number', value: v, step: d.kind === 'int' ? 1 : 'any', min: d.min, max: d.max || null, style: 'width:140px' });
          return { el: e, get: () => e.value === '' ? null : +e.value, set: x => { e.value = x; } };
        }
        case 'device': return selectWithOther(data.devices.map(x => [x.value, x.label]), v);
        case 'playlist': return selectWithOther([['', '(none)'], ...data.playlists.map(n => [n, n])], v ?? '', true);
        case 'preset': { const e = h('select', {}, data.presets.map(p => h('option', { value: p.name, selected: p.name === v }, `${p.name}: ${p.description}`))); return { el: e, get: () => e.value, set: x => { e.value = x; } }; }
        case 'choice': { const e = h('select', {}, d.options.map(o => h('option', { value: o, selected: o === v }, o))); return { el: e, get: () => e.value, set: x => { e.value = x; } }; }
        case 'numlist': { const e = h('input', { type: 'text', value: (v || []).join(', '), style: 'width:100%', placeholder: '120, 500, 2000, 6000' }); return { el: e, get: () => e.value.split(',').map(x => x.trim()).filter(Boolean).map(Number), set: x => { e.value = (x || []).join(', '); } }; }
        case 'formats': case 'list': { const e = h('input', { type: 'text', value: (v || []).join(', '), style: 'width:100%', placeholder: d.kind === 'formats' ? 'S32_LE, S24_3LE, S16_LE' : 'name1, name2' }); return { el: e, get: () => e.value.split(',').map(x => x.trim()).filter(Boolean), set: x => { e.value = (x || []).join(', '); } }; }
        default: { const e = h('input', { type: 'text', value: v ?? '', style: 'width:100%' }); return { el: e, get: () => e.value, set: x => { e.value = x ?? ''; } }; }
      }
    }
    function selectWithOther(opts, v, onlyNames) {
      const known = opts.some(o => o[0] === v);
      const e = h('select', {}, opts.map(o => h('option', { value: o[0], selected: o[0] === v }, o[1])), h('option', { value: '__other', selected: !known }, 'Other...'));
      const txt = h('input', { type: 'text', value: known ? '' : v, style: 'width:100%;margin-top:4px;display:' + (known ? 'none' : 'block'), placeholder: onlyNames ? 'a playlist name' : 'hw:CARD=...,DEV=0 or file:/path.wav' });
      e.addEventListener('change', () => { txt.style.display = e.value === '__other' ? 'block' : 'none'; });
      return { el: h('div', { style: 'width:100%' }, e, txt), get: () => e.value === '__other' ? txt.value : e.value, set: x => { const k = opts.some(o => o[0] === x); e.value = k ? x : '__other'; txt.value = k ? '' : (x ?? ''); txt.style.display = k ? 'none' : 'block'; } };
    }
    function changes() {
      const out = {};
      for (const [k, x] of Object.entries(controls)) {
        if (x.d.kind === 'readonly') continue;
        if (removed.has(k)) { if (x.explicit) out[k] = null; continue; }
        const v = x.c.get();
        const empty = z => z === '' || z === null || z === undefined;
        if (JSON.stringify(v) === JSON.stringify(x.initial) || (empty(v) && empty(x.initial))) continue;
        out[k] = empty(v) ? null : v;
      }
      return out;
    }
    save.addEventListener('click', async () => {
      Object.values(controls).forEach(x => { x.err.textContent = ''; });
      const values = changes();
      if (!Object.keys(values).length) { toast('Nothing changed'); return; }
      const url = t.scope === 'station' ? `/api/v1/settings/station/${t.id}` : '/api/v1/settings/global';
      let r;
      try { r = await jsend(url, { values }, 'PUT'); } catch (e) {
        result.replaceChildren(h('div', { class: 'notice', 'data-error': '1' }, e.message));
        toast(e.message, true);
        return;
      }
      toast(r.saved ? 'Saved' : 'Applied, not saved', !!r.warning);
      const fresh = await J('/api/v1/settings');
      Object.assign(data, fresh);
      t.block = t.scope === 'station' ? fresh.stations.find(s => s.id === t.id) : fresh.global;
      paint(summary(r));
    });
    function summary(r) {
      const out = [];
      const lines = r.changed.map(c => h('div', {}, h('span', { class: 'mono' }, c.key), ' = ', h('span', { class: 'mono' }, JSON.stringify(c.value)), ' ', h('span', { class: 'chip ' + (c.applied ? 'ok' : (c.apply === 'live' || c.apply === 'next_boundary') ? 'crit' : 'warn') }, c.applied ? 'applied now' : (c.apply === 'live' || c.apply === 'next_boundary') ? 'not applied (station not answering)' : 'waits for a restart')));
      out.push(h('div', { class: 'card', style: 'margin-top:14px', 'data-summary': '1' }, h('h3', {}, 'Saved'), ...lines, r.backup ? h('div', { class: 'dim' }, 'Previous file: ' + r.backup) : null, r.warning ? h('div', { class: 'notice' }, r.warning) : null));
      out.push(...restartNotices(r, t.scope, t.id));
      return out;
    }
    async function categoriesCard() {
      const cats = (await safe(J('/api/v1/categories')))?.categories || [];
      const card = h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Category crossfade style', helpLink('crossfades', 'category-and-item-styles')),
        h('div', { class: 'dim' }, 'A category can name a style of its own; the station\'s style is used when it says inherit. Patches (single parameters per category) are not editable here yet.'));
      for (const c of cats) {
        const sel = h('select', { 'data-category': c.id }, h('option', { value: '' }, 'inherit (station)'), data.presets.map(p => h('option', { value: p.name, selected: c.transition_preset === p.name }, p.name)));
        sel.addEventListener('change', async () => {
          const r = await safe(jsend(`/api/v1/categories/${c.id}`, { transition_preset: sel.value || null }, 'PATCH'), 'Category');
          if (r) toast(`${c.name}: ${sel.value || 'inherit'}`);
        });
        card.append(h('div', { class: 'row', style: 'padding:4px 0' }, h('span', { style: 'min-width:160px' }, c.name), sel, Object.keys(c.transition_patch || {}).length ? h('span', { class: 'chip', title: JSON.stringify(c.transition_patch) }, 'has a patch') : null));
      }
      body.append(card);
    }
  }
};

// The notices after a save of settings (`PUT /api/v1/settings/...`): the server's answer says which changed keys wait
// for which restart (`station_restart`, `display_restart`, `library_restart`); each list gets its notice and button.
// Used by the Settings page and by the Stations page's display name.
function restartNotices(r, scope, stationId) {
  const out = [];
  if (r.station_restart?.length && scope === 'station') out.push(h('div', { class: 'notice' }, 'These need the station restarted: ' + r.station_restart.join(', ') + '. ',
    h('button', { class: 'danger', 'data-restart': 'station', onclick: () => restartStation(stationId) }, 'Restart station now'), ' It is silent for a few seconds; it restarts at once, not at a song boundary.'));
  if (r.display_restart?.length && scope === 'global') out.push(h('div', { class: 'notice' }, 'These need the status display restarted: ' + r.display_restart.join(', ') + '. ',
    h('button', { class: 'danger', 'data-restart': 'display', onclick: restartDisplay }, 'Restart display now'), ' The screen goes blank for a second; no station is touched.'));
  if (r.library_restart?.length) out.push(h('div', { class: 'notice' }, 'These need the library service restarted: ' + r.library_restart.join(', ') + '. ',
    h('button', { class: 'danger', 'data-restart': 'library', onclick: restartLibrary }, 'Restart library service now'), ' This page reconnects when it is back. Stations keep playing.'));
  return out;
}

async function restartStation(id) {
  if (!confirm(`Restart station ${id} now? It is silent for a few seconds.`)) return;
  toast('Restarting ' + id + '...');
  const r = await safe(jsend('/api/v1/system/restart', { unit: 'station', name: id }), 'Restart');
  if (r) toast(`${id} is healthy again after ${Math.round(r.seconds)} s`);
}
async function restartLibrary() {
  if (!confirm('Restart the library service now? The stations keep playing.')) return;
  const r = await safe(jsend('/api/v1/system/restart', { unit: 'library' }), 'Restart');
  if (!r) return;
  toast('Restarting the library service...');
  for (let i = 0; i < 60; i++) { await new Promise(x => setTimeout(x, 1000)); try { await J('/health'); toast('The library service is back'); route(); return; } catch { } }
  toast('The library service did not come back within a minute', true);
}

async function restartDisplay() {
  const r = await safe(jsend('/api/v1/system/restart', { unit: 'display' }), 'Restart');
  if (r) toast('The status display restarted');
}

// ---- meter ballistics (architecture 11.2): the box default and one row per station -----------------------------------
// Each row edits one setting through /api/v1/display/meters (the box default, or ?station=); a station that has none of
// its own inherits the box default. Applied to the native display and to every web meter within a second.
function meterRow(title, station) {
  const row = h('div', { class: 'row', style: 'padding:8px 0;border-top:1px solid var(--line);align-items:flex-start', 'data-meter-row': station || 'default' });
  const url = () => '/api/v1/display/meters' + (station ? '?station=' + encodeURIComponent(station) : '');
  const fld = (label, key, step, unit, d) => h('label', { class: 'dim' }, label + ' ', h('input', { type: 'number', step, value: d[key], style: 'width:6em', onchange: ev => put({ [key]: +ev.target.value }) }), ' ' + unit);
  const paint = d => row.replaceChildren(h('span', { style: 'min-width:130px' }, title),
    station ? (d.inherits ? h('span', { class: 'chip' }, 'inherits box default') : h('button', { title: 'Drop this station\'s own setting', onclick: () => put({ inherit: true }) }, 'Use box default')) : null,
    h('select', { onchange: ev => put({ preset: ev.target.value }) }, d.presets.map(p => h('option', { value: p.preset, selected: p.preset === d.preset }, p.label))),
    ...(d.preset === 'custom' ? [
      h('select', { onchange: ev => put({ source: ev.target.value }) }, [['peak', 'sample peak'], ['ppm', 'PPM as measured']].map(([v, l]) => h('option', { value: v, selected: d.source === v }, l))),
      fld('attack', 'attack_ms', 1, 'ms for 20 dB', d), fld('falloff', 'release_db_s', 1, 'dB/s', d),
      h('label', { class: 'dim' }, h('input', { type: 'checkbox', checked: d.hold, onchange: ev => put({ hold: ev.target.checked }) }), ' peak hold'),
      fld('hold', 'hold_ms', 100, 'ms', d), fld('tick falls', 'hold_fall_db_s', 1, 'dB/s (0: drops)', d)] : []),
    // the processing view's gain-reduction bars: instant attack, their own release and peak tick, whatever the preset
    h('span', { class: 'dim', title: 'The gain-reduction bars of the processing view' }, 'GR bars'),
    fld('release', 'gr_release_db_s', 1, 'dB/s', d), fld('tick', 'gr_hold_ms', 100, 'ms (0: none)', d));
  const put = async body => { const r = await safe(jsend(url(), body, 'PUT'), 'Meters'); if (r) paint(r); };
  J(url()).then(paint).catch(() => { });
  return row;
}
function meterBallisticsCard(stationIds) {
  return h('div', { class: 'card', style: 'margin-top:14px', 'data-card': 'meters' }, h('h3', {}, 'Meter ballistics', helpLink('display', 'meter-ballistics')),
    h('div', { class: 'dim' }, 'How the program bars rise and fall on the status display and in the web meters, and how the gain-reduction bars of the processing view let go. The frame carries the measured PPM and the sample peak; each viewer applies this. Changes apply within a second, with no restart, and are kept across restarts.'),
    meterRow('Box default', null), ...stationIds.map(id => meterRow('Station ' + id, id)));
}
