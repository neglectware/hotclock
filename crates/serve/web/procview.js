// The processing view of the status display (architecture 11.2c): the JavaScript twin of
// crates/display/src/scene/proc.rs. One station's processing chain as meters: a gain-reduction bar per multiband
// band, the wideband compressor, the limiter and the clipper, growing downward from the top of one shared dB scale;
// the AGC's gain on its own center-zero scale; the level into the chain against the level out of it; and the
// loudness controller. Everything is sized from one number, the text size `t` (31 px at 1024 x 600), and no text is
// drawn smaller than that; graphics are as fine as the pixels allow. The geometry (`layout`, `columns`, the vertical
// positions) is the same arithmetic as the native view, and dev/proc-view-check.mjs compares the two.
const ProcView = (() => {
  const C = { bg: '#0b0e13', panel: '#141a23', panel2: '#1d2531', line: '#2a3442', text: '#e8edf2', dim: '#8a96a5', amber: '#f5a623', red: '#ff5c5c',
    redBg: '#5a161b', green: '#3ecf8e', blue: '#4f749c', blueLine: '#8fb4dc', grey: '#555f6c', off: '#1a1f27', white: '#ffffff' };
  const CAP = 0.729;                       // the cap height of the fonts, as a fraction of the size
  const AGC_RANGE = 9, GRID = 6;
  const sc = (t, f) => Math.round(t * f);
  const STAGES = ['agc', 'multiband', 'wideband', 'limiter', 'clipper', 'safety'];

  // The text size: the compact layout's own (31 px at 1024 x 600, scaled to the viewport) under 720 px high; from
  // 720 px up the full layout's 28 px at 1080p, never under 19 px (the 21.5-inch rule; the page cannot know the diagonal).
  function textPx(W, H) {
    if (H < 720) return Math.max(19, Math.round(31 * Math.min(W / 1024, H / 600)));
    return Math.max(19, Math.round(28 * Math.min(W / 1920, H / 1080)));
  }
  function metrics(W, H) {
    const t = textPx(W, H);
    return { t, pad: Math.max(4, sc(t, 0.3)), padV: Math.max(3, sc(t, 0.2)), line: sc(t, 1.25) };
  }
  // The zones, top to bottom: header, now, cols, the spectrum when asked for and there is room, meters.
  function layout(W, H, spectrum, noHead) {
    const m = metrics(W, H), t = m.t;
    const hdr = noHead ? 0 : sc(t, 1.42), now = noHead ? 0 : sc(t, 1.3), row = sc(t, 1.22), meters = 3 * row + 2 * m.padV;
    const rest = H - hdr - now - meters, avail = rest - sc(t, 10);
    const specH = spectrum && avail >= sc(t, 4) ? Math.max(sc(t, 4), Math.min(avail, Math.round(H * 0.3))) : 0;
    const rows = noHead ? [['cols', rest - specH]] : [['header', hdr], ['now', now], ['cols', rest - specH]];
    if (specH) rows.push(['spectrum', specH]);
    rows.push(['meters', H - rows.reduce((a, r) => a + r[1], 0) - 0]);
    let y = 0; const zones = {};
    for (const [n, h] of rows) { zones[n] = [0, y, W, h]; y += h; }
    zones.meters[3] = H - zones.meters[1];
    return { m, zones, spectrum: specH > 0 };
  }
  const bandNames = n => ({ 2: ['LOW', 'HIGH'], 3: ['LOW', 'MID', 'HIGH'], 4: ['LOW', 'LMID', 'HMID', 'HIGH'], 5: ['LOW', 'LMID', 'MID', 'HMID', 'HIGH'], 6: ['LOW', 'LMID', 'MID', 'HMID', 'PRES', 'HIGH'] }[n] || []);
  function hzText(f) { if (f >= 1000) { const k = f / 1000; return Math.abs(k - Math.round(k)) < 0.05 ? Math.round(k) + 'k' : k.toFixed(1) + 'k'; } return String(Math.round(f)); }

  // The blocks the station has, in chain order, each on or bypassed (11.2): a block the chain configuration does not
  // list is absent, a listed one that is off is bypassed (grey BYP, or omitted when `hide`), and with the whole chain
  // bypassed every column is. `f` is the live frame (parsed by common.js) or null.
  function columns(chain, f, hide) {
    const names = (chain?.stages?.length ? chain.stages.map(s => s.name) : STAGES.slice(0, 5)).filter(n => STAGES.slice(0, 5).includes(n));
    const chainByp = !f || f.bypassed, cols = [];
    for (const name of names) {
      const i = STAGES.indexOf(name), en = !chainByp && (f.stageMask >> i & 1) === 1, gr = en ? f.stages[i].gr : null;
      if (name === 'agc') cols.push({ kind: 'agc', label: 'AGC', name: 'agc', enabled: en, slot: null, raw: gr });
      else if (name === 'multiband') {
        const nb = en ? f.bandCount : 0;
        if (nb >= 2) for (let b = 0; b < Math.min(nb, 6); b++) cols.push({ kind: 'band', label: 'B' + (b + 1), name: 'band' + (b + 1), enabled: true, slot: b, raw: f.bandGr[b] });
        else cols.push({ kind: 'mb', label: 'MB', name: 'multiband', enabled: en, slot: null, raw: gr });
      } else {
        const [kind, label, slot] = { wideband: ['wide', 'WB', 6], limiter: ['lim', 'LIM', 7], clipper: ['clip', 'CLP', 8] }[name];
        cols.push({ kind, label, name, enabled: en, slot, raw: gr });
      }
    }
    return hide ? cols.filter(c => c.enabled) : cols;
  }

  // ---- drawing ----
  // `M.noHead`: the bars and the meters only, for a card of the On Air page (no header, no title strip, and the
  // text size follows the card, not the screen)
  function renderer(ctx, fam) {
    const runs = [];
    const font = (k, s) => `${k === 'sans' ? '' : 'bold '}${s}px ${k === 'mono' ? fam.mono : fam.sans}`;
    const width = (k, text, s) => { ctx.font = font(k, s); return Math.round(ctx.measureText(text).width); };
    const fit = (k, text, s, maxW) => {
      if (width(k, text, s) <= maxW) return text;
      let a = [...text];
      while (a.length) { a.pop(); while (a[a.length - 1] === ' ') a.pop(); const c = a.join('') + '…'; if (width(k, c, s) <= maxW) return c; }
      return '…';
    };
    const rect = (x, y, w, h, c) => { if (w > 0 && h > 0) { ctx.fillStyle = c; ctx.fillRect(x, y, w, h); } };
    const rrect = (x, y, w, h, r, c) => { ctx.fillStyle = c; ctx.beginPath(); ctx.roundRect(x, y, w, h, r); ctx.fill(); };
    // text on a baseline; returns the width
    function text(x, base, str, s, k, c, align, maxW, zone) {
      const t = maxW == null ? str : fit(k, str, s, maxW), w = width(k, t, s);
      ctx.font = font(k, s); ctx.fillStyle = c; ctx.textBaseline = 'alphabetic'; ctx.textAlign = align === 'right' ? 'right' : align === 'center' ? 'center' : 'left';
      ctx.fillText(t, x, base);
      const left = align === 'right' ? x - w : align === 'center' ? x - w / 2 : x;
      runs.push({ zone, text: t, size: s, x: Math.round(left), w, base });
      return w;
    }
    const baselineIn = (top, lh, s) => top + Math.floor((lh + Math.round(s * CAP)) / 2);
    let zone = '';
    const line = (x, top, lh, str, s, k, c, align, maxW) => text(x, baselineIn(top, lh, s), str, s, k, c, align, maxW, zone);
    const led = (cx, cy, r, c) => { ctx.fillStyle = c; ctx.beginPath(); ctx.arc(cx, cy, r, 0, Math.PI * 2); ctx.fill(); };
    const pill = (x, top, h, str, s, bg, fg, k = 'bold') => { const pad = Math.max(6, Math.floor(h / 4)), w = width(k, str, s) + 2 * pad; rrect(x, top, w, h, Math.floor(h / 3), bg); line(x + pad, top, h, str, s, k, fg, 'left'); return w; };
    const textBase = (x, base, str, s, k, c, align) => text(x, base, str, s, k, c, align, null, zone);
    return { runs, width, fit, rect, rrect, line, textBase, baselineIn, led, pill, setZone: z => { zone = z; } };
  }
  const fmtLu = v => v == null ? '--' : v.toFixed(1);
  const num1 = (v, one) => v == null ? '--' : one ? v.toFixed(1) : v.toFixed(0);
  const pillColors = label => ({ PLAYING: ['#144a35', C.green], ASSIST: ['#553d0c', C.amber], HELD: ['#553d0c', C.amber], 'SAFE MODE': ['#7a1d22', '#ffd6d6'], 'NO CONNECTION': ['#7a1d22', '#ffd6d6'] }[label] || [C.panel2, C.dim]);
  // A reduction as a numeral: -6.3 when that fits `room` pixels, else whole dB (-10), and 0.0 or 0 for none (never -0).
  const grNumeral = (R, g, room, t) => { const one = Math.abs(g) < 0.05 ? '0.0' : g.toFixed(1); if (R.width('mono', one, t) <= room) return one; return Math.abs(g) < 0.5 ? '0' : g.toFixed(0); };
  const dbX = (tr, db) => tr[0] + Math.round(Math.min(1, Math.max(0, (db + 60) / 60)) * tr[2]);

  // M: everything the view shows (see display.js). Returns what was drawn, for the tests.
  function render(ctx, W, H, M, fam) {
    const R = renderer(ctx, fam), { zones, m, spectrum } = layout(W, H, !!M.cfg.spectrum, !!M.noHead), t = m.t;
    const rep = { t, W, H, zones, cols: [], grid: [], bars: [], spectrum, notes: [] };
    ctx.fillStyle = C.bg; ctx.fillRect(0, 0, W, H);
    // ---- header
    if (!M.noHead) { const [x0, y0, w, h] = zones.header; R.setZone('header');
      R.rect(x0, y0, w, h, M.critical ? C.redBg : C.panel);
      const barH = M.rotate != null ? Math.max(2, sc(t, 0.1)) : 0, body = h - barH, ph = body - sc(t, 0.26), py = y0 + Math.floor((body - ph) / 2);
      const clockW = R.line(x0 + w - m.pad - sc(t, 0.1), y0, body, M.clock, t, 'mono', C.text, 'right');
      let right = x0 + w - m.pad - clockW - sc(t, 0.6);
      if (M.other) {
        const name = R.fit('bold', M.other.name, t, sc(t, 5.8)), pad = Math.max(6, Math.floor(ph / 4)), ow = R.width('bold', name, t) + 2 * pad, x = right - ow;
        R.rrect(x, py, ow, ph, Math.floor(ph / 3), M.other.color); R.line(x + pad, y0, body, name, t, 'bold', C.bg, 'left'); right = x - sc(t, 0.5);
        rep.indicator = M.other.color;
      }
      const nameW = R.line(x0 + m.pad, y0, body, M.name, t, 'bold', C.text, 'left', Math.floor(w / 3));
      const [pb, pf] = pillColors(M.pill);
      let x = x0 + m.pad + nameW + sc(t, 0.5);
      x += R.pill(x, py, ph, M.pill, t, pb, pf) + sc(t, 0.4);
      if (M.connected && M.alerts.length) {
        const a = [...M.alerts].sort((p, q) => (q.severity === 'critical') - (p.severity === 'critical')), first = a[0], more = a.length - 1;
        const txt = more > 0 ? `${first.code} +${more}` : first.code, pad = Math.max(6, Math.floor(ph / 4)), room = right - x - 2 * pad;
        if (room > R.width('bold', 'ab', t)) { const tt = R.fit('bold', txt, t, room); R.rrect(x, py, R.width('bold', tt, t) + 2 * pad, ph, Math.floor(ph / 3), first.severity === 'critical' ? C.red : C.amber); R.line(x + pad, y0, body, tt, t, 'bold', C.bg, 'left'); }
      }
      if (M.rotate != null) { const y = y0 + h - barH; R.rect(x0, y, w, barH, C.line); R.rect(x0, y, Math.round((M.pinned ? 1 : Math.min(1, Math.max(0, M.rotate))) * w), barH, M.pinned ? C.red : C.amber); }
    }
    // ---- now
    if (!M.noHead) { const [x0, y0, w, h] = zones.now; R.setZone('now');
      if (!M.connected) R.line(x0 + m.pad, y0, h, 'NO CONNECTION', t, 'bold', C.red, 'left');
      else if (!M.now) R.line(x0 + m.pad, y0, h, 'Nothing on air', t, 'sans', C.dim, 'left');
      else {
        const tw = R.line(x0 + w - m.pad, y0, h, M.timeText.text, t, 'mono', M.timeText.color, 'right'), maxW = Math.max(0, w - 2 * m.pad - tw - sc(t, 0.6));
        const titleW = R.line(x0 + m.pad, y0, h, M.now.title, t, 'bold', C.text, 'left', maxW), used = titleW + sc(t, 0.6);
        if (M.now.artist && maxW - used > R.width('sans', 'abcd', t)) R.line(x0 + m.pad + used, y0, h, M.now.artist, t, 'sans', C.dim, 'left', maxW - used);
      }
    }
    // ---- cols
    const fr = M.frame;
    { const [x0z, y0z, w, h] = zones.cols; R.setZone('cols');
      const range = Math.min(30, Math.max(6, M.cfg.range)); rep.range = range;
      const cols = M.connected ? columns(M.chain, fr, M.cfg.hide) : [];
      if (M.connected && !cols.length) R.line(x0z + m.pad, y0z, m.line, 'CHAIN BYPASSED', t, 'bold', C.grey, 'left');
      if (cols.length) {
        const lh = m.line, x0 = x0z + m.pad, x1 = x0z + w - m.pad, trackTop = y0z + m.padV + lh, trackBot = y0z + h - m.padV - 2 * lh - sc(t, 0.12), trackH = Math.max(4, trackBot - trackTop);
        const numTop = trackBot + sc(t, 0.06), footTop = y0z + h - m.padV - lh;
        const hasAgc = cols.some(c => c.kind === 'agc'), nGr = cols.filter(c => c.kind !== 'agc').length;
        const agcW = hasAgc ? sc(t, 3.2) : 0, sw = nGr > 0 ? R.width('sans', '-30', t) + sc(t, 0.3) : 0, gx0 = x0 + agcW + sw;
        const cw = nGr > 0 ? Math.max(1, Math.floor((x1 - gx0) / nGr)) : 1;
        const nb = cols.filter(c => c.kind === 'band').length, names = bandNames(nb);
        const useNames = names.length > 0 && names.every(n => R.width('bold', n, t) <= cw - sc(t, 0.2));
        const bw = Math.min(Math.max(6, Math.round(cw * 0.5)), Math.max(8, sc(t, 1.7)));
        if (nGr > 0) {
          let step = 1; while (trackH * GRID * step / range < t * 1.15) step++;
          let k = 0;
          for (let d = 0; d <= range + 1e-9; d += GRID, k++) {
            const y = trackTop + Math.round(d / range * trackH); rep.grid.push([d, y]);
            R.rect(gx0, Math.min(y, trackBot - 1), x1 - gx0, 1, C.line);
            // centered on its line, but the last one sits on the track's bottom edge, clear of the numerals
            if (k % step === 0) R.textBase(gx0 - sc(t, 0.15), Math.min(R.baselineIn(y - Math.floor(lh / 2), lh, t), trackBot), d === 0 ? '0' : `-${d.toFixed(0)}`, t, 'sans', C.dim, 'right');
          }
        }
        let gi = 0, bi = 0;
        for (const c of cols) {
          let cx0, cwid;
          if (c.kind === 'agc') { cx0 = x0; cwid = agcW; } else { cx0 = gx0 + gi * cw; cwid = cw; gi++; }
          const cx = cx0 + Math.floor(cwid / 2);
          let label = c.label; if (c.kind === 'band') { if (useNames) label = names[bi]; bi++; }
          const labColor = c.enabled ? C.text : C.grey, hasLed = (c.kind === 'agc' || c.kind === 'clip') && c.enabled, lr = Math.max(3, sc(t, 0.2)), lw = R.width('bold', label, t);
          let labelX = cx, ledX = 0, ledState = null;
          if (hasLed) { const total = lw + sc(t, 0.3) + 2 * lr, lx = cx - Math.floor(total / 2); labelX = lx + Math.floor(lw / 2); ledX = lx + lw + sc(t, 0.3) + lr; }
          R.line(labelX, y0z + m.padV, lh, label, t, 'bold', labColor, 'center');
          if (hasLed) { const lit = !!fr && ((c.kind === 'agc' ? fr.flags & 2 : fr.flags & 16) !== 0); ledState = lit; R.led(ledX, y0z + m.padV + Math.floor(lh / 2), lr, !lit ? C.grey : c.kind === 'agc' ? C.amber : C.red); }
          const tw = c.kind === 'agc' ? sc(t, 0.8) : bw, tx = c.kind === 'agc' ? cx0 + cwid - tw - sc(t, 0.15) : cx - Math.floor(tw / 2);
          const track = [tx, trackTop, tw, trackH];
          R.rect(tx, trackTop, tw, trackH, c.enabled ? C.panel2 : C.off);
          let bar = [tx, trackTop, tw, 0], barDb = 0, holdY = null;
          if (!c.enabled) R.line(cx, trackTop + Math.floor(trackH / 2) - Math.floor(lh / 2), lh, 'BYP', t, 'bold', C.grey, 'center');
          else if (c.kind === 'agc') {
            const mid = trackTop + Math.floor(trackH / 2), half = trackH / 2, right = tx - sc(t, 0.2);
            R.textBase(right, trackTop + sc(t, CAP) + 2, '+9', t, 'sans', C.dim, 'right');
            R.textBase(right, R.baselineIn(mid - Math.floor(lh / 2), lh, t), '0', t, 'sans', C.dim, 'right');
            R.textBase(right, trackTop + trackH - 1, '-9', t, 'sans', C.dim, 'right');
            if (c.raw != null) {
              barDb = c.raw; const px = Math.round(Math.min(1, Math.abs(c.raw) / AGC_RANGE) * half);
              bar = c.raw >= 0 ? [tx, mid - px, tw, px] : [tx, mid, tw, px]; R.rect(bar[0], bar[1], bar[2], bar[3], C.blueLine);
            }
            R.rect(tx, mid, tw, 1, C.dim);
          } else {
            const meter = c.slot == null ? null : M.proc.gr[c.slot];
            barDb = c.slot == null ? (c.raw == null ? 0 : Math.max(0, -c.raw)) : meter.bar;
            const px = Math.round(Math.min(1, Math.max(0, barDb / range)) * trackH); bar = [tx, trackTop, tw, px]; R.rect(tx, trackTop, tw, px, C.amber);
            if (meter && meter.hold != null) {
              const hy = trackTop + Math.round(Math.min(1, Math.max(0, meter.hold / range)) * trackH), th = Math.max(2, sc(t, 0.1));
              holdY = Math.min(Math.max(hy - Math.floor(th / 2), trackTop), trackTop + trackH - th); R.rect(tx, holdY, tw, th, C.white);
            }
          }
          if (c.enabled) {
            let str, col = C.text;
            if (c.raw == null) { str = '--'; col = C.dim; } else if (c.kind === 'agc') str = (c.raw >= 0 ? '+' : '') + c.raw.toFixed(1); else { str = grNumeral(R, c.raw, cw - sc(t, 0.2), t); if (Math.abs(c.raw) < 0.05) col = C.dim; }
            R.line(cx, numTop, lh, str, t, 'mono', col, 'center');
          }
          rep.cols.push({ name: c.name, label, track, bar, bypassed: !c.enabled, gr: c.raw, barDb, range: c.kind === 'agc' ? AGC_RANGE : range, centered: c.kind === 'agc', holdY, led: ledState });
        }
        // the footer: the AGC gate, the crossovers, the limiter's ceiling
        const en = n => cols.some(c => c.name === n && c.enabled), gate = hasAgc && !!fr && (fr.flags & 2) !== 0 && en('agc');
        const ceiling = en('limiter') ? M.chain?.stages?.find(s => s.name === 'limiter')?.ceiling_dbtp ?? null : null; rep.ceiling = ceiling;
        const xo = M.chain?.multiband?.crossovers_hz || M.chain?.multiband?.crossovers || [];
        let rightW = 0; if (ceiling != null) rightW = R.line(x1, footTop, lh, `CEIL ${ceiling.toFixed(1)} dBTP`, t, 'mono', C.dim, 'right');
        let leftEnd = x0; if (gate) leftEnd = x0 + R.line(x0, footTop, lh, 'AGC GATE', t, 'bold', C.amber, 'left') + sc(t, 0.8);
        if (en('band1') && xo.length) { const room = x1 - leftEnd - rightW - (rightW > 0 ? sc(t, 0.8) : 0); if (room > 0) R.line(leftEnd, footTop, lh, `XOVER ${xo.map(hzText).join(' ')}`, t, 'mono', C.dim, 'left', room); }
      }
    }
    // ---- spectrum
    if (spectrum) {
      const [x0, y0, w, h] = zones.spectrum; R.setZone('spectrum');
      if (M.connected) {
        const labW = R.width('sans', '-40', t) + sc(t, 0.4), plot = [x0 + m.pad + labW, y0 + m.padV, w - 2 * m.pad - labW, h - 2 * m.padV];
        const yat = db => plot[1] + Math.round((1 - Math.min(1, Math.max(0, (db + 60) / 60))) * plot[3]);
        for (const db of [-20, -40]) { R.rect(plot[0], yat(db), plot[2], 1, C.line); R.line(plot[0] - sc(t, 0.2), yat(db) - Math.floor(m.line / 2), m.line, String(db), t, 'sans', C.dim, 'right'); }
        const bwid = plot[2] / 64, D = M.spectra, src = M.cfg.spectrumSource || 'post';
        if (src !== 'pre') for (let i = 0; i < 64; i++) {
          const a = plot[0] + Math.round(i * bwid), b = plot[0] + Math.round((i + 1) * bwid), top = yat(D.post[i]);
          R.rect(a, top, Math.max(1, b - a - 1), plot[1] + plot[3] - top, C.blue); R.rect(a, Math.max(plot[1], yat(D.capPost[i]) - 1), Math.max(1, b - a - 1), 2, C.blueLine);
        }
        if (src !== 'post') { ctx.strokeStyle = '#e8edf2c8'; ctx.lineWidth = 2; ctx.beginPath(); for (let i = 0; i < 64; i++) { const x = plot[0] + (i + 0.5) * bwid, y = yat(D.pre[i]); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); } ctx.stroke(); }
      }
    }
    // ---- meters
    { const [x0z, y0z, w, h] = zones.meters; R.setZone('meters');
      if (M.connected) {
        const row = sc(t, 1.22), leftX = x0z + m.pad, splitX = x0z + Math.round(w * (M.noHead ? 0.42 : 0.53)), leftR = splitX - sc(t, 0.9);
        R.rect(splitX - sc(t, 0.45), y0z + m.padV, 1, h - 2 * m.padV, C.line);
        const labW = R.width('bold', 'IN', t) + sc(t, 0.3), numW = R.width('mono', '-88.8', t), barX = leftX + labW, barW = Math.max(20, leftR - numW - sc(t, 0.3) - barX), barH = Math.max(6, sc(t, 0.78));
        const ceiling = rep.ceiling;
        [['IN', M.proc.input, M.proc.inputHold], ['L', M.bars.l, M.bars.holdL], ['R', M.bars.r, M.bars.holdR]].forEach(([lab, level, hold], i) => {
          const top = y0z + m.padV + i * row, tr = [barX, top + Math.floor((row - barH) / 2), barW, barH];
          R.line(leftX, top, row, lab, t, 'bold', C.dim, 'left');
          R.rect(tr[0], tr[1], tr[2], tr[3], C.bg);
          const fillX = level == null ? tr[0] : dbX(tr, level);
          for (const [lo, hi, col] of [[-60, -12, C.green], [-12, -3, C.amber], [-3, 0, C.red]]) { const a = dbX(tr, lo), b = Math.min(dbX(tr, hi), fillX); if (b > a) R.rect(a, tr[1], b - a, tr[3], col); }
          if (hold != null) R.rect(Math.min(dbX(tr, hold), tr[0] + tr[2] - 2), tr[1], 2, tr[3], C.white);
          for (const tk of [-40, -20, 0]) R.rect(Math.min(dbX(tr, tk), tr[0] + tr[2] - 2), tr[1], 2, tr[3], 'rgba(42,52,66,0.9)');
          if (i > 0 && ceiling != null) R.rect(Math.min(dbX(tr, ceiling), tr[0] + tr[2] - 3), tr[1] - 3, 3, tr[3] + 6, C.amber);
          rep.bars.push({ name: lab, track: tr, db: level, fillX, pxPerDb: barW / 60 });
          R.line(leftR, top, row, num1(level, true), t, 'mono', level != null ? C.text : C.dim, 'right');
        });
        const rx = splitX, rw = x0z + w - m.pad - rx, state = fr ? fr.lstate : 0;
        const [label, bg, fg] = [['OFF', C.panel2, C.dim], ['CONVERGING', C.panel2, C.blueLine], ['ON TARGET', '#144a35', C.green], ['FROZEN', '#553d0c', C.amber], ['AT LIMIT', C.redBg, C.red]][state] || ['OFF', C.panel2, C.dim];
        const top = y0z + m.padV, ph = row - sc(t, 0.2), pw = R.pill(rx, top + Math.floor((row - ph) / 2), ph, label, t, bg, fg);
        const gain = state === 0 ? '' : `${fr.gain >= 0 ? '+' : ''}${fr.gain.toFixed(1)} dB`;
        if (gain) R.line(rx + pw + sc(t, 0.5), top, row, gain, t, 'mono', C.text, 'left', Math.max(0, rw - pw - sc(t, 0.5)));
        R.line(rx, top + row, row, `TARGET ${fr?.target == null ? '--' : fr.target.toFixed(1)}   W ${fmtLu(fr?.window)}`, t, 'mono', C.text, 'left', rw);
        R.line(rx, top + 2 * row, row, `S ${fmtLu(fr?.shortTerm)}   I ${fmtLu(fr?.integrated)}`, t, 'mono', C.text, 'left', rw);
      }
    }
    rep.runs = R.runs;
    return rep;
  }
  return { layout, metrics, textPx, columns, render, bandNames, hzText, grNumeral, C, AGC_RANGE, GRID };
})();
if (typeof module !== 'undefined') module.exports = ProcView;
