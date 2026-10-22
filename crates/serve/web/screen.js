// The box's screen (architecture 11.2b) as a web page: the kiosk fallback of the native display (11.4). It follows
// the library service's `display` topic (the view, set from the Stream Deck and the web UI), keeps its last view
// when the library service is down, and shows each station with the single-station page in a frame.
(async function () {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const pass = new URLSearchParams(location.search);
  const passthru = ['processing_view', 'spectrum_source', 'curve_range_db', 'bypassed_stages', 'curve_include_makeup', 'proc_range_db', 'proc_spectrum'].filter(k => pass.has(k)).map(k => `&${k}=${encodeURIComponent(pass.get(k))}`).join('');
  let stations = [];
  try { stations = (await J('/api/v1/stations')).stations.map(s => ({ id: s.id, name: s.display_name || s.id, crit: false, connected: false })); } catch { }
  // the stations in port order: the first is the left half of split
  stations.sort((a, b) => 0);
  const saved = (() => { try { return JSON.parse(localStorage.getItem('hotclock.display.view') || 'null'); } catch { return null; } })();
  let view = { view: 'single', station: null, rotate_s: 20, rotate_pin_on_alert: true, rotate_processing: false, ...(saved || {}) };
  // the kiosk URL can say it too: /display?rotate_processing=1 (the library's `display` payload carries the global file's value)
  const urlRotProc = pass.has('rotate_processing') ? ['1', 'true'].includes(pass.get('rotate_processing')) : null;
  const screen = $('#screen');
  const frames = [];
  stations.forEach((s, i) => {
    const other = stations.find((o, j) => j !== i)?.id;
    const q = `/display?station=${encodeURIComponent(s.id)}${other ? '&other=' + encodeURIComponent(other) : ''}${passthru}`;
    const full = h('iframe', { class: 'full', src: q, title: s.id });
    const hf = h('iframe', { class: 'half ' + (i === 0 ? 'l' : 'r'), src: q.replace('&other=' + other, '') + '&half=1', title: s.id + ' half' });
    screen.append(full); if (i < 2) screen.append(hf);
    // the station's processing view (11.2c) is the same page with ?view=processing, made when it is first wanted
    const fr = { full, half: hf, proc: null, procSrc: q + '&view=processing' };
    fr.needProc = () => { if (!fr.proc) { fr.proc = h('iframe', { class: 'pview', src: fr.procSrc, title: s.id + ' processing' }); screen.append(fr.proc); } return fr.proc; };
    frames.push(fr);
  });
  // the stations' alerts: a critical one pins a rotation (11.2b); the page watches each station for it
  for (const s of stations) {
    new Live(`${proto}://${location.host}/api/v1/stations/${s.id}/ws?topics=state,alerts`, {
      open() { s.connected = true; }, close() { s.connected = false; },
      snapshot(d) { s.alerts = d.alerts || []; s.crit = s.alerts.some(a => a.severity === 'critical'); },
      alert(d) { s.alerts = (s.alerts || []).filter(a => a.id !== d.id).concat([d]); s.crit = s.alerts.some(a => a.severity === 'critical'); },
      alert_cleared(d) { s.alerts = (s.alerts || []).filter(a => a.id !== d.id); s.crit = s.alerts.some(a => a.severity === 'critical'); },
    });
  }
  new Live(`${proto}://${location.host}/api/v1/ws?topics=display`, {
    snapshot(d) { if (d.display) apply(d.display); },
    display(d) { apply(d); },
  });
  function apply(v) {
    view = { ...view, ...v };
    try { localStorage.setItem('hotclock.display.view', JSON.stringify(view)); } catch { }
  }
  // the shared top bar of split: the product and version, the clock, the date, the box's CPU use and the network dot.
  // Which items show is the box's settings (display.bar_*), read with the numbers every 2 s.
  const bar = { items: { product: true, clock: true, date: true, cpu: true, network: false }, cpu: null, net: null };
  const prod = $('#prod'); prod.textContent = prod.textContent.toUpperCase();
  const DAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'], MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
  const dateText = d => `${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]}`;
  function drawBar() {
    const it = bar.items;
    prod.hidden = !it.product; $('#clk').hidden = !it.clock; $('#date').hidden = !it.date; $('#cpu').hidden = !it.cpu;
    const net = $('#net'); net.hidden = !it.network;
    net.className = bar.net === 'up' ? 'up' : bar.net === 'down' ? 'down' : '';
    net.title = bar.net === 'up' ? 'network: route out' : bar.net === 'down' ? 'network: no route out' : 'network: unknown';
    const cpu = $('#cpu'); cpu.textContent = bar.cpu == null ? 'CPU --' : `CPU ${bar.cpu}%`; cpu.classList.toggle('busy', bar.cpu != null && bar.cpu >= 85);
  }
  async function pollBar() {
    try {
      const b = await J('/api/v1/display/bar');
      bar.items = { ...bar.items, ...(b.items || {}) }; bar.cpu = b.cpu_percent ?? null; bar.net = b.network ?? null;
      prod.textContent = `${String(b.product || '').toUpperCase()} ${b.version || ''}`.trim();
    } catch { bar.cpu = null; bar.net = null; }
    drawBar();
  }
  drawBar(); pollBar(); setInterval(pollBar, 2000);
  // the presenter: the same rules as the native display
  let rotStart = null, pinned = null, lastView = null, lastRotProc = null;
  function shown(now) {
    const n = stations.length;
    const first = Math.max(0, stations.findIndex(s => s.id === view.station));
    // asked for by name, so also on a box with one station
    if (view.view === 'processing') return { view: 'processing', idx: n ? first : 0, frac: null, pinned: false, proc: true };
    if (n <= 1) return { view: 'single', idx: 0, frac: null, pinned: false, proc: false };
    if (view.view === 'split') return { view: 'split' };
    if (view.view === 'single') return { view: 'single', idx: first, frac: null, pinned: false, proc: false };
    const rotProc = urlRotProc ?? !!view.rotate_processing;
    if (lastView !== 'rotate' || lastRotProc !== rotProc) { rotStart = null; }
    lastRotProc = rotProc;
    const pinOn = view.rotate_pin_on_alert !== false;
    let pin = null;
    if (pinOn) pin = pinned != null && stations[pinned].crit ? pinned : stations.findIndex(s => s.crit);
    if (pin === -1) pin = null;
    if (pinned != null && pin == null) rotStart = now;
    pinned = pin;
    // a critical alert pins the station's normal view, where the alarms are
    if (pin != null) return { view: 'rotate', idx: pin, frac: 1, pinned: true, proc: false };
    const interval = Math.max(3, view.rotate_s || 20) * 1000;
    if (rotStart == null) rotStart = now;
    const el = Math.max(0, now - rotStart), step = Math.floor(el / interval), frac = (el % interval) / interval;
    // with the processing views in, each station shows twice in a row: its normal view, then its processing view
    if (rotProc) { const slot = (first * 2 + step) % (2 * n); return { view: 'rotate', idx: Math.floor(slot / 2), frac, pinned: false, proc: slot % 2 === 1 }; }
    return { view: 'rotate', idx: (first + step) % n, frac, pinned: false, proc: false };
  }
  function tick() {
    const now = Date.now(), sh = shown(now);
    lastView = sh.view;
    document.body.className = sh.view;
    document.body.dataset.shown = JSON.stringify({ view: sh.view, idx: sh.idx ?? null, pinned: !!sh.pinned, proc: !!sh.proc });
    const d = new Date(), dt = dateText(d);
    $('#clk').textContent = clock(d);
    if ($('#date').textContent !== dt) $('#date').textContent = dt;
    frames.forEach((f, i) => {
      f.full.classList.toggle('on', sh.view !== 'split' && !sh.proc && sh.idx === i);
      if (sh.proc && sh.idx === i) f.needProc();
      f.proc?.classList.toggle('on', !!sh.proc && sh.idx === i);
    });
    const rot = $('#rot'); rot.style.width = sh.view === 'rotate' ? (sh.frac * 100) + '%' : '0'; rot.classList.toggle('pinned', !!sh.pinned);
    requestAnimationFrame(tick);
  }
  window.__screen = { view: () => view, stations, shown, bar, pollBar };
  requestAnimationFrame(tick);
})();
