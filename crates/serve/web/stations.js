// The Stations page (architecture 5.1, 9): create, rename and delete stations, and the first-run callout on On Air.
// Loaded after settings.js. Everything goes through /api/v1/station-admin (crates/serve/src/stations_http.rs); the
// authorization layer of the future login wraps that one prefix (docs/BUILDLOG.md, "Stations from the web page").
// On Air stays as it is for the operator: only a box with no station at all shows the callout there.
NAV.splice(Math.max(0, NAV.findIndex(n => n[0] === 'settings')), 0, ['stations', 'Stations']);
const SA = '/api/v1/station-admin';
const SA_NAME_RE = /^[a-z0-9][a-z0-9_-]*$/;

async function refreshStations() {
  const sl = await J('/api/v1/stations').catch(() => ({ stations: [] }));
  S.stations = sl.stations;
  if (!S.station || !S.stations.some(s => s.id === S.station)) S.station = S.stations[0]?.id || null;
  if (S.station) storeStation(S.station);
}
const slugify = t => t.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 32);
const streamUrlOf = (port, path) => `http://${location.hostname}:${port}${path || '/stream/program.ogg'}`;
function nameCheck(v, data, except) {
  if (!v) return 'The short name cannot be empty.';
  if (v.length > (data.max_name || 32)) return `At most ${data.max_name || 32} characters.`;
  if (!SA_NAME_RE.test(v)) return 'Use lowercase letters a to z, digits, - and _, starting with a letter or digit.';
  if ((data.reserved || []).includes(v)) return `"${v}" is reserved; choose another.`;
  if (data.stations.some(s => s.id === v && s.id !== except)) return `A station named "${v}" already exists.`;
  return '';
}
function modal(...kids) {
  const m = h('div', { class: 'sa-modal', 'data-modal': '' }, h('div', { class: 'card' }, kids));
  m.addEventListener('click', e => { if (e.target === m) m.remove(); });
  document.body.append(m);
  return m;
}
const outputText = s => ({ card: 'sound card', stream: 'network stream only', both: 'sound card and network stream', none: 'no output' }[s.output] || '');
function demoResult(j) {
  const url = j.stream_url || (j.station && streamUrlOf(j.station.port, j.station.stream?.path));
  return h('div', {}, h('div', { class: 'notice ok' }, `Station "${j.station.display_name}" is running on port ${j.station.port}.`),
    h('p', {}, 'It plays six generated test tones, 30 seconds each, with crossfades between them. Its only output is the network stream. Listen with a media player or OBS:'),
    h('p', {}, h('a', { href: url, class: 'mono', 'data-stream-url': '' }, url)),
    h('audio', { controls: true, src: url, preload: 'none', style: 'width:100%;max-width:520px' }),
    h('p', { class: 'sa-help' }, 'The player above works in Firefox. Chrome and Safari may not play Ogg FLAC; VLC and OBS do.'),
    (j.warnings || []).map(w => h('div', { class: 'notice' }, w)));
}

// ---- the first-run callout, in place of On Air when the box has no station ----
{
  const onair = routes.onair;
  routes.onair = async (a) => {
    if (S.stations.length) return onair(a);
    const root = h('div');
    root.append(h('h1', {}, 'On Air', helpLink('onair')));
    const out = h('div');
    const demoBtn = h('button', { 'data-demo': '', onclick: async () => {
      demoBtn.disabled = true; createBtn.disabled = true; demoBtn.textContent = 'Creating the demo: making test tones, starting the station...';
      const r = await safe(jsend(SA + '/demo', {}), 'Demo station');
      if (!r) { demoBtn.disabled = false; createBtn.disabled = false; demoBtn.textContent = 'Create a demo station'; return; }
      await refreshStations();
      out.replaceChildren(demoResult(r), h('div', { class: 'row', style: 'margin-top:10px' }, h('button', { class: 'primary', onclick: () => route() }, 'Go to On Air')));
      demoBtn.remove(); createBtn.remove();
    } }, 'Create a demo station');
    const createBtn = h('button', { class: 'primary', 'data-create': '', onclick: () => { location.hash = '#/stations/new'; } }, 'Create your first station');
    root.append(h('div', { class: 'card sa-callout', 'data-first-run': '' },
      h('h2', {}, 'No station yet'),
      h('div', { class: 'dim' }, 'A station is one program with its own playlists, output and port. Create one to start. Add music in the Library first or after: a new station plays its emergency playlist or silence until it has a playlist.'),
      h('div', { class: 'row' }, createBtn, demoBtn),
      h('div', { class: 'dim', style: 'margin-top:10px' }, 'The demo station plays generated test tones to a network stream, so you can hear something in one click. You can delete it later.', helpLink('stations', 'first-run'))), out);
    return root;
  };
}

routes.stations = async ([sub]) => {
  const data = await J(SA);
  if (sub === 'new') return wizard(data);
  const root = h('div');
  root.append(h('h1', {}, 'Stations', helpLink('stations')));
  if (!data.manageable) root.append(h('div', { class: 'notice bad' }, 'This box has no systemd, so this page cannot start, stop or create stations. Write the files by hand (see Help, Getting started).'));
  else if (!data.writable) root.append(h('div', { class: 'notice bad' }, 'The service user cannot write the stations directory, so creating, renaming and deleting are refused (see deploy.md, "Stations from the web page").'));
  root.append(h('div', { class: 'row', style: 'margin-bottom:12px' },
    h('button', { class: 'primary', 'data-new': '', onclick: () => { location.hash = '#/stations/new'; } }, 'Create a station'),
    h('button', { 'data-demo-page': '', title: 'A stream-only station on generated test tones', onclick: async (e) => {
      e.target.disabled = true; e.target.textContent = 'Creating the demo...';
      const r = await safe(jsend(SA + '/demo', {}), 'Demo station');
      if (r) { toast('Demo station created'); await refreshStations(); }
      route();
    } }, 'Create a demo station')));
  const list = h('div', { class: 'sa-grid', 'data-station-list': '' });
  if (!data.stations.length) list.append(h('div', { class: 'notice' }, 'No station is configured on this box.'));
  for (const s of data.stations) list.append(stationCard(s, data));
  root.append(list);
  if (data.archived.length) root.append(h('div', { class: 'card', style: 'margin-top:14px' }, h('h3', {}, 'Archived stations'),
    h('div', { class: 'dim' }, 'Stations deleted with "keep its data". Nothing here is used by the box. To bring one back, copy its file to the stations directory and its state folder back (Help, Stations).'),
    ...data.archived.map(a => h('div', { class: 'row', style: 'margin-top:6px' }, h('span', { class: 'mono' }, a.name), h('span', { class: 'dim' }, a.stamp), h('span', { class: 'mono dim' }, a.dir)))));
  return root;
};

function stationCard(s, data) {
  const running = s.unit_state === 'active';
  const hs = s.health ? (s.health.status || 'down') : 'down';
  const status = s.error ? h('span', { class: 'chip crit' }, 'file error')
    : h('span', { class: 'chip ' + (s.health?.http === 200 ? 'ok' : running ? 'warn' : 'crit'), 'data-status': '' }, running || s.health ? hs : 'stopped');
  const nameInput = h('input', { type: 'text', value: s.display_name, style: 'width:100%;max-width:260px', 'data-display-name': s.id });
  const meta = h('dl', { class: 'sa-meta' }, ...[
    h('dt', {}, 'Short name'), h('dd', { class: 'mono' }, s.id, ' ', h('span', { class: 'dim' }, `(unit hotclock-station@${s.id})`)),
    s.error ? [h('dt', {}, 'Problem'), h('dd', {}, s.error)] : [
      h('dt', {}, 'Port'), h('dd', {}, String(s.port)),
      h('dt', {}, 'Output'), h('dd', {}, outputText(s), s.device && s.device !== 'null' ? h('span', { class: 'mono dim' }, ' ' + s.device) : ''),
      s.stream?.enabled ? [h('dt', {}, 'Stream'), h('dd', { class: 'mono' }, h('a', { href: streamUrlOf(s.port, s.stream.path) }, streamUrlOf(s.port, s.stream.path)))] : null,
      h('dt', {}, 'Playlists'), h('dd', {}, s.default_playlist || h('span', { class: 'dim' }, '(no default)'), s.emergency_playlist ? h('span', { class: 'dim' }, ` / emergency: ${s.emergency_playlist}`) : ''),
      h('dt', {}, 'Style'), h('dd', {}, `${s.transition_preset} crossfade, processing ${s.processing_preset}${s.loudness ? ', loudness on' : ''}`)]].flat(3).filter(Boolean));
  // the save goes through the Settings page's endpoint, and its answer says what waits for a restart: the same notices
  const notices = h('div', { 'data-restart-notices': s.id });
  const save = h('button', { title: 'The Settings page does the same', onclick: async () => {
    const r = await safe(jsend(`/api/v1/settings/station/${s.id}`, { values: { display_name: nameInput.value.trim() || null } }, 'PUT'), 'Display name');
    if (!r) return;
    toast(r.saved ? 'Display name saved' : 'Nothing changed');
    notices.replaceChildren(...(r.saved ? restartNotices(r, 'station', s.id) : []));
    await refreshStations();
  } }, 'Save');
  return h('div', { class: 'card sa-station', 'data-station': s.id },
    h('div', {}, h('div', { class: 'sa-name' }, s.display_name, ' ', status),
      s.error ? '' : h('div', { class: 'row', style: 'margin-top:8px' }, h('span', { class: 'dim' }, 'Display name'), nameInput, save), notices),
    meta,
    h('div', { class: 'sa-actions' },
      h('button', { onclick: () => { S.station = s.id; storeStation(s.id); location.hash = '#/onair'; } }, 'On Air'),
      h('button', { onclick: () => { S.station = s.id; storeStation(s.id); location.hash = '#/settings'; } }, 'Settings'),
      running
        ? h('button', { 'data-stop': s.id, onclick: async () => { if (!confirm(`Stop ${s.display_name}? It goes off the air until you start it.`)) return; if (await safe(jsend(`${SA}/${s.id}/stop`, {}), 'Stop')) route(); } }, 'Stop')
        : h('button', { 'data-start': s.id, onclick: async () => { if (await safe(jsend(`${SA}/${s.id}/start`, {}), 'Start')) route(); } }, 'Start'),
      h('button', { 'data-rename': s.id, onclick: () => renameDialog(s, data) }, 'Rename short name...'),
      h('button', { class: 'danger', 'data-delete': s.id, onclick: () => deleteDialog(s) }, 'Delete...')));
}

// ---- rename ----
function renameDialog(s, data) {
  const input = h('input', { type: 'text', value: '', placeholder: 'new short name', 'data-new-name': '', style: 'width:100%' });
  const err = h('div', { class: 'sa-err' });
  const planBox = h('div');
  const go = h('button', { class: 'primary', 'data-rename-go': '', disabled: true }, 'Rename now');
  const show = h('button', { 'data-plan': '' }, 'Show what will change');
  const status = h('div', { class: 'dim' });
  let planned = '';
  const m = modal(h('h2', {}, `Rename the short name of "${s.display_name}"`),
    h('div', { class: 'dim' }, `Now: ${s.id}. The short name is the file name, the systemd unit, the data folder and part of the API address. The display name is separate and is not changed here.`),
    h('div', { style: 'margin:10px 0' }, input, err), planBox, status,
    h('div', { class: 'row', style: 'margin-top:12px' }, show, go, h('button', { onclick: () => m.remove() }, 'Cancel')));
  input.addEventListener('input', () => { planned = ''; go.disabled = true; planBox.replaceChildren(); err.textContent = nameCheck(input.value.trim(), data, s.id); });
  show.addEventListener('click', async () => {
    const v = input.value.trim();
    err.textContent = nameCheck(v, data, s.id);
    if (err.textContent) return;
    try {
      const r = await J(`${SA}/${s.id}/rename`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ new_name: v, dry_run: true }) });
      planBox.replaceChildren(planView(r.plan)); planned = v; go.disabled = false;
    } catch (e) { err.textContent = e.message; }
  });
  go.addEventListener('click', async () => {
    if (planned !== input.value.trim()) return;
    go.disabled = true; show.disabled = true; status.textContent = 'Renaming: stopping the station, moving its files, starting it under the new name...';
    try {
      const r = await J(`${SA}/${s.id}/rename`, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ new_name: planned }) });
      m.remove(); toast(`Renamed to ${r.renamed.to}. It was back on the air after ${r.seconds_to_healthy.toFixed(1)} s.`);
      if (S.station === s.id) { S.station = r.renamed.to; storeStation(S.station); }
      await refreshStations(); route();
    } catch (e) {
      status.replaceChildren(h('div', { class: 'notice bad' }, e.message)); go.disabled = false; show.disabled = false;
    }
  });
  input.focus();
}
function planView(p) {
  const c = p.changes, u = p.unchanged;
  const rows = c.library_rows.reduce((n, x) => n + x.rows, 0);
  return h('div', { 'data-plan-view': '' }, h('ul', { class: 'sa-plan' },
    h('li', {}, h('div', { class: 'k' }, 'Changes'), `Unit ${c.unit.from} becomes ${c.unit.to}.`),
    h('li', {}, `The file ${c.file.from} becomes ${c.file.to}, and its name key becomes "${c.file.name_key}".`),
    c.data_dir.exists ? h('li', {}, `Its data folder (queue, history, state) moves from ${c.data_dir.from} to ${c.data_dir.to}.`) : null,
    c.snapshots ? h('li', {}, `${c.snapshots} nightly snapshot file${c.snapshots === 1 ? '' : 's'} get the new name.`) : null,
    h('li', {}, rows ? `${rows} record${rows === 1 ? '' : 's'} in the library (${c.library_rows.map(x => `${x.table}: ${x.rows}`).join(', ')}) are pointed at the new name.` : 'The library holds no schedule or history records of this station to change.'),
    c.meter_override ? h('li', {}, 'Its own meter settings keep working under the new name.') : null,
    h('li', {}, h('div', { class: 'k' }, 'What stays'), `Its port (${u.port}) and display name. `, u.stream_path ? `The stream address stays ${streamUrlOf(u.port, u.stream_path)}, so OBS sources that read it keep working.` : 'It has no network stream.'),
    h('li', {}, h('div', { class: 'k' }, 'What you must change'), `Anything that reaches the station through the library, as ${c.api_path.from}, uses ${c.api_path.to} from now on (Stream Deck buttons, scripts). Monitoring of the unit name follows the new unit.`),
    h('li', {}, h('div', { class: 'k' }, 'On the air'), p.running ? 'The station is running. It goes off the air for a few seconds and comes back on the same item at the same place. If anything fails, everything is put back and the old station is started again.' : 'The station is not running now. It is started under the new name.')));
}

// ---- delete ----
function deleteDialog(s) {
  const confirmIn = h('input', { type: 'text', 'data-confirm': '', placeholder: s.id, autocomplete: 'off', style: 'width:100%' });
  const del = h('button', { class: 'danger', 'data-delete-go': '', disabled: true }, 'Delete station');
  const status = h('div');
  let data = 'keep';
  const radio = (v, title, text) => h('label', { class: 'sa-radio' }, h('input', { type: 'radio', name: 'sa-data', value: v, checked: v === 'keep', onchange: () => { data = v; } }), h('div', {}, h('div', { style: 'font-weight:600' }, title), h('div', { class: 'sa-help' }, text)));
  const m = modal(h('h2', {}, `Delete "${s.display_name}"`),
    h('div', {}, 'The station is stopped and its file is removed, so it no longer starts with the box.', s.unit_state === 'active' ? h('b', {}, ' It is running now and goes off the air.') : ''),
    h('div', { style: 'margin:10px 0' },
      radio('keep', 'Keep its data (archive)', 'Its file and data folder move to the archive folder. Its backup snapshots and its schedule and history records in the library stay. Nothing is lost.'),
      radio('delete', 'Delete its data too', 'Its file, data folder (queue, history, state), backup snapshots, and its schedule and history records in the library are removed. This cannot be undone. Playlists and music are not touched.')),
    h('div', {}, 'Type the short name ', h('span', { class: 'mono' }, s.id), ' to confirm:'), h('div', { style: 'margin:6px 0' }, confirmIn), status,
    h('div', { class: 'row', style: 'margin-top:12px' }, del, h('button', { onclick: () => m.remove() }, 'Cancel')));
  confirmIn.addEventListener('input', () => { del.disabled = confirmIn.value !== s.id; });
  del.addEventListener('click', async () => {
    del.disabled = true;
    try {
      const r = await J(`${SA}/${s.id}?confirm=${encodeURIComponent(confirmIn.value)}&data=${data}`, { method: 'DELETE', headers: { 'Content-Type': 'application/json' } });
      m.remove(); toast(r.data === 'keep' ? `Deleted ${s.id}; its data is in ${r.archive}` : `Deleted ${s.id} and its data`);
      await refreshStations(); route();
    } catch (e) { status.replaceChildren(h('div', { class: 'notice bad' }, e.message)); del.disabled = false; }
  });
  confirmIn.focus();
}

// ---- the create wizard ----
function wizard(data) {
  const root = h('div');
  root.append(h('h1', {}, 'Create a station', helpLink('stations', 'create-a-station')), h('div', { class: 'dim', style: 'margin-bottom:10px' }, 'A station is one program with its own output, port and playlists. The page writes its file, starts it and waits until it answers.'));
  const errs = {};
  const field = (key, label, control, help) => {
    const e = h('div', { class: 'sa-err', 'data-err': key }); errs[key] = e;
    return h('div', { class: 'sa-form', 'data-field': key }, h('label', {}, label), h('div', {}, control, e), h('div', { class: 'sa-help' }, help));
  };
  const display = h('input', { type: 'text', 'data-f': 'display_name', placeholder: 'Office Stream', maxlength: 60 });
  const short = h('input', { type: 'text', 'data-f': 'name', placeholder: 'office' });
  let shortTouched = false;
  display.addEventListener('input', () => { if (!shortTouched) { short.value = slugify(display.value); checkShort(); } });
  short.addEventListener('input', () => { shortTouched = true; checkShort(); });
  const checkShort = () => { errs.name.textContent = short.value ? nameCheck(short.value, data) : ''; };
  const cards = data.devices.filter(d => d.value !== 'null');
  let output = 'stream';
  const dev = h('select', { 'data-f': 'device' }, cards.map(d => h('option', { value: d.value }, d.label)));
  const devRow = h('div', { style: 'margin-top:6px;display:none' }, dev);
  const expect = h('input', { type: 'checkbox', 'data-f': 'expect_listener' });
  const expectRow = h('label', { class: 'sa-radio', style: 'margin-top:6px' }, expect, h('span', { class: 'sa-help' }, 'Raise an alert when nobody is reading the stream (leave off until a program such as OBS is set up).'));
  const loud = h('input', { type: 'checkbox', 'data-f': 'loudness' });
  const paintOut = () => { devRow.style.display = output === 'stream' ? 'none' : ''; expectRow.style.display = output === 'card' ? 'none' : ''; loud.checked = output !== 'card' ? loud.checked : false; };
  const radio = (v, title, text, off) => h('label', { class: 'sa-radio' }, h('input', { type: 'radio', name: 'out', value: v, checked: v === output, disabled: !!off, onchange: () => { output = v; paintOut(); } }), h('div', {}, h('div', {}, title), h('div', { class: 'sa-help' }, off || text)));
  const noCards = cards.length ? '' : 'No sound card was found on this box (cat /proc/asound/cards).';
  const outCtl = h('div', {}, radio('stream', 'Network stream only', 'No sound card. A program such as OBS reads the stream over the network.'), radio('card', 'Sound card', 'Plays to a sound card of this box.', noCards), radio('both', 'Sound card and stream', 'Both at once.', noCards), devRow, expectRow);
  const port = h('input', { type: 'number', 'data-f': 'port', value: data.next_port, min: 1024, max: 65535 });
  const plSel = k => h('select', { 'data-f': k }, h('option', { value: '' }, '(none yet)'), data.playlists.map(p => h('option', { value: p }, p)));
  const defPl = plSel('default_playlist'), emPl = plSel('emergency_playlist');
  const plWarn = h('div', { class: 'notice', 'data-pl-warn': '' }, data.playlists.length ? 'Without a default playlist the station plays its emergency playlist or silence until you start a playlist on On Air.' : 'The library has no playlists yet. You can create the station now and make playlists afterwards in Playlists, then set them in Settings (default_playlist, emergency_playlist).');
  const paintWarn = () => { plWarn.style.display = defPl.value ? 'none' : ''; };
  defPl.addEventListener('change', paintWarn); paintWarn();
  const preset = h('select', { 'data-f': 'transition_preset' }, data.presets.map(p => h('option', { value: p.name, selected: p.name === 'plexamp' }, `${p.name}: ${p.description}`)));
  const proc = h('select', { 'data-f': 'processing_preset' }, data.processing_presets.map(p => h('option', { value: p, selected: p === 'bypass' }, p)));
  const status = h('div');
  const create = h('button', { class: 'primary', 'data-create-go': '' }, 'Create station');
  const card = h('div', { class: 'card' },
    field('display_name', 'Display name', display, 'What people see: pages, the status screen, the stream title. You can change it any time.'),
    field('name', 'Short name', short, 'Lowercase letters, digits, - and _. It becomes the file name, the systemd unit and part of the API address. You can rename it later, but a rename briefly takes the station off the air.'),
    field('output', 'Output', outCtl, ''),
    field('port', 'Port', h('div', {}, port), `The station's own address. The next free port is filled in (the library uses ${data.library_port || 8300}).`),
    field('default_playlist', 'Default playlist', h('div', {}, defPl, plWarn), 'Played by a new station. A station resumes what it was doing after a restart, so this matters for a fresh start.'),
    field('emergency_playlist', 'Emergency playlist', emPl, 'Played in safe mode, after long silence and when the queue runs dry. Music that is safe at any time.'),
    field('transition_preset', 'Crossfade style', preset, 'How songs overlap. You can change it any time on On Air.'),
    field('processing_preset', 'Processing', proc, 'bypass leaves the program alone except for the safety limiter. gentle and radio add compression; edit them on the System page.'),
    field('loudness', 'Loudness control', h('label', { class: 'sa-radio' }, loud, h('span', {}, 'Hold the stream at a steady level (-14 LUFS)')), 'Recommended for a stream to YouTube or Twitch. Off for a station whose level you set by ear.'),
    h('div', { class: 'row', style: 'margin-top:14px' }, create, h('button', { onclick: () => { location.hash = '#/stations'; } }, 'Cancel')), status);
  root.append(card);
  paintOut();
  create.addEventListener('click', async () => {
    Object.values(errs).forEach(e => { e.textContent = ''; });
    const body = { name: short.value.trim(), display_name: display.value.trim(), output, port: +port.value || undefined, default_playlist: defPl.value, emergency_playlist: emPl.value, transition_preset: preset.value, processing_preset: proc.value, loudness: loud.checked, expect_listener: expect.checked };
    if (output !== 'stream') body.device = dev.value;
    const local = nameCheck(body.name, data);
    if (local) { errs.name.textContent = local; short.focus(); return; }
    create.disabled = true; status.replaceChildren(h('div', { class: 'dim' }, 'Creating: writing the file, starting the station, waiting for it to answer (up to 30 s)...'));
    try {
      const r = await J(SA, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
      await refreshStations();
      S.station = r.created; storeStation(r.created);
      card.replaceChildren(h('div', { 'data-created': r.created }, h('div', { class: 'notice ok' }, `Station "${r.station.display_name}" (${r.created}) is running on port ${r.station.port}. It answered after ${r.seconds_to_healthy.toFixed(1)} s.`),
        r.stream_url ? h('p', {}, 'Stream: ', h('a', { class: 'mono', href: r.stream_url }, r.stream_url)) : null,
        (r.warnings || []).map(w => h('div', { class: 'notice' }, w)),
        h('div', { class: 'row', style: 'margin-top:12px' }, h('button', { class: 'primary', onclick: () => { location.hash = '#/onair'; route(); } }, 'Go to On Air'), h('button', { onclick: () => { location.hash = '#/settings'; } }, 'Settings'), h('button', { onclick: () => { location.hash = '#/stations'; } }, 'All stations'))));
    } catch (e) {
      create.disabled = false;
      const body = e.body; // not kept by J; the message holds every field
      status.replaceChildren(h('div', { class: 'notice bad' }, e.message));
      for (const m of String(e.message).split('; ')) { const [k, ...rest] = m.split(': '); if (errs[k] && rest.length) errs[k].textContent = rest.join(': '); }
    }
  });
  return root;
}
