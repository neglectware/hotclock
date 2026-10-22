// The units of the Processing page's Rack view (processing.js): one unit for each block of the chain, in signal
// order, 1U or 2U high like rack gear (1U = 56 px, 64 px under 1024; 2U = 2 x 1U + the 8 px gap, so its edges line up
// with two 1U units). The equalizer, the multiband compressor, the AGC and the loudness controller are 2U and have a
// display. Every unit has the same columns: the number, the power switch, the name, a summary of the block's main
// settings, a meter area and a chevron. Opening a unit is processing.js's job; this file builds the strip and keeps
// its summary, meters and display live from the page's one meter-frame handler (no timer of its own).
//
// The meter shows the level at the output of the block, left and right, as two thin LED bars (green to -12 dBFS,
// amber to -3, red above; -48 to 0 dBFS) with a peak that holds for 1.5 s and falls back, and the peak number of the
// louder channel; the wideband compressor, clipper and limiters add a gain-reduction bar. A 2U unit adds its display:
// the equalizer's curve with the live spectrum, a labeled gain-reduction meter for each multiband band, and the state
// lights, the centred gain bar and the 60 s gain history of the AGC and the loudness controller.
const ProcRack = (() => {
  // the frame's per-block output slots (engine::chain::BLK_NAMES)
  const SLOT = { trim: 0, highpass: 1, rotator: 2, stereo: 3, agc: 4, eq: 5, bass: 6, multiband: 7, wideband: 8, makeup: 9, clipper: 10, loudness: 11, limiter: 12, safety: 13 };
  const GR_STAGE = { wideband: 2, clipper: 4, limiter: 3, safety: 5 };
  const clamp = (x, a, b) => Math.min(b, Math.max(a, x));
  const signed = (x, d = 1) => (x > 0.049 ? '+' : '') + (Math.abs(x) < 0.05 ? 0 : x).toFixed(d);
  const LED_MIN = -48, HOLD_MS = 1500, FALL_DB_S = 24;
  const AGC_LIGHTS = [['riding', 'RIDING', '#2f9e6e', 'Riding: the AGC follows the program'], ['gated', 'GATED', '#4a566c', 'Gated: silence, the gain is held'], ['riding_fade', 'FADE', '#4f7fe0', 'Fade: the program is fading out and the AGC follows it up'], ['clamping', 'CLAMPING', '#f5a623', 'Clamping: the fast clamp is cutting']];
  const LOUD_LIGHTS = [['acquiring', 'ACQUIRING', '#4f7fe0', 'Acquiring: measuring the program'], ['holding', 'HOLDING', '#4a566c', 'Holding: silence or gated content'], ['converging', 'RIDING', '#2f9e6e', 'Riding: the gain is moving to the target'], ['on_target', 'ON TARGET', '#2f9e6e', 'On target'], ['at_limit', 'AT LIMIT', '#f5a623', 'At limit: the gain is at its end']];
  const LOUD_STATE = { ACQUIRING: 'acquiring', HOLDING: 'holding', RIDING: 'converging', 'ON TARGET': 'on_target', 'AT LIMIT': 'at_limit' };

  // One LED bar, -48 to 0 dBFS, with a peak mark that holds and falls back.
  function led(title) {
    const lit = h('i', { class: 'lit' }), pk = h('b', { class: 'pk' });
    const el = h('div', { class: 'led', title }, lit, pk);
    let hold = -Infinity, at = 0, last = 0, cl = '', pl = '';
    return {
      el,
      // db: the block's peak in this frame (null: silence); returns the held peak
      set(db, now) {
        const v = db == null ? -Infinity : db, dt = last ? (now - last) / 1000 : 0;
        last = now;
        if (v >= hold) { hold = v; at = now; }
        else if (now - at > HOLD_MS) hold = Math.max(v, hold - FALL_DB_S * dt);
        const c = 'inset(0 ' + (100 - clamp((v - LED_MIN) / -LED_MIN, 0, 1) * 100).toFixed(1) + '% 0 0)';
        if (c !== cl) { cl = c; lit.style.clipPath = c; }
        const p = hold > LED_MIN ? 'calc(' + (clamp((hold - LED_MIN) / -LED_MIN, 0, 1) * 100).toFixed(1) + '% - 2px)' : '-4px';
        if (p !== pl) { pl = p; pk.style.left = p; }
        return hold;
      },
    };
  }
  const bar = () => { const fill = h('i'); return { el: h('div', { class: 'rbar' }, fill), fill }; };

  function strip(o) {
    const key = o.key;
    const items = [];
    const sum = h('div', { class: 'rsum' });
    const addItem = (label, get, cls) => {
      const v = h('b', { class: 'mono rvv' }, '');
      sum.append(h('span', { class: 'rv ' + (cls || '') }, label ? h('span', { class: 'dim rl' }, label) : null, v));
      items.push((c, x) => setText(v, get(c, x)));
    };
    // ---- the summary ----
    const U2 = key === 'eq' || key === 'multiband' || key === 'agc' || key === 'loudness';   // 2U: two strips high, with a display
    const par = (c, k) => o.schema[key].params.find(p => p.key === k);
    const num = (c, k, plus) => { const v = c.blocks[key][k]; const t = PW.fmtValue(v, PW.spec(par(c, k))); return plus && v > 0 ? '+' + t : t; };
    const SUM = {
      trim: [['GAIN', 'gain_db', 1]], highpass: [['FREQ', 'freq_hz'], ['ORDER', 'order']], rotator: [['FREQ', 'freq_hz'], ['SECTIONS', 'sections']],
      stereo: [['WIDTH', 'width']], bass: [['FREQ', 'freq_hz'], ['AMOUNT', 'amount_pct']], wideband: [['THRESH', 'threshold_dbfs'], ['RATIO', 'ratio']],
      makeup: [['GAIN', 'gain_db', 1]], clipper: [['THRESH', 'threshold_dbfs']], limiter: [['CEILING', 'ceiling_dbtp']],
    };
    if (SUM[key]) for (const [lab, k, plus] of SUM[key]) addItem(lab, c => num(c, k, plus));
    else if (key === 'agc') {
      addItem('GAIN', (c, x) => x.agc && x.agc.gain != null ? signed(x.agc.gain) + ' dB' : '--');
      addItem('TARGET', c => num(c, 'target_dbfs'));
    } else if (key === 'eq') addItem('BANDS', c => String(c.blocks.eq.bands));
    else if (key === 'multiband') {
      addItem('BANDS', c => String(c.blocks.multiband.crossovers_hz.length + 1));
      addItem('PRESET', c => c.block_presets?.multiband || 'custom');
    } else if (key === 'loudness') {
      addItem('GAIN', (c, x) => x.loud && x.loud.gain != null ? signed(x.loud.gain) + ' dB' : '--');
      addItem('TARGET', (c, x) => x.loud && x.loud.target != null ? x.loud.target.toFixed(1) + ' LUFS' : '--');
    } else if (key === 'safety') addItem('CEILING', () => '-1.0 dBFS');

    // ---- the level meter (every block): the peaks at the output of the block, left and right ----
    const meter = h('div', { class: 'rmeter' + (U2 ? ' l2' : '') });
    const sp = h('div', { class: 'rm-sp' });
    const ledL = led('Left output peak'), ledR = led('Right output peak');
    const pv = h('span', { class: 'mono rm-num', title: 'Peak of the louder channel at the output of this block, dBFS' }, '--');
    let grBar = null, grPv = null;
    if (GR_STAGE[key] != null) {
      grBar = bar(); grPv = h('span', { class: 'mono rm-gn' }, '--');
      sp.append(h('span', { class: 'dim rm-lab' }, 'GR'), grBar.el, grPv);
      grBar.el.title = 'Gain reduction now, 0 to -12 dB';
    }
    meter.append(sp, h('div', { class: 'rm-lr' }, ledL.el, ledR.el), pv);
    const slot = SLOT[key];
    const mupd = f => {
      const now = performance.now(), b = f.blk && f.blk[slot];
      const hl = ledL.set(b ? b[0] : null, now), hr = ledR.set(b ? b[1] : null, now);
      const m = Math.max(hl, hr);
      setText(pv, m > -60 ? m.toFixed(1) : '--');
      if (grBar) {
        const st = GR_STAGE[key], en = key === 'safety' || (f.stageMask >> st & 1) === 1, g = en ? f.stages[st].gr : null;
        const w = g == null ? '0' : Math.min(12, Math.abs(g)) / 12 * 100 + '%';
        if (grBar.fill.style.width !== w) grBar.fill.style.width = w;
        setText(grPv, g == null ? '--' : (Math.abs(g) < 0.05 ? 0 : g).toFixed(1));
      }
    };

    // ---- the display of a 2U block ----
    const vis = U2 ? h('div', { class: 'rvis rvis-' + key }) : null;
    let vupd = null, specFeed = null;
    // a canvas that follows the size of its box, drawn by `draw(ctx, W, H)`; `redraw()` draws it again
    const fitCanvas = (cls, draw) => {
      const cv = h('canvas', { class: cls, 'aria-hidden': 'true' });
      let W = 0, H = 0, dpr = 1;
      const paint = () => {
        if (!W || !H) return;
        const g = cv.getContext('2d');
        g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, W, H);
        draw(g, W, H);
      };
      const ro = new ResizeObserver(() => {
        const w = Math.round(cv.clientWidth), hh = Math.round(cv.clientHeight);
        if (!w || !hh || (w === W && hh === H && dpr === (window.devicePixelRatio || 1))) return;
        W = w; H = hh; dpr = window.devicePixelRatio || 1; cv.width = W * dpr; cv.height = H * dpr; paint();
      });
      ro.observe(cv);
      return { cv, redraw: paint };
    };
    if (key === 'eq') {
      // the response curve, and behind it the live spectrum while the analyzer is on (the card's own switch sets it)
      let eqc = null, tap = 'off', lv = null, lastAt = 0, shown = '';
      const A_LO = -90, A_HI = -10, NB = 120, DB = 18;
      const fresh = () => tap !== 'off' && lv && performance.now() - lastAt < 1500;
      const cv = fitCanvas('rvc', (g, W, H) => {
        const L = 26, R = 6, T = 6, B = 14, px = f => L + Math.log10(f / 20) / 3 * (W - L - R), py = d => T + (1 - (d + DB) / (2 * DB)) * (H - T - B);
        g.fillStyle = '#0b0e13'; g.fillRect(0, 0, W, H);
        g.font = '9px sans-serif'; g.fillStyle = '#7d8799'; g.strokeStyle = '#242c3a'; g.lineWidth = 1;
        for (const hz of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) { g.beginPath(); g.moveTo(px(hz) + .5, T); g.lineTo(px(hz) + .5, H - B); g.stroke(); g.fillText(hz >= 1000 ? hz / 1000 + 'k' : String(hz), px(hz) + 2, H - 3); }
        for (const d of [-12, 0, 12]) { g.strokeStyle = d === 0 ? '#3a4457' : '#242c3a'; g.beginPath(); g.moveTo(L, py(d) + .5); g.lineTo(W - R, py(d) + .5); g.stroke(); g.fillText((d > 0 ? '+' : '') + d, 2, py(d) + 3); }
        if (fresh()) {
          const ay = d => T + (1 - (clamp(d, A_LO, A_HI) - A_LO) / (A_HI - A_LO)) * (H - T - B), ax = i => px(20 * Math.pow(2, (i + .5) / 12));
          g.beginPath(); g.moveTo(ax(0), H - B);
          for (let i = 0; i < NB; i++) g.lineTo(ax(i), ay(lv[i]));
          g.lineTo(ax(NB - 1), H - B); g.closePath(); g.fillStyle = 'rgba(79,195,247,.22)'; g.fill();
          g.beginPath(); for (let i = 0; i < NB; i++) i ? g.lineTo(ax(i), ay(lv[i])) : g.moveTo(ax(i), ay(lv[i]));
          g.strokeStyle = '#4fc3f7'; g.lineWidth = 1; g.stroke();
        }
        if (eqc) {
          const fs = Curve.logFreqs(Math.max(60, Math.round(W - L - R))), db = EqMath.responseDb(eqc.blocks.eq, fs);
          g.strokeStyle = el.classList.contains('off') ? '#7a6a48' : '#f5a623'; g.lineWidth = 2; g.beginPath();
          db.forEach((d, i) => { const x = px(fs[i]), y = py(clamp(d, -DB, DB)); i ? g.lineTo(x, y) : g.moveTo(x, y); });
          g.stroke();
        }
      });
      vis.append(cv.cv);
      specFeed = u8 => {
        if (tap === 'off' || u8.length < 6 + NB || u8[4] !== (tap === 'in' ? 0 : 1) || u8[5] !== NB) return;
        lv = lv || new Float32Array(NB);
        for (let i = 0; i < NB; i++) lv[i] = u8[6 + i] / 2 - 120;
        lastAt = performance.now(); cv.redraw();
      };
      vupd = (f, c, x) => {
        const t = x.eqTap || 'off';
        if (t !== tap) { tap = t; lv = null; }
        const k = JSON.stringify(c.blocks.eq) + el.classList.contains('off') + fresh() + tap;
        if (k !== shown) { shown = k; eqc = c; cv.redraw(); }
      };
    } else if (key === 'multiband') {
      const cells = Array.from({ length: 6 }, (_, i) => {
        const fill = h('i'), nm = h('b', { class: 'rbcn' }, ''), gv = h('span', { class: 'mono rbcg' }, '--');
        const el2 = h('div', { class: 'rbc' }, h('div', { class: 'rbcb' }, fill), nm, gv);
        return { el: el2, fill, nm, gv, t: '' };
      });
      vis.append(h('div', { class: 'rbands', role: 'img', 'aria-label': 'Gain reduction of each band' }, cells.map(c => c.el)));
      vupd = (f, c) => {
        const n = c.blocks.multiband.crossovers_hz.length + 1, names = o.bandNames(n);
        cells.forEach((cl, i) => {
          const hide = i >= n; if (cl.el.hidden !== hide) cl.el.hidden = hide;
          if (hide) return;
          setText(cl.nm, names[i]);
          const g = f && (f.stageMask >> 1 & 1) && f.bandGr[i] != null ? f.bandGr[i] : null;
          if (!f) return;
          cl.fill.style.height = g == null ? '0' : Math.min(18, Math.abs(g)) / 18 * 100 + '%';
          setText(cl.gv, g == null ? '--' : (Math.abs(g) < 0.05 ? 0 : g).toFixed(1));
          const t = names[i] + ': ' + (g == null ? '--' : g.toFixed(1) + ' dB');
          if (t !== cl.t) { cl.t = t; cl.el.title = t; }
        });
      };
    } else if (key === 'agc' || key === 'loudness') {
      const list = key === 'agc' ? AGC_LIGHTS : LOUD_LIGHTS;
      const dots = list.map(([id, lab, col, tip]) => h('span', { class: 'rlight', title: tip, 'data-state': id }, h('i', { style: '--lc:' + col }), h('span', { class: 'rlt' }, lab)));
      const fill = h('i'), gpv = h('span', { class: 'mono rm-num' }, '--');
      const pts = []; let lastPt = 0;
      const R = 12;
      const cv = fitCanvas('rspark', (g, W, H) => {
        g.fillStyle = '#0b0e13'; g.fillRect(0, 0, W, H);
        g.font = '9px sans-serif'; g.fillStyle = '#7d8799';
        g.strokeStyle = '#242c3a'; g.lineWidth = 1;
        for (const d of [-R / 2, R / 2]) { const y = H / 2 - d / R * (H / 2 - 2); g.beginPath(); g.moveTo(24, y + .5); g.lineTo(W, y + .5); g.stroke(); }
        g.strokeStyle = '#3a4457'; g.beginPath(); g.moveTo(24, H / 2 + .5); g.lineTo(W, H / 2 + .5); g.stroke();
        if (H >= 40) { g.fillText('+' + R, 2, 10); g.fillText('0', 2, H / 2 + 3); g.fillText('-' + R, 2, H - 3); }
        g.fillText('60 s', W - 26, H - 3);
        g.strokeStyle = '#3ecf8e'; g.lineWidth = 1.5; g.beginPath();
        let pen = false;
        pts.forEach((v, i) => { if (v == null) { pen = false; return; } const px = 24 + (W - 24) - (pts.length - 1 - i) / 120 * (W - 24), py = H / 2 - clamp(v, -R, R) / R * (H / 2 - 2); pen ? g.lineTo(px, py) : g.moveTo(px, py); pen = true; });
        g.stroke();
      });
      vis.append(h('div', { class: 'rlights', role: 'img', 'aria-label': 'State' }, dots),
        h('div', { class: 'rgain' }, h('div', { class: 'rbar bp', title: 'The gain now, 12 dB each way' }, fill, h('b', { class: 'mid' })), gpv), cv.cv);
      vupd = (f, c, x) => {
        const d = key === 'agc' ? x.agc : x.loud, now = performance.now();
        const st = d && d.on !== false ? (key === 'agc' ? d.state : LOUD_STATE[d.pill[0]]) : null;
        dots.forEach(dt => { const on = dt.dataset.state === st; if (dt.classList.contains('on') !== on) dt.classList.toggle('on', on); });
        const g = d ? d.gain : null;
        if (g == null) { fill.style.width = '0'; setText(gpv, '--'); }
        else { const w = Math.min(R, Math.abs(g)) / R * 50; fill.style.left = g >= 0 ? '50%' : (50 - w) + '%'; fill.style.width = w + '%'; setText(gpv, signed(g)); }
        if (f && now - lastPt >= 500) { lastPt = now; pts.push(g); while (pts.length > 120) pts.shift(); cv.redraw(); }
      };
    }

    // ---- the strip ----
    const chev = h('span', { class: 'rchev', 'aria-hidden': 'true' }, '▸');
    const el = h('div', { class: 'rstrip' + (U2 ? ' u2' : ''), role: 'button', tabindex: '0', 'aria-expanded': 'false', 'data-rack-strip': key, title: 'Open or close this block' },
      h('span', { class: 'pidx', 'aria-hidden': 'true' }, String(o.idx)), o.power,
      h('div', { class: 'rname' }, h('span', { class: 'ptitle-t' }, o.label), o.help),
      h('div', { class: 'rrest' }, sum, meter), vis, chev);
    el.addEventListener('click', e => { if (e.target.closest('.pwr, a, input, select')) return; o.onToggle(); });
    el.addEventListener('keydown', e => {
      if (e.target !== el) return;
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); o.onToggle(); return; }
      const d = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0;
      if (!d) return;
      e.preventDefault();
      const all = [...el.closest('.prack').querySelectorAll('.rstrip')];
      all[clamp(all.indexOf(el) + d, 0, all.length - 1)].focus();
    });
    return {
      el, key, u2: U2,
      setOpen(on) { el.setAttribute('aria-expanded', String(on)); setText(chev, on ? '▾' : '▸'); },
      spectrum(u8) { specFeed?.(u8); },
      // c: the chain, x: the live extras of processing.js, f: a meter frame (null when only the settings changed)
      update(c, x, f) {
        if (!f || key === 'loudness') o.paintPower(el);
        for (const fn of items) fn(c, x);
        if (f) mupd(f);
        if (vupd) vupd(f, c, x);
      },
    };
  }
  return { strip, SLOT };
})();
