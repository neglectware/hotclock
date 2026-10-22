// The status display page (architecture 11): one station, full screen, dark, large type. It is the kiosk fallback of
// the native display (11.4) and draws the same layouts: the full layout of 11.2 from 720 px high, the compact
// 1024 x 600 layout of 11.2b under that, and with `?half` one half of the `split` view (the library's /display page
// puts two of them side by side). It is a client of the station's WebSocket and meter frames and computes elapsed
// and remaining itself at its own frame rate.
(async function () {
  const params = new URLSearchParams(location.search);
  const half = params.has('half');
  if (half) document.body.classList.add('half');
  // `?view=processing`: the processing view (architecture 11.2c) instead of the single view, drawn by procview.js
  const procMode = params.get('view') === 'processing';
  if (procMode) document.body.classList.add('pvmode');
  let apiBase = document.body.dataset.api;
  if (apiBase.includes('{station}')) {
    let st = params.get('station');
    if (!st) { const l = await J('/api/v1/stations'); st = l.stations[0]?.id; }
    apiBase = apiBase.replace('{station}', st);
  }
  const api = p => apiBase + '/' + p;
  // The processing view's settings (the native display reads them from the box's config; here they are the URL's).
  const cfg = {
    pv: ['spectrum', 'curve', 'both'].includes(params.get('processing_view')) ? params.get('processing_view') : 'both',
    ss: ['pre', 'post', 'both'].includes(params.get('spectrum_source')) ? params.get('spectrum_source') : 'post',
    range: Math.min(18, Math.max(3, +params.get('curve_range_db') || 12)),
    byp: params.get('bypassed_stages') === 'hide' ? 'hide' : 'show',
    makeup: params.get('curve_include_makeup') === 'true',
  };
  // The processing view's settings: the URL's, else the box's (`GET /api/v1/display` carries the keys of the global file).
  const pcfg = { range: 18, spectrum: false, ss: cfg.ss, byp: cfg.byp };
  const urlHas = k => params.has(k);
  async function loadDisplayCfg() {
    try {
      const d = await J('/api/v1/display');
      if (!urlHas('proc_range_db') && d.proc_range_db != null) pcfg.range = d.proc_range_db;
      if (!urlHas('proc_spectrum') && d.proc_spectrum != null) pcfg.spectrum = !!d.proc_spectrum;
      if (!urlHas('spectrum_source') && d.spectrum_source) pcfg.ss = d.spectrum_source;
      if (!urlHas('bypassed_stages') && d.bypassed_stages) pcfg.byp = d.bypassed_stages;
    } catch { }
  }
  if (urlHas('proc_range_db')) pcfg.range = Math.min(30, Math.max(6, +params.get('proc_range_db') || 18));
  if (urlHas('proc_spectrum')) pcfg.spectrum = ['1', 'true'].includes(params.get('proc_spectrum'));
  if (procMode) { loadDisplayCfg(); setInterval(loadDisplayCfg, 10000); }
  const S = { state: null, now: null, skew: 0, frame: null, meterAt: 0, asrun: [], connected: false, chain: null, chainSeen: null, chainAt: 0, other: null };
  // The client's part of the spectrum (11.2a): bars rise at once and fall 15 dB/s, the cap waits 1 s and falls 20 dB/s;
  // sample peaks are held 2 s.
  const D = { post: new Float32Array(64).fill(-90), pre: new Float32Array(64).fill(-90), capPost: new Float32Array(64).fill(-90), capPre: new Float32Array(64).fill(-90),
    capPostAt: new Float64Array(64), capPreAt: new Float64Array(64), hold: [null, null], holdAt: [0, 0], last: 0 };
  const bars = new Ballistics.Bars(); Ballistics.follow(apiBase.includes('/stations/') ? apiBase.split('/').pop() : null);
  const mcv = $('#mcv'), scv = $('#scv'), stcv = $('#stcv');
  // the loudness controller's gain range (the station's settings), the scale of its bar
  const LC = { min: null, max: null };
  async function loadLoudness() {
    try { const j = await J(api('loudness')); LC.min = j.settings.gain_min_db; LC.max = j.settings.gain_max_db; } catch { }
  }
  if (!procMode) { loadLoudness(); setInterval(loadLoudness, 15000); }
  const compactQ = matchMedia('(max-height: 719px)');
  const compact = () => compactQ.matches || half;
  compactQ.addEventListener?.('change', () => { renderState(); });

  function art(url) {
    const box = $('#art');
    if (url) { if (box.tagName !== 'IMG') { const i = h('img', { id: 'art' }); box.replaceWith(i); } $('#art').src = url.replace('size=144', 'size=600'); }
    else if (box.tagName === 'IMG') box.replaceWith(h('div', { class: 'noart', id: 'art' }));
  }
  function renderState() {
    const s = S.state; if (!s) return;
    $('#stname').textContent = s.display_name || s.station;
    const pill = $('#stpill');
    const label = !S.connected ? 'NO CONNECTION' : s.safe_mode ? 'SAFE MODE' : s.state === 'playing' ? (s.mode === 'assist' ? 'ASSIST' : 'PLAYING') : s.hold ? 'HELD' : s.state.toUpperCase();
    pill.textContent = label; pill.className = 'pillstate ' + (!S.connected || s.safe_mode ? 'safe' : s.mode === 'assist' && s.state === 'playing' ? 'assist' : s.hold ? 'held' : s.state);
    $('#hd').classList.toggle('crit', !S.connected || (s.alerts || []).some(a => a.severity === 'critical'));
    renderQueue(); renderAlarms();
  }
  function renderNow() {
    const n = S.now;
    if (!n) { $('#title').textContent = 'Nothing on air'; $('#artist').textContent = ''; $('#album').textContent = ''; $('#chips').replaceChildren(); art(null); return; }
    $('#title').textContent = n.title; $('#artist').textContent = n.artist || ''; $('#album').textContent = n.album || '';
    $('#chips').replaceChildren(...[n.category ? h('span', { class: 'chip' }, n.category.toUpperCase()) : null, n.continuity_next ? h('span', { class: 'chip ok' }, 'ALBUM') : null].filter(Boolean));
    art(n.art_url);
  }
  const ORIGIN = { rotation: 'ROT', liner: 'LINER', id: 'ID', block: 'BLK', manual: 'MAN' };
  // The back-timing line (M5): `END +3 s` and its state, above the queue, when a program is back-timing a play block.
  const BT_STATE = { planning: 'PLANNING', on_target: 'ON TARGET', within_tolerance: 'WITHIN TOLERANCE', fallback: 'FALLBACK' };
  function renderEnd() {
    const bt = S.program?.back_time;
    let el = $('#endline');
    if (!bt || !bt.state || bt.state === 'off') { if (el) el.remove(); return; }
    if (!el) { el = h('div', { class: 'qr', id: 'endline' }, h('span', { class: 'bd blk' }, 'END'), h('div', { class: 'nm' })); $('#qrows').before(el); }
    const txt = (bt.label || 'END') + '  ' + (BT_STATE[bt.state] || String(bt.state).toUpperCase());
    setText(el.querySelector('.nm'), txt);
    el.querySelector('.nm').className = 'nm ' + (bt.state === 'fallback' ? 'red' : bt.state === 'within_tolerance' ? 'amber' : '');
  }
  function renderQueue() {
    renderEnd();
    const s = S.state; const box = $('#qrows'); box.replaceChildren();
    const rows = half ? 1 : compact() ? 2 : 6;
    const list = (s?.queue || []).slice(0, rows);
    const at = S.queueAt || Date.now();
    for (const e of list) {
      const block = e.kind === 'block';
      const left = block && e.in_ms != null ? Math.max(0, e.in_ms - (Date.now() - at)) : null;
      box.append(h('div', { class: 'qr' },
        block ? h('span', { class: 'bd blk' }, String(e.badge || e.type || e.block_type || 'BLK').toUpperCase()) : (e.art_url ? h('img', { class: 'thumb', src: e.art_url }) : h('div', { class: 'thumb' })),
        h('div', { class: 'nm' }, block ? (e.name || e.type || e.block_type || '') : (e.title || ''), e.artist && !compact() ? h('span', { class: 'dim' }, '  ' + e.artist) : null),
        !block && e.origin && ORIGIN[e.origin] ? h('span', { class: 'bd' }, ORIGIN[e.origin]) : null,
        block ? h('div', { class: 'tm ' + (left != null && left < 10000 ? 'red' : left != null && left < 60000 ? 'amber' : ''), 'data-left': left ?? '' }, left != null ? mmss(left) : '')
          : h('div', { class: 'tm' }, e.planned_start ? hhmm(e.planned_start).slice(0, 5) : '')));
    }
    if (!list.length) box.append(h('div', { class: 'dim' }, 'Queue empty'));
  }
  function renderAlarms() {
    const box = $('#alrows'); box.replaceChildren();
    const a = [...(S.state?.alerts || [])].sort((x, y) => (y.severity === 'critical') - (x.severity === 'critical'));
    if (!S.connected) box.append(h('div', { class: 'alm critical' }, 'NO CONNECTION'));
    if (S.other && !S.other.connected) box.append(h('div', { class: 'alm critical' }, `${S.other.name}: NO CONNECTION`));
    for (const x of a.slice(0, half ? 2 : compact() ? 2 : 4)) box.append(h('div', { class: 'alm ' + x.severity }, x.code + '  ' + x.message));
    if (!a.length && S.connected) box.append(h('div', { class: 'dim' }, 'NO ALARMS PENDING'));
  }
  function renderAsRun() {
    const box = $('#arrows'); box.replaceChildren();
    const END = { end: 'END', next: 'SEG', fade: 'SEG', cut: 'CUT', stop: 'HARD', error: 'ERR', restart: 'ERR' };
    for (const r of S.asrun.slice(0, 6)) box.append(h('div', { class: 'ar-row' }, h('span', { class: 't' }, hhmm(r.started_at).slice(0, 5)), h('span', { class: 'n' }, r.title + (r.artist ? '  ' + r.artist : '')), r.end_reason ? h('span', { class: 'bd' }, END[r.end_reason] || r.end_reason.toUpperCase().slice(0, 4)) : null));
  }
  async function loadAsRun() {
    try { const r = await J(api('history?limit=6')); S.asrun = r.rows.filter(x => x.ended_at); renderAsRun(); } catch { }
  }
  async function loadChain() {
    S.chainAt = Date.now();
    try { S.chain = await J(api('chain')); S.chainSeen = S.frame ? S.frame.chainRev : S.chain.chain_cfg_rev; S.table = null; } catch { }
  }

  // ---- the processing view (11.2a) and the stage meters (11.2) ----
  const fit = cv => { const w = cv.clientWidth, hh = cv.clientHeight; if (w && hh && (cv.width !== w || cv.height !== hh)) { cv.width = w; cv.height = hh; } };
  const fullScale = () => Math.min(innerWidth / 1920, innerHeight / 1080);
  function drawProcessing(cv, now) {
    fit(cv); const c = cv.getContext('2d'), W = cv.width, H = cv.height; if (!W || !H) return;
    const s = fullScale(), lab = Math.max(11, Math.round(20 * s)), strip = Math.round(lab * 1.35), ph = H - strip;
    c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H);
    const xAt = f => Curve.xFrac(f) * W;
    c.font = `${lab}px sans-serif`; c.textBaseline = 'middle';
    for (const [hz, t] of [[50, '50'], [100, '100'], [200, '200'], [500, '500'], [1000, '1k'], [2000, '2k'], [5000, '5k'], [10000, '10k']]) {
      const x = Math.round(xAt(hz));
      c.fillStyle = '#2a3442'; c.fillRect(x, 0, 1, ph);
      if (s >= 0.8 || [100, 1000, 10000].includes(hz)) { c.fillStyle = '#8a96a5'; c.textAlign = 'center'; c.fillText(t, x, ph + strip / 2); }
    }
    c.textAlign = 'left';
    const f = S.frame && now - S.meterAt <= 500 ? S.frame : null;
    const wantSpec = cfg.pv !== 'curve', wantCurve = cfg.pv !== 'spectrum';
    const nb = W >= 800 ? 64 : 32;
    const yS = db => ph - Math.min(1, Math.max(0, (db + 60) / 60)) * ph;
    if (wantSpec) {
      for (const db of [-20, -40]) { c.fillStyle = '#2a344288'; c.fillRect(0, yS(db), W, 1); c.fillStyle = '#8a96a5'; c.fillText(String(db), 4, yS(db) - lab / 2); }
      const bars = (arr, cap) => Array.from({ length: nb }, (_, i) => nb === 64 ? [arr[i], cap[i]] : [Math.max(arr[2 * i], arr[2 * i + 1]), Math.max(cap[2 * i], cap[2 * i + 1])]);
      const post = cfg.ss !== 'pre';
      const mainBars = bars(post ? D.post : D.pre, post ? D.capPost : D.capPre);
      mainBars.forEach(([v, cp], i) => {
        const x0 = Math.round(i / nb * W), x1 = Math.round((i + 1) / nb * W);
        c.fillStyle = '#4f749c'; c.fillRect(x0, yS(v), Math.max(1, x1 - x0 - 1), ph - yS(v));
        c.fillStyle = '#8fb4dc'; c.fillRect(x0, Math.max(0, yS(cp) - 1), Math.max(1, x1 - x0 - 1), 2);
      });
      if (cfg.ss === 'both') {
        c.strokeStyle = '#e8edf2cc'; c.lineWidth = Math.max(1, s); c.beginPath();
        bars(D.pre, D.capPre).forEach(([v], i) => { const x0 = i / nb * W, x1 = (i + 1) / nb * W; if (i === 0) c.moveTo(x0, yS(v)); else c.lineTo(x0, yS(v)); c.lineTo(x1, yS(v)); });
        c.stroke();
      }
    }
    if (wantCurve) {
      const ch = S.chain, xo = ch?.multiband?.crossovers_hz || ch?.multiband?.crossovers || [];
      const mbOn = f && !f.bypassed && (f.stageMask & 2);
      const have = mbOn && xo.length >= 1 && ch.multiband.bands === xo.length + 1;
      const yC = db => Math.min(1, Math.max(0, -db / cfg.range)) * ph;
      if (have) {
        const n = Math.max(8, Math.min(512, W)), key = xo.join(',') + '/' + n;
        if (!S.table || S.table.key !== key) { S.table = new Curve.BandTable(xo, Curve.logFreqs(n)); S.table.key = key; }
        const gr = Array.from({ length: xo.length + 1 }, (_, b) => f.bandGr[b] ?? 0);
        const mk = cfg.makeup ? (Array.isArray(ch.makeup_db) ? ch.makeup_db : []) : [];
        const db = S.table.curveDb(gr, mk);
        c.beginPath(); db.forEach((d, i) => { const x = i / (n - 1) * W; i ? c.lineTo(x, yC(d)) : c.moveTo(x, yC(d)); });
        c.strokeStyle = '#f5a623'; c.lineWidth = Math.max(1.5, 2.5 * s); c.stroke();
        c.lineTo(W, 0); c.lineTo(0, 0); c.closePath(); c.fillStyle = '#f5a62345'; c.fill();
        c.setLineDash([Math.max(3, 5 * s), Math.max(2, 4 * s)]); c.strokeStyle = '#e8edf2bb'; c.lineWidth = Math.max(1.5, 1.6 * s);
        for (const fc of xo) { const x = xAt(fc); c.beginPath(); c.moveTo(x, 0); c.lineTo(x, ph); c.stroke(); }
        c.setLineDash([]);
        const edges = [20, ...xo, 20000];
        c.fillStyle = '#f5a623'; c.font = `bold ${lab}px sans-serif`; c.textAlign = 'center';
        for (let b = 0; b < edges.length - 1; b++) c.fillText(String(b + 1), xAt(Math.sqrt(edges[b] * edges[b + 1])), 4 + lab / 2);
        c.textAlign = 'right'; c.font = `${lab}px sans-serif`; c.fillStyle = '#f5a623cc';
        for (const d of [-6, -12]) if (-d <= cfg.range) c.fillText(String(d), W - 6, yC(d) - lab / 2);
        S.curveDebug = { db, n, ph, W };
      } else if (cfg.byp !== 'hide') {
        c.fillStyle = '#555f6c'; c.fillRect(0, 0, W, Math.max(2, 2 * s));
        c.font = `bold ${lab}px sans-serif`; c.textAlign = 'right'; c.fillText('BYP', W - 8, 6 + lab / 2);
        S.curveDebug = null;
      }
    }
    c.textAlign = 'left';
  }
  const STAGE_RANGE = { agc: 9, multiband: 12, wideband: 12, limiter: 6, clipper: 6 };
  function drawStages(cv, now) {
    fit(cv); const c = cv.getContext('2d'), W = cv.width, H = cv.height; if (!W || !H) return;
    const s = fullScale(), lab = Math.max(11, Math.round(20 * s)), lh = Math.round(lab * 1.35), pad = Math.round(10 * s);
    c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H); c.font = `bold ${lab}px sans-serif`; c.textBaseline = 'middle';
    const f = S.frame && now - S.meterAt <= 500 ? S.frame : null;
    // the safety limiter's light
    const r = Math.max(5, Math.round(8 * s)); c.beginPath(); c.arc(W - pad - r, pad + lh / 2, r, 0, 7);
    c.fillStyle = f?.safetyActive ? '#ff5c5c' : '#3ecf8e6e'; c.fill();
    c.fillStyle = '#8a96a5'; c.textAlign = 'right'; c.font = `${lab}px sans-serif`; c.fillText('SAFE', W - pad - 2 * r - 8, pad + lh / 2);
    const names = (S.chain?.stages?.length ? S.chain.stages.map(x => x.name) : ['agc', 'multiband', 'wideband', 'limiter', 'clipper']).filter(n => n in STAGE_RANGE);
    const order = ['agc', 'multiband', 'wideband', 'limiter', 'clipper'];
    const chainByp = !f || f.bypassed; const cols = [];
    for (const name of names) {
      const i = order.indexOf(name), en = !chainByp && (f.stageMask >> i & 1) === 1, m = f?.stages[i];
      if (name === 'multiband') {
        const nb = en ? f.bandCount : (S.chain?.multiband?.bands || 0);
        if (nb >= 2) { for (let b = 0; b < Math.min(nb, 6); b++) cols.push({ label: 'B' + (b + 1), en, in: m?.in, out: m?.out, gr: en ? f.bandGr[b] : null, range: 12 }); continue; }
        cols.push({ label: 'MB', en, in: m?.in, out: m?.out, gr: en ? m?.gr : null, range: 12 }); continue;
      }
      cols.push({ label: { agc: 'AGC', wideband: 'WB', limiter: 'LIM', clipper: 'CLP' }[name], en, in: m?.in, out: m?.out, gr: en ? m?.gr : null, range: STAGE_RANGE[name], centered: name === 'agc' });
    }
    const shown = cfg.byp === 'hide' ? cols.filter(x => x.en) : cols;
    const top = pad + lh + 4;
    c.textAlign = 'center';
    if (!shown.length) { c.fillStyle = '#555f6c'; c.textAlign = 'left'; c.fillText('CHAIN BYPASSED', pad, pad + lh / 2); return; }
    const cw = Math.floor((W - 2 * pad) / shown.length), numerals = cw >= c.measureText('-88').width + 4 * s + 8;
    const numH = numerals ? 2 * lh : 0;
    shown.forEach((col, i) => {
      const cx = pad + i * cw + cw / 2, tw = Math.max(10 * s, cw * 0.4), trTop = top + lh + 4, trH = Math.max(4, H - trTop - numH - pad - 4);
      c.fillStyle = col.en ? '#e8edf2' : '#555f6c'; c.font = `bold ${lab}px sans-serif`; c.fillText(col.label, cx, top + lh / 2);
      c.fillStyle = col.en ? '#1d2531' : '#1a1f27'; c.fillRect(cx - tw / 2, trTop, tw, trH);
      if (col.en && col.gr != null) {
        if (col.centered) { const mid = trTop + trH / 2, px = Math.min(1, Math.abs(col.gr) / col.range) * trH / 2; c.fillStyle = '#8fb4dc'; col.gr >= 0 ? c.fillRect(cx - tw / 2, mid - px, tw, px) : c.fillRect(cx - tw / 2, mid, tw, px); c.fillStyle = '#8a96a5'; c.fillRect(cx - tw / 2, mid, tw, 1); }
        else { c.fillStyle = '#f5a623'; c.fillRect(cx - tw / 2, trTop, tw, Math.min(1, -col.gr / col.range) * trH); }
      } else if (!col.en) { c.fillStyle = '#555f6c'; c.fillText('BYP', cx, trTop + trH / 2); }
      if (numerals && col.en) { c.font = `${lab}px sans-serif`; c.fillStyle = '#8a96a5'; c.fillText(col.in == null ? '--' : col.in.toFixed(0), cx, trTop + trH + 4 + lh / 2); c.fillStyle = '#e8edf2'; c.fillText(col.out == null ? '--' : col.out.toFixed(0), cx, trTop + trH + 4 + lh * 1.5); }
    });
    c.textAlign = 'left';
  }

  function dynamics(now) {
    const dt = D.last ? Math.min(0.5, (now - D.last) / 1000) : 0; D.last = now;
    const f = S.frame && now - S.meterAt <= 500 ? S.frame : null;
    for (let i = 0; i < 64; i++) {
      for (const [bar, cap, capAt, src] of [[D.post, D.capPost, D.capPostAt, f?.postDb], [D.pre, D.capPre, D.capPreAt, f?.preDb]]) {
        bar[i] = Math.max(src ? src[i] : -90, bar[i] - 15 * dt);
        if (bar[i] >= cap[i]) { cap[i] = bar[i]; capAt[i] = now; } else if (now - capAt[i] > 1000) cap[i] = Math.max(bar[i], cap[i] - 20 * dt);
      }
    }
  }

  function renderOther() {
    const el = $('#other'), o = S.other;
    if (!o) { setText(el, ''); return; }
    const crit = !o.connected || (o.state?.alerts || []).some(a => a.severity === 'critical');
    const warn = (o.state?.alerts || []).some(a => a.severity === 'warning');
    const cls = 'ind ' + (crit ? 'red' : warn ? 'amber' : ''); if (el.className !== cls) el.className = cls;
    const left = o.now && o.state?.state === 'playing' ? Math.max(0, Date.parse(o.now.ends_at) - Date.now()) : o.now ? 0 : null;
    setText(el, o.name + (!compact() && o.connected && left != null ? '  ' + mmss(left) : '') + (!o.connected && !compact() ? '  NO CONNECTION' : ''));
  }

  // ---- the processing view (11.2c): procview.js draws it; this builds what it shows from the page's state ----
  const pvCv = $('#pv'), procState = new Ballistics.Proc();
  const FAM = { sans: '"DejaVu Sans", Verdana, ' + (getComputedStyle(document.documentElement).getPropertyValue('--font') || 'sans-serif'), mono: '"DejaVu Sans Mono", ' + (getComputedStyle(document.documentElement).getPropertyValue('--mono') || 'monospace') };
  function procPill() {
    const s = S.state; if (!S.connected) return 'NO CONNECTION'; if (!s) return 'STOPPED';
    return s.safe_mode ? 'SAFE MODE' : s.state === 'playing' && s.mode === 'assist' ? 'ASSIST' : s.hold || s.state === 'held' ? 'HELD' : s.state === 'playing' ? 'PLAYING' : 'STOPPED';
  }
  // the time at the right of the title strip: the time left, or what the station is doing when it is not playing the item out
  function procTime(now) {
    const n = S.now; if (!n) return { text: '', color: ProcView.C.dim };
    const ends = Date.parse(n.ends_at), st = S.state?.state, over = now - S.skew - ends, idle = st !== 'playing' || over >= 0;
    if (!idle) return { text: mmss(ends - now - S.skew), color: ProcView.C.text };
    const lab = { held: 'HELD', stopped: 'STOPPED', safe: 'SAFE MODE' }[st];
    if (!lab) return { text: '0:00', color: ProcView.C.dim };
    return { text: over > 0 ? lab + ' ' + mmss(over) : lab, color: ProcView.C.amber };
  }
  function tickProc() {
    const now = Date.now(), dpr = window.devicePixelRatio || 1, W = innerWidth, H = innerHeight;
    if (pvCv.width !== Math.round(W * dpr) || pvCv.height !== Math.round(H * dpr)) { pvCv.width = Math.round(W * dpr); pvCv.height = Math.round(H * dpr); }
    const ctx = pvCv.getContext('2d'); ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    dynamics(now);
    const f = S.frame && now - S.meterAt <= 500 ? S.frame : null;
    const b = bars.update(f, now), p = procState.update(f, now), s = S.state, alerts = s?.alerts || [];
    const o = S.other, oCrit = o && (!o.connected || (o.state?.alerts || []).some(a => a.severity === 'critical')), oWarn = o && (o.state?.alerts || []).some(a => a.severity === 'warning');
    const M = {
      name: s ? (s.display_name || s.station) : '-', connected: S.connected, pill: procPill(), critical: !S.connected || alerts.some(a => a.severity === 'critical'),
      clock: clock(new Date()), other: o ? { name: o.name, color: oCrit ? ProcView.C.red : oWarn ? ProcView.C.amber : ProcView.C.green } : null, rotate: null, pinned: false,
      alerts, now: S.now, timeText: procTime(now), frame: f, chain: S.chain, proc: p,
      bars: { l: b.frame.ppmL, r: b.frame.ppmR, holdL: b.hold[0], holdR: b.hold[1] },
      cfg: { range: pcfg.range, hide: pcfg.byp === 'hide', spectrum: pcfg.spectrum, spectrumSource: pcfg.ss },
      spectra: { post: D.post, capPost: D.capPost, pre: D.pre },
    };
    window.__proc = { M, rep: ProcView.render(ctx, W, H, M, FAM) };
  }
  function tick() {
    if (procMode) { tickProc(); requestAnimationFrame(tick); return; }
    const now = Date.now();
    S.ticks = (S.ticks || 0) + 1;
    setText($('#clk'), clock(new Date()));
    const n = S.now;
    if (n) {
      const started = Date.parse(n.started_at), ends = Date.parse(n.ends_at), seg = n.segue_at ? Date.parse(n.segue_at) : null;
      // Held, stopped or past the item's end: nothing is extrapolated (a held station read "-0:24 left" and "Segue in -0:32"
      // and kept counting down below zero); the item reads 0:00 and the line says HELD and for how long.
      const stt = S.state?.state, over = now - S.skew >= ends, idle = over || stt === 'stopped' || stt === 'safe';
      const upto = stt === 'stopped' && S.stoppedAt ? Math.min(now, S.stoppedAt) : now;
      const el = Math.min(Math.max(0, upto - started + S.skew), ends - started), rem = idle ? 0 : ends - now - S.skew;
      setText($('#elapsed'), mmss(el)); setText($('#remain'), mmss(rem));
      $('#prog').style.width = Math.max(0, Math.min(100, 100 * el / Math.max(1, ends - started))) + '%';
      const sg = $('#segue');
      if (idle) {
        const lab = { held: 'HELD', stopped: 'STOPPED', safe: 'SAFE MODE' }[S.state?.state];
        setText(sg, lab ? lab + (over ? ' ' + mmss(now - S.skew - ends) : '') : ''); sg.className = 'segue amber';
      }
      else if (stt === 'held') { setText(sg, 'HELD, ENDS ' + mmss(rem)); sg.className = 'segue amber'; }
      else if (n.continuity_next) { setText(sg, 'ALBUM, ENDS ' + mmss(rem)); sg.className = 'segue'; }
      else if (seg) { const left = Math.max(0, seg - now - S.skew); setText(sg, 'SEGUE ' + mmss(left)); sg.className = 'segue ' + (left < 10000 ? 'red' : left < 30000 ? 'amber' : ''); }
      else setText(sg, '');
    } else { $('#elapsed').textContent = '0:00'; $('#remain').textContent = '0:00'; $('#prog').style.width = '0'; $('#segue').textContent = ''; }
    for (const tm of $$('#qrows .tm[data-left]')) {
      const left0 = tm.dataset.left; if (left0 === '') continue;
      const left = Math.max(0, +left0 - (now - (S.queueAt || now))); tm.textContent = mmss(left); tm.className = 'tm ' + (left < 10000 ? 'red' : left < 60000 ? 'amber' : '');
    }
    dynamics(now);
    const f = S.frame && now - S.meterAt <= 500 ? S.frame : null;
    // the loudness controller's bar while it runs (a frame without a target: the controller is off), on its own gain range
    const lcOn = !!f && f.target != null && LC.min != null && LC.max != null;
    const opts = { compact: compact(), lc: lcOn ? { gain: f.gain, lstate: f.lstate, min: LC.min, max: LC.max } : null };
    const b = bars.update(f, now); drawMeters(mcv, b.frame, b.hold, opts);
    if (!compact()) { drawProcessing(scv, now); drawStages(stcv, now); }
    const fmt = v => v == null ? '--' : v.toFixed(1);
    const rows = [['M', f?.momentary, 'ms'], ['S', f?.shortTerm, 'ms'], ['W', f?.window, ''], ['I', f?.integrated, 'iv']];
    const tg = f?.target;
    // compact: the controller's gain takes the place of the target while its bar shows ("LC +3.2"; a split half has room
    // for the number only, in the bar's colour)
    const lcCells = lcOn ? [...(half ? [] : [['LC', 'lcl']]), [`${f.gain >= 0 ? '+' : ''}${f.gain.toFixed(1)}`, 'lcv s' + (f.lstate || 0)]] : [];
    const cells = [...rows.flatMap(([k, v, cl]) => [[k, cl], [fmt(v), cl]]), ...(compact() && lcOn ? lcCells : compact() && tg != null ? [['/ ' + tg.toFixed(0), 'tg']] : [])];
    const numsEl = $('#nums'), sig = cells.map(([, cl]) => cl).join(',');
    if (numsEl.dataset.sig !== sig) { numsEl.dataset.sig = sig; numsEl.replaceChildren(...cells.map(([, cl]) => h('span', { class: cl }))); }
    cells.forEach(([t], i) => { setText(numsEl.children[i], t); });
    const ctl = f && f.lstate ? `${f.gain >= 0 ? '+' : ''}${f.gain.toFixed(1)} dB ${['OFF', 'CONVERGING', 'ON TARGET', 'FROZEN', 'AT LIMIT'][f.lstate] || ''}` : '';
    setText($('#tgt'), f ? (tg != null ? `TARGET ${tg.toFixed(0)}   ${ctl}` : 'CONTROLLER OFF') : '');
    renderOther();
    requestAnimationFrame(tick);
  }
  async function snapshotNow() {
    try { const n = await J(api('now')); S.now = n.playing ? n : null; renderNow(); } catch { }
  }
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  new Live(`${proto}://${location.host}${apiBase}/ws?topics=state,now,queue,alerts,asrun,meters,program`, {
    program(d) { S.program = d; renderEnd(); },
    open() { S.connected = true; J(api('program/state')).then(d => { S.program = d; renderEnd(); }).catch(() => { }); document.body.classList.remove('offline'); loadAsRun(); loadChain(); renderState(); },
    close() { S.connected = false; S.frame = null; document.body.classList.add('offline'); renderState(); },
    snapshot(d) { S.state = d; S.now = d.now; S.queueAt = Date.now(); S.skew = 0; renderState(); renderNow(); loadAsRun(); },
    now(d) { S.now = d; S.skew = 0; renderNow(); },
    timing(d) { if (S.now && d.entry_id === S.now.entry_id) S.skew = Math.max(-5000, Math.min(5000, d.elapsed_ms - (Date.now() - Date.parse(S.now.started_at)))); },
    state(d) {
      if (!S.state) return;
      const was = S.state.state; Object.assign(S.state, d);
      // stopped: the elapsed time and the bar stand where the audio stopped
      if (d.state && d.state !== 'playing' && was === 'playing') S.stoppedAt = Date.now();
      if (d.state === 'playing') { S.stoppedAt = null; if (was !== 'playing') snapshotNow(); }
      renderState();
    },
    queue(d) { if (S.state) { S.state.queue = d.entries; S.queueAt = Date.now(); renderQueue(); } },
    alert(d) { if (S.state) { S.state.alerts = (S.state.alerts || []).filter(a => a.id !== d.id).concat([d]); renderState(); } },
    alert_cleared(d) { if (S.state) { S.state.alerts = (S.state.alerts || []).filter(a => a.id !== d.id); renderState(); } },
    chain(d) { S.chain = d; S.chainSeen = S.frame ? S.frame.chainRev : d.chain_cfg_rev; S.table = null; },
    asrun() { loadAsRun(); },
    meters(f) {
      S.frame = f; S.meterAt = Date.now();
      // a new chain revision in a frame: fetch the configuration again (11.2a)
      if (S.chainSeen != null && f.chainRev !== S.chainSeen && Date.now() - S.chainAt > 900) loadChain();
    },
  });
  // The other station's header indicator (the library's page names it with ?other=): its name, remaining time and
  // alert color, so a problem there is never invisible.
  const otherId = params.get('other');
  if (otherId) {
    const base = apiBase.replace(/\/[^/]+$/, '/' + otherId);
    S.other = { id: otherId, name: otherId, connected: false, state: null, now: null };
    new Live(`${proto}://${location.host}${base}/ws?topics=state,now,alerts`, {
      open() { S.other.connected = true; renderAlarms(); }, close() { S.other.connected = false; renderAlarms(); },
      snapshot(d) { S.other.state = d; S.other.now = d.now; S.other.name = d.display_name || d.station || otherId; },
      now(d) { S.other.now = d; },
      state(d) { Object.assign(S.other.state || (S.other.state = {}), d); },
      alert(d) { const st = S.other.state || (S.other.state = {}); st.alerts = (st.alerts || []).filter(a => a.id !== d.id).concat([d]); },
      alert_cleared(d) { const st = S.other.state || (S.other.state = {}); st.alerts = (st.alerts || []).filter(a => a.id !== d.id); },
    });
  }
  const prod = await J('/health').then(x => x.product).catch(() => null);
  if (prod) { $('#prod').textContent = prod; document.title = prod + ' status'; }
  window.__display = { S, D, cfg, pcfg, LC };   // for the tests of the page
  requestAnimationFrame(tick);
})();
