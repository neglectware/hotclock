// The On Air page's own layout (docs/design/onair-proc-2026-10-09/OPTIONS.md): the three compact processing displays and
// the Layout control. A layout to try out for a while, kept apart from app.js so it can be simplified later:
//   OnAirProc    the processing displays: A, the meter bridge (a thin row under both columns); B, a line in the on-air unit;
//                C, a small rack unit at the bottom of the left column. Each one is built from the meter frame the page already
//                receives (parseFrame) and the chain's preset (GET /chain); A and C also read the AGC's state (GET /chain/agc)
//                about once a second while they are shown, for its FADE and CLAMPING lights.
//   OnAirLayout  the choices (processing display, queue size, program unit, column split, the order on a phone), kept per
//                browser in localStorage. Every access is in try/catch: without storage the page uses the default layout.
const OnAirProc = (() => {
  const BANDS = { 1: ['ALL'], 2: ['LOW', 'HIGH'], 3: ['LOW', 'MID', 'HIGH'], 4: ['LOW', 'LO-MID', 'HI-MID', 'HIGH'], 5: ['LOW', 'LO-MID', 'MID', 'HI-MID', 'HIGH'], 6: ['1', '2', '3', '4', '5', '6'] };
  const GREEN = '#2f9e6e', GREY = '#4a566c', BLUE = '#4f7fe0', AMBER = '#f5a623';
  // the AGC: from GET /chain/agc when it has answered lately, else from the frame (RIDING, or GATED with flag bit 1)
  const AGC = { riding: ['RIDING', GREEN], gated: ['GATED', GREY], riding_fade: ['FADE', BLUE], clamping: ['CLAMPING', AMBER] };
  // the loudness controller's state in the frame (byte 216): 0 off, 1 converging, 2 on target, 3 frozen, 4 at limit
  const LOUD = [null, ['RIDING', GREEN], ['ON TARGET', GREEN], ['HOLDING', GREY], ['AT LIMIT', AMBER]];
  const GRS = [['WB', 'Wideband compressor', 'wb'], ['CLIP', 'Clipper', 'clip'], ['LIM', 'True-peak limiter', 'lim'], ['SAFE', 'Safety limiter', 'safe']];
  // the numbers: one decimal, an ASCII minus, at most 99.9 dB either way, so the widest is 5 characters ("-24.0", "+12.0");
  // each readout has a fixed width in the monospaced font (onair.css: .pm-n, .pm-n.db, .pm-lufs), right-aligned, so a value that
  // gets a sign or a digit, or goes to "--", moves nothing beside it
  const clamp = x => Math.max(-99.9, Math.min(99.9, x));
  const sg = x => x == null ? '--' : (x > 0.049 ? '+' : '') + (Math.abs(x) < 0.05 ? 0 : clamp(x)).toFixed(1);
  const gr = x => x == null ? '--' : (Math.abs(x) < 0.05 ? 0 : clamp(x)).toFixed(1);
  const tgt = d => 'target ' + (d?.loud.target != null ? clamp(d.loud.target).toFixed(1) : '--').padStart(5);

  // ---- the model: the frame's values; the gain reductions fall back at 15 dB/s (the GR release of the box's meters) ----
  class Model {
    constructor() { this.shown = {}; this.last = 0; this.agc = null; this.agcAt = 0; }
    rel(k, v, dt) { if (v == null) { this.shown[k] = null; return null; } const s = this.shown[k]; return this.shown[k] = s == null || v < s ? v : Math.min(v, s + 15 * dt); }
    setAgc(v) { this.agc = v; this.agcAt = Date.now(); }
    update(f, now) {
      const dt = Math.min(1, Math.max(0, (now - (this.last || now)) / 1000)); this.last = now;
      if (!f) { this.shown = {}; return null; }
      const on = i => (f.stageMask >> i & 1) === 1, n = Math.min(6, Math.max(1, f.bandCount || 5));
      const a = this.agc && Date.now() - this.agcAt < 3000 ? this.agc : null;
      const agcState = !on(0) || (a && a.enabled === false) ? null : a && AGC[a.state] ? a.state : (f.flags & 2) !== 0 ? 'gated' : 'riding';
      return {
        bypassed: f.bypassed,
        agc: { state: agcState, gain: on(0) ? f.stages[0].gr : null },
        mbOn: on(1),
        bands: Array.from({ length: n }, (_, i) => ({ name: BANDS[n][i], gr: this.rel('b' + i, on(1) ? Math.min(0, f.bandGr[i] ?? 0) : null, dt) })),
        wb: this.rel('wb', on(2) ? f.stages[2].gr : null, dt), lim: this.rel('lim', on(3) ? f.stages[3].gr : null, dt),
        clip: this.rel('clip', on(4) ? f.stages[4].gr : null, dt), safe: this.rel('safe', f.stages[5].gr, dt),
        loud: { state: f.lstate || 0, gain: f.lstate ? f.gain : null, target: f.target },
      };
    }
  }

  // ---- small parts in the rack's vocabulary (style.css: .rlight, .rbar, .rbar.bp, .rgain); every write is change-only ----
  const setW = (i, prop, v) => { if (i.style[prop] !== v) i.style[prop] = v; };
  // a light; with `words`, its word sits over every word it can say (hidden), so it is as wide as the widest one
  const lamp = (word, color, words) => {
    const t = h('span', { class: 'rlt' }, word);
    const el = h('span', { class: 'rlight pm-lamp', style: `--lc:${color}` }, h('i'), words ? h('span', { class: 'pm-lw' }, ...words.map(w => h('span', { class: 'pm-lws', 'aria-hidden': 'true' }, w)), t) : t);
    el.rlt = t; return el;
  };
  const paintLamp = (lp, on, word, color) => { if (lp.classList.contains('on') !== !!on) lp.classList.toggle('on', !!on); if (word != null) setText(lp.rlt, word); if (color && lp.style.getPropertyValue('--lc') !== color) lp.style.setProperty('--lc', color); };
  const hbar = () => { const i = h('i'); return { el: h('div', { class: 'rbar pm-gr' }, i), set(g, R = 12) { setW(i, 'width', g == null ? '0%' : (Math.min(R, Math.abs(g)) / R * 100).toFixed(1) + '%'); } }; };
  const cbar = () => { const i = h('i'); return { el: h('div', { class: 'rbar bp pm-gain' }, i, h('b', { class: 'mid' })), set(g, R = 12) { if (g == null) { setW(i, 'width', '0%'); return; } const w = Math.min(R, Math.abs(g)) / R * 50; setW(i, 'left', (g >= 0 ? 50 : 50 - w).toFixed(1) + '%'); setW(i, 'width', w.toFixed(1) + '%'); } }; };
  const vbar = () => { const i = h('i'); return { el: h('div', { class: 'pm-vb' }, i), set(g, R = 12) { setW(i, 'height', g == null ? '0%' : (Math.min(R, Math.abs(g)) / R * 100).toFixed(1) + '%'); } }; };
  const num = (cls = '') => h('b', { class: 'mono pm-n' + cls }, '--');
  const glab = t => h('span', { class: 'pm-l' }, t);
  const link = (t, cls = 'pm-more') => h('a', { class: cls, href: '#/processing', title: 'The Processing page: the rack' }, t);
  const agcWord = d => d.agc.state ? AGC[d.agc.state] : ['OFF', GREY];
  const loudWord = d => LOUD[d.loud.state] || ['OFF', GREY];
  // the band bars: six are built, those past the frame's band count are hidden; without a frame the last count stays (empty
  // bars), so the bars do not go and come back
  const bandBars = (mk = vbar) => Object.assign(Array.from({ length: 6 }, () => mk()), { n: 5 });
  const paintBands = (bars, d, each) => { if (d) bars.n = d.bands.length; bars.forEach((b, i) => {
    const x = d && d.bands[i], el = b.el.closest('.pm-bc') || b.el, show = d ? !!x : i < bars.n; if (el.hidden !== !show) el.hidden = !show;
    if (x) { b.set(x.gr); const t = `${x.name} ${gr(x.gr)} dB`; if (b.el.title !== t) b.el.title = t; } else b.set(null); if (each) each(b, x, i);
  }); };

  // ---- A: the meter bridge, one row under both columns, like the bridge on top of a console ----
  function bridge() {
    const agcL = lamp('RIDING', GREEN, ['RIDING', 'GATED', 'FADE', 'CLAMPING', 'OFF']), agcN = num();
    const bands = bandBars(), bandN = num();
    const gs = GRS.map(([t, tip, k]) => ({ t, k, b: hbar(), n: num(), tip }));
    const ldL = lamp('ON TARGET', GREEN, ['OFF', 'RIDING', 'ON TARGET', 'HOLDING', 'AT LIMIT']), ldN = num(' db'), ldT = h('span', { class: 'mono pm-lufs' });
    const preset = link('', 'pm-preset'), byp = h('span', { class: 'chip crit pm-byp', hidden: true }, 'bypassed');
    const sep = () => h('span', { class: 'pm-sep', 'aria-hidden': 'true' });
    const el = h('section', { class: 'unit pm-bridge', id: 'procdisp', 'data-card': 'processing', 'data-proc': 'a', 'aria-label': 'Processing' },
      h('div', { class: 'pm-row' },
        h('div', { class: 'pm-g pm-name' }, h('span', {}, glab('Processing'), helpLink('onair', 'processing')), h('span', {}, preset, byp)), sep(),
        h('div', { class: 'pm-g pm-agc', title: 'AGC: its state and its gain now' }, glab('AGC'), h('span', {}, agcL, agcN)), sep(),
        h('div', { class: 'pm-g pm-mb', title: 'Multiband compressor: the gain reduction of each band, and of the band that reduces most' }, glab('Multiband'), h('span', {}, h('span', { class: 'pm-bands' }, bands.map(b => b.el)), bandN)), sep(),
        ...gs.map(g => h('div', { class: 'pm-g pm-grg', 'data-gr': g.k, title: g.tip + ': gain reduction now, 0 to -12 dB' }, glab(g.t), h('span', {}, g.b.el, g.n))), sep(),
        h('div', { class: 'pm-g pm-ld', title: 'Loudness controller: its state, its gain and its target (the loudness itself is in the on-air unit: M, S, W, I)' }, glab('Loudness'), h('span', {}, ldL, ldN, ldT)),
        link('Processing ›')));
    return {
      el, agc: true,
      preset(name) { setText(preset, name || ''); preset.hidden = !name; },
      paint(d) {
        if (byp.hidden !== !d?.bypassed) byp.hidden = !d?.bypassed;
        const [aw, ac] = d ? agcWord(d) : ['OFF', GREY]; paintLamp(agcL, d && d.agc.state, aw, ac); setText(agcN, d ? sg(d.agc.gain) : '--');
        paintBands(bands, d); setText(bandN, d && d.mbOn ? gr(Math.min(...d.bands.map(b => b.gr ?? 0))) : '--');
        gs.forEach(g => { const v = d ? d[g.k] : null; g.b.set(v); setText(g.n, gr(v)); });
        const [lw, lc] = d ? loudWord(d) : ['OFF', GREY]; paintLamp(ldL, d && LOUD[d.loud.state], lw, lc); setText(ldN, d && d.loud.gain != null ? sg(d.loud.gain) + ' dB' : '--');
        setText(ldT, tgt(d));
      },
    };
  }

  // ---- B: one line in the on-air unit, under the levels ----
  function inline() {
    const agcL = lamp('AGC', GREEN, ['AGC', 'AGC OFF', 'GATED', 'FADE', 'CLAMPING']), agcN = num();
    const bands = bandBars();
    const gs = GRS.slice(0, 3).map(([t, tip, k]) => ({ t, k, b: hbar(), n: num(), tip }));
    const ldL = lamp('ON TARGET', GREEN, ['LOUD OFF', 'RIDING', 'ON TARGET', 'HOLDING', 'AT LIMIT']), ldN = num(' db');
    const tag = link('Proc', 'pm-tag');
    const el = h('div', { class: 'pm-inline', id: 'procdisp', 'data-card': 'processing', 'data-proc': 'b', role: 'group', 'aria-label': 'Processing' },
      tag,
      h('span', { class: 'pm-i pm-agc', title: 'AGC: its state and its gain now' }, agcL, agcN),
      h('span', { class: 'pm-i pm-mb', title: 'Multiband compressor: the gain reduction of each band' }, glab('MB'), h('span', { class: 'pm-bands' }, bands.map(b => b.el))),
      ...gs.map(g => h('span', { class: 'pm-i pm-grg', 'data-gr': g.k, title: g.tip + ': gain reduction now' }, glab(g.t), g.b.el, g.n)),
      h('span', { class: 'pm-i pm-ld', title: 'Loudness controller: its state and its gain' }, ldL, ldN));
    return {
      el, agc: false,
      preset(name) { const t = (name ? `Processing: ${name}. ` : '') + 'The Processing page: the rack'; if (tag.title !== t) tag.title = t; },
      paint(d) {
        setText(tag, d?.bypassed ? 'Bypassed' : 'Proc'); if (tag.classList.contains('byp') !== !!d?.bypassed) tag.classList.toggle('byp', !!d?.bypassed);
        const [aw, ac] = d ? agcWord(d) : ['OFF', GREY]; paintLamp(agcL, d && d.agc.state, d && d.agc.state === 'riding' ? 'AGC' : aw === 'OFF' ? 'AGC OFF' : aw, ac); setText(agcN, d ? sg(d.agc.gain) : '--');
        paintBands(bands, d);
        gs.forEach(g => { const v = d ? d[g.k] : null; g.b.set(v); setText(g.n, gr(v)); });
        const [lw, lc] = d ? loudWord(d) : ['OFF', GREY]; paintLamp(ldL, d && LOUD[d.loud.state], lw === 'OFF' ? 'LOUD OFF' : lw, lc); setText(ldN, d && d.loud.gain != null ? sg(d.loud.gain) + ' dB' : '');
      },
    };
  }

  // ---- C: a small rack unit: AGC and loudness (the lit light, the gain, the centred gain bar), the bands, the GR bars ----
  function rack() {
    const AL = Object.entries(AGC).map(([k, [w, c]]) => Object.assign(lamp(w, c), { st: k }));
    const LL = LOUD.slice(1).map(([w, c], i) => Object.assign(lamp(w, c), { st: i + 1 }));
    const agcOff = lamp('OFF', GREY), ldOff = lamp('OFF', GREY);
    const agcB = cbar(), agcN = num(' db'), ldB = cbar(), ldN = num(' db'), ldT = h('span', { class: 'mono pm-lufs' });
    const cells = bandBars(() => { const b = vbar(), nm = h('span', { class: 'pm-bn' }), v = h('span', { class: 'mono pm-bv' }); b.nm = nm; b.v = v; h('div', { class: 'pm-bc' }, b.el, nm, v); return b; });
    const gs = GRS.map(([t, tip, k]) => ({ t, k, b: hbar(), n: num(), tip }));
    const chip = h('span', { class: 'chip pm-chip' });
    const el = h('section', { class: 'unit pm-unit', id: 'procdisp', 'data-card': 'processing', 'data-proc': 'c' },
      h('div', { class: 'uhead' }, h('span', { class: 't' }, 'Processing'), helpLink('onair', 'processing'), chip, h('span', { class: 'r' }, link('Open the rack ›'))),
      h('div', { class: 'pm-c' },
        h('div', { class: 'pm-cell pm-agc', title: 'AGC: its state and its gain now (12 dB each way)' }, h('div', { class: 'pm-ch' }, glab('AGC'), h('span', { class: 'rlights pm-lights' }, AL, agcOff), agcN), h('div', { class: 'rgain' }, agcB.el)),
        h('div', { class: 'pm-cell pm-ld', title: 'Loudness controller: its state, its gain (12 dB each way) and its target' }, h('div', { class: 'pm-ch' }, glab('Loudness'), h('span', { class: 'rlights pm-lights' }, LL, ldOff), ldN), h('div', { class: 'rgain' }, ldB.el, ldT)),
        h('div', { class: 'pm-cell pm-mb', title: 'Multiband compressor: the gain reduction of each band, 0 to -12 dB' }, glab('Multiband'), h('div', { class: 'pm-bcs' }, cells.map(c => c.el.parentElement))),
        h('div', { class: 'pm-cell pm-grs' }, ...gs.map(g => h('div', { class: 'pm-grl', 'data-gr': g.k, title: g.tip + ': gain reduction now, 0 to -12 dB' }, glab(g.t), g.b.el, g.n)))));
    let name = '';
    return {
      el, agc: true,
      preset(n) { name = n || ''; },
      paint(d) {
        setText(chip, d?.bypassed ? 'bypassed' : name); const cc = 'chip pm-chip' + (d?.bypassed ? ' crit' : ''); if (chip.className !== cc) chip.className = cc;
        if (chip.hidden !== !chip.textContent) chip.hidden = !chip.textContent;
        const ast = d ? d.agc.state : null;
        AL.forEach(l => paintLamp(l, l.st === ast)); paintLamp(agcOff, !ast); agcB.set(d ? d.agc.gain : null); setText(agcN, d && d.agc.gain != null ? sg(d.agc.gain) + ' dB' : '--');
        const ls = d ? d.loud.state : 0;
        LL.forEach(l => paintLamp(l, l.st === ls)); paintLamp(ldOff, !LOUD[ls]); ldB.set(d ? d.loud.gain : null); setText(ldN, d && d.loud.gain != null ? sg(d.loud.gain) + ' dB' : '--');
        setText(ldT, tgt(d));
        paintBands(cells, d, (b, x, i) => { setText(b.nm, x ? x.name : BANDS[cells.n]?.[i] ?? ''); setText(b.v, x ? gr(x.gr) : '--'); });
        gs.forEach(g => { const v = d ? d[g.k] : null; g.b.set(v); setText(g.n, gr(v)); });
      },
    };
  }

  return { Model, build: { a: bridge, b: inline, c: rack } };
})();

const OnAirLayout = (() => {
  const KEY = 'hotclock.onair.layout';
  const DEFAULT = Object.freeze({ proc: 'a', queue: 'full', prog: 'full', split: '50', phone: 'queue' });
  const CHOICES = {
    proc: [['a', 'Meter bridge', 'a row under both columns'], ['b', 'In the on-air unit', 'a line under the levels'], ['c', 'Rack unit', 'in the left column'], ['off', 'Off', 'no processing on this page']],
    queue: [['full', 'Full'], ['4', '4'], ['6', '6'], ['8', '8'], ['12', '12']],
    prog: [['full', 'Full', 'the rundown scrolls inside'], ['compact', 'Compact', 'status and the current block']],
    split: [['40', '40 / 60'], ['50', '50 / 50'], ['60', '60 / 40']],
    phone: [['queue', 'Queue first'], ['program', 'Program first']],
  };
  const valid = (k, v) => CHOICES[k].some(c => c[0] === v);
  // the stored layout, each value checked; anything missing, unknown or unreadable is the default
  function load() {
    const out = { ...DEFAULT };
    let raw = null; try { raw = JSON.parse(localStorage.getItem(KEY) || 'null'); } catch { raw = null; }
    if (raw && typeof raw === 'object') for (const k of Object.keys(DEFAULT)) { const v = raw[k] == null ? null : String(raw[k]); if (v != null && valid(k, v)) out[k] = v; }
    return out;
  }
  function save(l) {
    try { if (Object.keys(DEFAULT).every(k => l[k] === DEFAULT[k])) localStorage.removeItem(KEY); else localStorage.setItem(KEY, JSON.stringify(l)); } catch { }
  }
  // The Layout control: a button and its popover. `onchange(layout)` applies a change at once; `onCleanup` is the page's.
  function control(cur, onchange, onCleanup) {
    const btn = h('button', { type: 'button', class: 'f-layout', 'aria-haspopup': 'true', 'aria-expanded': 'false', title: 'The layout of this page, kept in this browser' }, 'Layout');
    const pop = h('div', { class: 'menu lpop', role: 'dialog', 'aria-label': 'Layout of the On Air page', hidden: true });
    const groups = {};
    const seg = (k, label, note) => {
      const box = h('div', { class: 'segc', role: 'group', 'aria-label': label });
      groups[k] = box;
      box.append(...CHOICES[k].map(([v, t, sm]) => h('button', { type: 'button', 'data-k': k, 'data-v': v, 'aria-pressed': 'false', title: sm || null, onclick: () => set(k, v) }, t)));
      return h('div', { class: 'lgrp', 'data-group': k }, h('div', { class: 'fl' }, label, note ? h('small', {}, ' ' + note) : null), box);
    };
    const reset = h('button', { type: 'button', class: 'lreset f-layout-reset', onclick: () => { cur = { ...DEFAULT }; save(cur); paint(); onchange({ ...cur }); } }, 'Reset to default');
    pop.append(seg('proc', 'Processing display'), seg('queue', 'Queue', 'rows shown; Full fills the column'), seg('prog', 'Program unit'), seg('split', 'Columns', 'program : queue, on a desktop'),
      seg('phone', 'On a phone or tablet', 'which comes after the keys'), h('div', { class: 'lfoot' }, reset, helpLink('onair', 'layout')));
    function paint() {
      for (const [k, box] of Object.entries(groups)) for (const b of box.children) { const on = b.dataset.v === cur[k]; b.classList.toggle('on', on); b.setAttribute('aria-pressed', String(on)); }
      const d = Object.keys(DEFAULT).every(k => cur[k] === DEFAULT[k]); reset.disabled = d;
    }
    function set(k, v) { if (cur[k] === v) return; cur = { ...cur, [k]: v }; save(cur); paint(); onchange({ ...cur }); }
    let open = false;
    const outside = ev => { if (!pop.contains(ev.target) && !btn.contains(ev.target)) close(); };
    const onKey = ev => { if (ev.key === 'Escape') { close(); btn.focus(); } };
    const place = () => {
      // the popover is placed under the button and kept inside the window (a phone's head wraps: the button may be anywhere)
      const r = wrap.getBoundingClientRect(), w = Math.min(380, innerWidth - 24);
      const left = Math.min(Math.max(12, r.right - w), innerWidth - 12 - w) - r.left;
      pop.style.width = w + 'px'; pop.style.left = left + 'px';
    };
    function close() { if (!open) return; open = false; pop.hidden = true; btn.setAttribute('aria-expanded', 'false'); document.removeEventListener('click', outside); document.removeEventListener('keydown', onKey); removeEventListener('resize', place); }
    function show() { open = true; paint(); pop.hidden = false; place(); btn.setAttribute('aria-expanded', 'true'); setTimeout(() => { if (open) { document.addEventListener('click', outside); document.addEventListener('keydown', onKey); addEventListener('resize', place); } }, 0); pop.querySelector('button.on')?.focus(); }
    btn.addEventListener('click', ev => { ev.stopPropagation(); if (open) close(); else show(); });
    onCleanup(close);
    const wrap = h('span', { class: 'layoutctl' }, btn, pop);
    paint();
    return wrap;
  }
  return { KEY, DEFAULT, CHOICES, load, save, control };
})();
if (typeof module !== 'undefined') module.exports = { OnAirProc, OnAirLayout };
