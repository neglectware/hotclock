// The controls of the Processing panel (processing.js): rotary knobs, vertical faders, segmented switches, toggle
// buttons, level and gain-reduction meters and the equalizer's curve editor. Plain DOM and SVG, no dependencies.
//
// Every control is keyboard operable and carries ARIA (`role=slider`, `aria-valuenow`, `aria-valuetext`), and every
// value is also written as text, so nothing is shown by colour or position alone.
//   knob / fader   drag up or down (Shift: fine), arrow keys (Shift or Page: x10, Home and End: the ends),
//                  double-click or Backspace resets to the default, click the number (or Enter) to type a value
//   onChange(v, final)   called at most every 100 ms while dragging (final false) and once on release (final true)
const PW = (() => {
  const NS = 'http://www.w3.org/2000/svg';
  const sv = (tag, attrs, ...kids) => {
    const e = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs || {})) if (v != null && v !== false) e.setAttribute(k, v);
    for (const c of kids.flat()) if (c) e.append(c);
    return e;
  };
  const clamp = (x, a, b) => Math.min(b, Math.max(a, x));
  const trimZeros = s => s.includes('.') ? s.replace(/0+$/, '').replace(/\.$/, '') : s;
  const SHORT = { 'dBFS RMS': 'dBFS', 'dB re target': 'dB', 'dB/s': 'dB/s', ': 1': ':1', 'dB per minute': 'dB/min' };
  const shortUnit = u => SHORT[u] ?? u ?? '';

  // A parameter of the schema as the numbers a control needs.
  function spec(p, extra) {
    const min = p.min, max = p.max, int = p.kind === 'int';
    const log = !int && min > 0 && max / min >= 10;
    const r = max - min;
    // `exact`: the schema names the parameter's resolution (the equalizer's 1 Hz, 0.1 dB, 0.01 Q): a typed value is
    // kept to that resolution, a drag is coarse (Shift: to the resolution), arrows step in proportion
    const exact = !int && p.step > 0;
    const step = exact ? p.step : int ? 1 : r <= 2 ? 0.01 : r <= 30 ? 0.1 : r <= 300 ? 0.5 : r <= 3000 ? 1 : 5;
    return { min, max, int, log, exact, step, def: p.default, unit: p.unit || '', label: p.label, help: p.help, ...extra };
  }
  function fmtValue(v, o, withUnit = true) {
    if (v == null || !isFinite(v)) return '--';
    let t, u = shortUnit(o.unit);
    if (o.exact) t = v.toFixed(Math.max(0, Math.round(-Math.log10(o.step))));   // every digit of the resolution: 4087, -1.7, 1.57
    else if (o.int) t = String(Math.round(v));
    else if (o.unit === 'Hz' && Math.abs(v) >= 1000) { t = trimZeros((v / 1000).toFixed(2)); u = 'kHz'; }
    else if (o.log) { const a = Math.abs(v); t = trimZeros(v.toFixed(a >= 100 ? 0 : a >= 10 ? 1 : 2)); }
    else t = trimZeros(v.toFixed(o.step < 0.1 ? 2 : o.step < 1 ? 1 : 0));
    if (!withUnit || !u) return t;
    return u === ':1' ? t + ':1' : t + ' ' + u;
  }

  // The shared behaviour of knob and fader: the value, snapping, keyboard, reset, typed entry, throttled output.
  function core(o, hooks) {
    let v = clamp(o.value ?? o.def ?? o.min, o.min, o.max);
    let tm = null, entering = false, wheelTm = null;
    const st = { dragging: false };
    const toF = x => clamp(o.log ? Math.log(x / o.min) / Math.log(o.max / o.min) : (x - o.min) / (o.max - o.min), 0, 1);
    // How a number is kept: `how` 'drag' is the coarse feel of a knob; 'exact' (typed entry, arrows, Shift-drag) keeps
    // the resolution of a parameter that names one.
    const quant = x => clamp(Number((Math.round(x / o.step) * o.step).toFixed(6)), o.min, o.max);
    const snap = (x, how) => {
      x = clamp(x, o.min, o.max);
      if (o.int) return Math.round(x);
      if (o.exact) {
        if (how === 'drag') return quant(o.log ? Number(x.toPrecision(o.unit === 'Hz' ? 3 : 2)) : Math.round(x / (o.step * 5)) * o.step * 5);
        return quant(x);
      }
      if (o.log) return clamp(Number(x.toPrecision(3)), o.min, o.max);
      return clamp(Number((Math.round(x / o.step) * o.step).toFixed(6)), o.min, o.max);
    };
    const fromF = (f, fine) => snap(o.log ? o.min * Math.pow(o.max / o.min, clamp(f, 0, 1)) : o.min + clamp(f, 0, 1) * (o.max - o.min), fine ? 'exact' : 'drag');
    const api = {
      st, toF, fromF, snap, paint: () => hooks.render(v), get: () => v, busy: () => st.dragging || entering || tm != null,
      set(x) { if (api.busy() || x == null) return; v = snap(x, 'exact'); hooks.render(v); },
      // A change the user made: show it now, tell the owner soon (final: now, once).
      user(x, final, how) {
        const n = snap(x, how || 'exact');
        const changed = n !== v;
        v = n; hooks.render(v);
        if (final) { clearTimeout(tm); tm = null; o.onChange?.(v, true); }
        else if (changed && !tm) tm = setTimeout(() => { tm = null; o.onChange?.(v, false); }, 100);
      },
      reset() { if (o.def != null) api.user(o.def, true); },
      // One notch up (dir 1) or down (-1); `mult` 10 for Shift and the Page keys; `alt`: exactly one resolution step.
      // A parameter with a resolution steps in proportion: a notch is 1 percent of a log value (at least one resolution
      // step), 5 resolution steps of a linear one (0.5 dB).
      nudge(dir, mult, final, alt) {
        if (o.exact) {
          const d = alt ? o.step : o.log ? Math.max(o.step, Math.round(v * 0.01 / o.step) * o.step) * mult : o.step * 5 * mult;
          const n = quant(v + dir * d);
          api.user(n, final);
          return;
        }
        const n = o.log ? snap(v * Math.pow(1.02, dir * mult)) : snap(v + dir * mult * o.step);
        // snapping to three digits can return the same number for a small log step: move by one digit instead
        if (o.log && n === v) api.user(v * (dir > 0 ? 1.05 : 1 / 1.05), final); else api.user(n, final);
      },
    };
    api.keys = ev => {
      const big = ev.shiftKey || ev.key === 'PageUp' || ev.key === 'PageDown' ? 10 : 1;
      let handled = true;
      switch (ev.key) {
        case 'ArrowUp': case 'ArrowRight': case 'PageUp': api.nudge(1, big, false, ev.altKey); break;
        case 'ArrowDown': case 'ArrowLeft': case 'PageDown': api.nudge(-1, big, false, ev.altKey); break;
        case 'Home': api.user(o.min, false); break;
        case 'End': api.user(o.max, false); break;
        case 'Backspace': case 'Delete': api.reset(); break;
        case 'Enter': hooks.entry?.(); break;
        default: handled = false;
      }
      if (handled) { ev.preventDefault(); clearTimeout(wheelTm); wheelTm = setTimeout(() => { clearTimeout(tm); tm = null; o.onChange?.(v, true); }, 250); }
    };
    // Typed entry in place of the readout button.
    api.entry = (btn, aria) => {
      if (entering) return;
      entering = true;
      const inp = h('input', { type: 'number', class: 'pk-input', step: 'any', min: o.min, max: o.max, value: String(v), 'aria-label': aria + ' (' + (o.unit || 'number') + ')' });
      let done = false;
      const end = commit => {
        if (done) return; done = true;
        const x = parseFloat(inp.value);
        entering = false;
        inp.replaceWith(btn);
        if (commit && isFinite(x)) api.user(x, true); else hooks.render(v);
        btn.focus?.();
      };
      inp.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); end(true); } else if (e.key === 'Escape') { e.preventDefault(); end(false); } e.stopPropagation(); });
      inp.addEventListener('blur', () => end(true));
      btn.replaceWith(inp); inp.focus(); inp.select();
    };
    return api;
  }

  const arcPt = (cx, cy, r, deg) => { const a = deg * Math.PI / 180; return [cx + r * Math.sin(a), cy - r * Math.cos(a)]; };
  const arcPath = (cx, cy, r, a0, a1) => {
    if (Math.abs(a1 - a0) < 0.3) return '';
    const [x0, y0] = arcPt(cx, cy, r, a0), [x1, y1] = arcPt(cx, cy, r, a1);
    return `M${x0.toFixed(2)} ${y0.toFixed(2)}A${r} ${r} 0 ${Math.abs(a1 - a0) > 180 ? 1 : 0} ${a1 > a0 ? 1 : 0} ${x1.toFixed(2)} ${y1.toFixed(2)}`;
  };

  // ---- a rotary knob ----
  // opts: spec() fields plus { value, onChange, size, small, key }
  function knob(opts) {
    const o = { ...opts };
    const size = o.small ? 44 : 58;
    const bipolar = o.min < 0 && o.max > 0 && !o.log;
    const track = sv('path', { class: 'pk-track', fill: 'none', 'stroke-width': 4, 'stroke-linecap': 'round' });
    const arc = sv('path', { class: 'pk-arc', fill: 'none', 'stroke-width': 4, 'stroke-linecap': 'round' });
    const needle = sv('line', { class: 'pk-needle', 'stroke-width': 3, 'stroke-linecap': 'round' });
    const defDot = sv('circle', { class: 'pk-def', r: 1.6 });
    const svg = sv('svg', { viewBox: '0 0 64 64', width: size, height: size, 'aria-hidden': 'true', focusable: 'false' },
      track, arc, sv('circle', { class: 'pk-cap', cx: 32, cy: 32, r: 16 }), needle, defDot);
    const A0 = -135, A1 = 135;
    track.setAttribute('d', arcPath(32, 32, 24, A0, A1));
    const dial = h('div', { class: 'pk-dial', role: 'slider', tabindex: '0', 'aria-label': o.label, 'aria-valuemin': o.min, 'aria-valuemax': o.max }, svg);
    const val = h('button', { type: 'button', class: 'pk-val mono', title: 'Click to type a value', 'aria-label': o.label + ' value, type a number' });
    const wrap = h('div', { class: 'pk' + (o.small ? ' small' : ''), title: o.help || o.label, 'data-k': o.key }, h('span', { class: 'pk-lab' }, o.label), dial, val);
    const c = core(o, {
      render(v) {
        const f = c.toF(v), ang = A0 + (A1 - A0) * f;
        const from = bipolar ? A0 + (A1 - A0) * c.toF(0) : A0;
        arc.setAttribute('d', arcPath(32, 32, 24, Math.min(from, ang), Math.max(from, ang)));
        const [x0, y0] = arcPt(32, 32, 6, ang), [x1, y1] = arcPt(32, 32, 15, ang);
        needle.setAttribute('x1', x0.toFixed(2)); needle.setAttribute('y1', y0.toFixed(2)); needle.setAttribute('x2', x1.toFixed(2)); needle.setAttribute('y2', y1.toFixed(2));
        const text = fmtValue(v, o);
        setText(val, text);
        dial.setAttribute('aria-valuenow', v); dial.setAttribute('aria-valuetext', text);
      },
      entry: () => c.entry(val, o.label),
    });
    if (o.def != null) { const [dx, dy] = arcPt(32, 32, 24, A0 + (A1 - A0) * c.toF(o.def)); defDot.setAttribute('cx', dx.toFixed(2)); defDot.setAttribute('cy', dy.toFixed(2)); }
    else defDot.remove();
    // vertical drag: 160 px for the whole range, 1600 with Shift
    let last = 0, f0 = 0;
    dial.addEventListener('pointerdown', e => {
      if (e.button !== 0) return;
      dial.setPointerCapture(e.pointerId); dial.focus();
      c.st.dragging = true; last = e.clientY; f0 = c.toF(c.get()); e.preventDefault();
    });
    dial.addEventListener('pointermove', e => {
      if (!c.st.dragging) return;
      f0 = clamp(f0 + (last - e.clientY) / (e.shiftKey ? 1600 : 160), 0, 1); last = e.clientY;
      c.user(c.fromF(f0, e.shiftKey), false);
    });
    const up = e => { if (!c.st.dragging) return; c.st.dragging = false; try { dial.releasePointerCapture(e.pointerId); } catch { } c.user(c.get(), true); };
    dial.addEventListener('pointerup', up); dial.addEventListener('pointercancel', up);
    dial.addEventListener('dblclick', () => c.reset());
    dial.addEventListener('keydown', c.keys);
    val.addEventListener('click', () => c.entry(val, o.label));
    c.set0 = v => { c.st.dragging = false; c.user(v, false); };
    const w = { el: wrap, set: v => c.set(v), busy: () => c.busy(), get: () => c.get(), focusEl: dial };
    // the loudness card reads and writes `.value` of whatever carries data-k
    Object.defineProperty(wrap, 'value', { get: () => String(c.get()), set: x => { const n = parseFloat(x); if (isFinite(n)) { c.st.dragging = false; c.user(n, false); } } });
    c.user(c.get(), false);
    return w;
  }

  // ---- a vertical fader ----
  function fader(opts) {
    const o = { ...opts };
    const H = o.height || 84;
    const fill = h('i', { class: 'pf-fill' });
    const cap = h('b', { class: 'pf-cap' });
    const track = h('div', { class: 'pf-track', style: `height:${H}px` }, fill, cap);
    const dial = h('div', { class: 'pf-dial', role: 'slider', tabindex: '0', 'aria-orientation': 'vertical', 'aria-label': o.label, 'aria-valuemin': o.min, 'aria-valuemax': o.max }, track);
    const val = h('button', { type: 'button', class: 'pk-val mono', title: 'Click to type a value', 'aria-label': o.label + ' value, type a number' });
    // `unitInLabel`: the unit under the name instead of after the number, so a narrow strip of faders stays legible
    const inLabel = o.unitInLabel && o.unit && shortUnit(o.unit) !== ':1';
    const wrap = h('div', { class: 'pf', title: o.help || o.label, 'data-k': o.key }, h('span', { class: 'pk-lab' }, inLabel ? h('span', {}, o.label, h('br'), h('span', { class: 'pk-u' }, shortUnit(o.unit))) : o.label), dial, val);
    const bipolar = o.min < 0 && o.max > 0 && !o.log;
    const c = core(o, {
      render(v) {
        const f = c.toF(v);
        cap.style.bottom = `calc(${(f * 100).toFixed(2)}% - 5px)`;
        if (bipolar) { const z = c.toF(0); fill.style.bottom = Math.min(z, f) * 100 + '%'; fill.style.height = Math.abs(f - z) * 100 + '%'; }
        else { fill.style.bottom = '0'; fill.style.height = (f * 100).toFixed(2) + '%'; }
        const text = fmtValue(v, o);
        setText(val, inLabel ? fmtValue(v, o, false) : text);
        dial.setAttribute('aria-valuenow', v); dial.setAttribute('aria-valuetext', text);
      },
      entry: () => c.entry(val, o.label),
    });
    const fromY = (e, rel) => { const r = track.getBoundingClientRect(); return clamp(1 - (e.clientY - r.top) / r.height, 0, 1); };
    let last = 0, f0 = 0;
    dial.addEventListener('pointerdown', e => {
      if (e.button !== 0) return;
      dial.setPointerCapture(e.pointerId); dial.focus(); c.st.dragging = true; last = e.clientY;
      f0 = e.shiftKey ? c.toF(c.get()) : fromY(e);
      c.user(c.fromF(f0), false); e.preventDefault();
    });
    dial.addEventListener('pointermove', e => {
      if (!c.st.dragging) return;
      const dy = (last - e.clientY) / (track.getBoundingClientRect().height || H);
      f0 = clamp(f0 + dy / (e.shiftKey ? 10 : 1), 0, 1); last = e.clientY;
      c.user(c.fromF(f0), false);
    });
    const up = e => { if (!c.st.dragging) return; c.st.dragging = false; try { dial.releasePointerCapture(e.pointerId); } catch { } c.user(c.get(), true); };
    dial.addEventListener('pointerup', up); dial.addEventListener('pointercancel', up);
    dial.addEventListener('dblclick', () => c.reset());
    dial.addEventListener('keydown', c.keys);
    val.addEventListener('click', () => c.entry(val, o.label));
    c.user(c.get(), false);
    const w = { el: wrap, set: v => c.set(v), busy: () => c.busy(), get: () => c.get(), focusEl: dial };
    Object.defineProperty(wrap, 'value', { get: () => String(c.get()), set: x => { const n = parseFloat(x); if (isFinite(n)) c.user(n, false); } });
    return w;
  }

  // ---- a row of exclusive buttons: options [{ value, label }] ----
  function segmented(opts) {
    const o = { ...opts };
    let cur = o.value;
    const btns = o.options.map(op => h('button', { type: 'button', role: 'radio', class: 'seg', 'data-v': op.value, title: op.title || null, onclick: () => pick(op.value) }, op.label));
    const grp = h('div', { class: 'segs', role: 'radiogroup', 'aria-label': o.label }, btns);
    // `slot`: laid out like a knob (label, the control in the dial's place, an empty value line), so a row of mixed
    // controls shares its label and value lines; `vertical`: the buttons stacked (the multiband's band count)
    const wrap = o.slot || o.vertical
      ? h('div', { class: 'pseg slot' + (o.vertical ? ' vert' : ''), title: o.help || o.label, 'data-k': o.key }, h('span', { class: 'pk-lab' }, o.label), h('div', { class: 'pseg-d' }, grp), o.vertical ? null : h('span', { class: 'pk-val pk-ph', 'aria-hidden': 'true' }, '\u00a0'))
      : h('div', { class: 'pseg', title: o.help || o.label, 'data-k': o.key }, h('span', { class: 'pk-lab' }, o.label), grp);
    const paint = () => btns.forEach((b, i) => { const on = String(o.options[i].value) === String(cur); b.classList.toggle('on', on); b.setAttribute('aria-checked', on); b.tabIndex = on || (cur == null && i === 0) ? 0 : -1; });
    const pick = v => { if (String(v) === String(cur)) return; cur = v; paint(); o.onChange?.(v, true); };
    grp.addEventListener('keydown', e => {
      const i = o.options.findIndex(x => String(x.value) === String(cur));
      const d = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
      if (!d) return;
      e.preventDefault();
      const n = clamp(i + d, 0, o.options.length - 1);
      pick(o.options[n].value); btns[n].focus();
    });
    paint();
    return { el: wrap, set: v => { cur = v; paint(); }, busy: () => false, get: () => cur, focusEl: btns[0] };
  }

  // ---- an on/off button with a lamp ----
  function toggle(opts) {
    const o = { ...opts };
    let on = !!o.value;
    const b = h('button', { type: 'button', class: 'ptog', role: 'switch', 'data-k': o.key, title: o.help || o.label, onclick: () => { on = !on; paint(); o.onChange?.(on, true); } }, h('i'), h('span', {}, o.label));
    const paint = () => { b.classList.toggle('on', on); b.setAttribute('aria-checked', on); };
    paint();
    return { el: b, set: v => { on = !!v; paint(); }, busy: () => false, get: () => on, focusEl: b };
  }

  // ---- meters ----
  // A vertical level bar, -60 to 0 dB, growing up; `set(null)` empties it. The element keeps the class `plevel`.
  // `mark`: a reference line across the bar at that level (the loudness target).
  function level(label, mark) {
    const fill = h('i');
    const pv = h('span', { class: 'mono pv' }, '--');
    const el = h('div', { class: 'plevel vm', title: label }, h('div', { class: 'bar', role: 'meter', 'aria-label': label + ' level, dB', 'aria-valuemin': -60, 'aria-valuemax': 0 }, fill,
      mark != null ? h('b', { class: 'mark', style: `bottom:${(clamp((mark + 60) / 60, 0, 1) * 100).toFixed(1)}%` }) : null), pv, h('span', { class: 'dim ml' }, label));
    const bar = el.querySelector('.bar');
    const markEl = bar.querySelector('.mark');
    return { el, setMark(db) { if (markEl && db != null) markEl.style.bottom = (clamp((db + 60) / 60, 0, 1) * 100).toFixed(1) + '%'; }, set(db) { fill.style.height = (db == null ? 0 : clamp((db + 60) / 60, 0, 1) * 100) + '%'; setText(pv, db == null ? '--' : db.toFixed(1)); if (db != null) bar.setAttribute('aria-valuenow', db); else bar.removeAttribute('aria-valuenow'); } };
  }
  // A gain-reduction bar hanging from the top, 0 to -`range` dB (Optimod style). `signed` draws the AGC's gain around
  // the middle: a boost grows up from the centre line, a cut hangs down from it.
  function gr(label, range = 18, signed = false) {
    const fill = h('i');
    const pv = h('span', { class: 'mono pv' }, '--');
    const el = h('div', { class: 'plevel gr hang' + (signed ? ' signed' : ''), title: label }, h('div', { class: 'bar', role: 'meter', 'aria-label': label + ', dB', 'aria-valuemin': signed ? -range : -range, 'aria-valuemax': signed ? range : 0 }, fill, signed ? h('b', { class: 'mid' }) : null), pv, h('span', { class: 'dim ml' }, label));
    const bar = el.querySelector('.bar');
    // `heading(db)`: a tick across the bar where the gain is heading (the signed bar only); null hides it.
    const tick = signed ? h('b', { class: 'tick', style: 'display:none' }) : null;
    if (tick) bar.append(tick);
    return {
      el, heading(db) { if (!tick) return; if (db == null) { tick.style.display = 'none'; return; } tick.style.display = ''; tick.style.top = (50 - clamp(db, -range, range) / range * 50) + '%'; },
      set(db) {
        if (db == null) { fill.style.height = '0'; fill.style.top = '0'; fill.style.bottom = ''; setText(pv, '--'); bar.removeAttribute('aria-valuenow'); return; }
        if (signed) {
          const r = Math.min(range, Math.abs(db)) / range * 50;
          if (db >= 0) { fill.style.top = (50 - r) + '%'; } else { fill.style.top = '50%'; }
          fill.style.height = r + '%';
        } else { fill.style.top = '0'; fill.style.height = Math.min(range, Math.abs(db)) / range * 100 + '%'; }
        if (Math.abs(db) < 0.05) db = 0;   // no "-0.0"
        setText(pv, (signed && db > 0 ? '+' : '') + db.toFixed(1));
        bar.setAttribute('aria-valuenow', db);
      },
    };
  }
  // A small horizontal readout for a card header: a thin bar and a number.
  function mini(label, kind) {
    const fill = h('i');
    const pv = h('span', { class: 'mono pv' }, '--');
    const el = h('div', { class: 'pmini ' + kind, title: label }, h('span', { class: 'dim' }, label), h('div', { class: 'bar' }, fill), pv);
    return {
      el, set(db) {
        if (db == null) { fill.style.width = '0'; setText(pv, '--'); return; }
        if (kind === 'gr') fill.style.width = Math.min(18, Math.abs(db)) / 18 * 100 + '%'; else fill.style.width = clamp((db + 60) / 60, 0, 1) * 100 + '%';
        setText(pv, db.toFixed(1));
      },
    };
  }

  // ---- a scrolling history: the last `seconds` of a block's meters, newest at the right ----
  // kind 'gain': a signed gain (the AGC), drawn around a centre line, +-`range` dB.
  // kind 'comp': the input level filled from the bottom (-60 to 0 dB) and the gain reduction hanging from the top
  // (0 to -`range` dB, the scale on the right), the way a compressor plug-in shows what it is doing.
  // push({ in, out, gr }) with nulls for "no reading" (the block is bypassed); it takes the size of its box.
  function history(opts) {
    const o = { seconds: 20, range: 12, ...opts };
    const cv = h('canvas', { class: 'phist-cv', role: 'img', 'aria-label': o.label });
    const el = h('div', { class: 'phist pelastic', title: o.help || o.label }, cv, h('span', { class: 'phist-lab' }, o.label));
    const buf = [];
    let raf = 0;
    const draw = () => {
      raf = 0;
      const dpr = window.devicePixelRatio || 1, w = el.clientWidth, hh = el.clientHeight;
      if (w < 10 || hh < 10) return;
      if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(hh * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(hh * dpr); }
      const c = cv.getContext('2d');
      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.clearRect(0, 0, w, hh);
      const now = performance.now(), span = o.seconds * 1000;
      const stripH = buf.some(b => b.strip) ? 5 : 0;   // an optional state strip along the bottom: a colour a sample
      const X = t => w - (now - t) / span * w;
      c.lineWidth = 1; c.strokeStyle = '#242c3a';
      const gl = y => { c.beginPath(); c.moveTo(0, Math.round(y) + .5); c.lineTo(w, Math.round(y) + .5); c.stroke(); };
      c.font = '10px ' + getComputedStyle(el).fontFamily; c.fillStyle = '#97a1b2';
      if (o.kind === 'gain') {
        const Y = d => hh / 2 - clamp(d, -o.range, o.range) / o.range * (hh / 2 - 4);
        for (const d of [-o.range, -o.range / 2, o.range / 2, o.range]) gl(Y(d));
        c.strokeStyle = '#4a566c'; gl(Y(0));
        c.fillText('+' + o.range, 3, Y(o.range) + 10); c.fillText('0', 3, Y(0) - 3); c.fillText('-' + o.range, 3, Y(-o.range) - 3);
        if (stripH) { let x0 = null, col = null; const flushStrip = x1 => { if (col) { c.fillStyle = col; c.fillRect(x0, hh - stripH, Math.max(1, x1 - x0), stripH); } }; for (const s of buf) { const x = X(s.t); if (s.strip !== col) { flushStrip(x); col = s.strip; x0 = x; } } if (buf.length) flushStrip(X(buf[buf.length - 1].t) + 1); }
        c.strokeStyle = '#f5a623'; c.lineWidth = 2; c.beginPath();
        let on = false;
        for (const s of buf) { if (s.gr == null) { on = false; continue; } const x = X(s.t), y = Y(s.gr); on ? c.lineTo(x, y) : c.moveTo(x, y); on = true; }
        c.stroke();
      } else {
        const YL = d => hh - clamp((d + 60) / 60, 0, 1) * hh;
        for (const d of [-12, -24, -36, -48]) gl(YL(d));
        c.fillStyle = '#3a4457';
        c.beginPath(); let on = false, x0 = 0, xl = 0;
        const close = () => { if (on) { c.lineTo(xl, hh); c.lineTo(x0, hh); c.closePath(); } on = false; };
        for (const s of buf) { if (s.in == null) { close(); continue; } const x = X(s.t), y = YL(s.in); if (!on) { c.moveTo(x, hh); x0 = x; c.lineTo(x, y); } else c.lineTo(x, y); xl = x; on = true; }
        close(); c.fill();
        c.strokeStyle = '#3ecf8e'; c.lineWidth = 1.5; c.beginPath(); on = false;
        for (const s of buf) { if (s.out == null) { on = false; continue; } const x = X(s.t), y = YL(s.out); on ? c.lineTo(x, y) : c.moveTo(x, y); on = true; }
        c.stroke();
        const YG = d => clamp(Math.abs(d), 0, o.range) / o.range * hh;
        c.fillStyle = '#f5a62399'; c.strokeStyle = '#f5a623'; c.lineWidth = 1.5;
        let seg = [];
        const flush = () => { if (seg.length) { c.beginPath(); c.moveTo(seg[0][0], 0); for (const [x, y] of seg) c.lineTo(x, y); c.lineTo(seg[seg.length - 1][0], 0); c.closePath(); c.fill(); c.beginPath(); seg.forEach(([x, y], i) => i ? c.lineTo(x, y) : c.moveTo(x, y)); c.stroke(); } seg = []; };
        for (const s of buf) { if (s.gr == null) { flush(); continue; } seg.push([X(s.t), YG(s.gr)]); }
        flush();
        c.fillStyle = '#97a1b2'; c.textAlign = 'right'; c.fillText('GR -' + o.range, w - 3, hh - 4); c.textAlign = 'left'; c.fillText('0 dB', 3, 11);
      }
    };
    const schedule = () => { if (!raf) raf = requestAnimationFrame(draw); };
    new ResizeObserver(schedule).observe(el);
    return {
      el, push(s) {
        const t = performance.now();
        buf.push({ t, ...s });
        while (buf.length && t - buf[0].t > (o.keep ?? o.seconds) * 1000 + 500) buf.shift();
        schedule();
      },
      // The span shown, seconds (the buffer keeps the longest the card has asked for).
      setSeconds(sec) { o.seconds = sec; schedule(); },
    };
  }

  // ---- a number you can scrub: a readout that takes a drag, the arrows or a typed value ----
  // For the equalizer's band strip, where a knob per number would not fit. It shares everything with the knob (`core`):
  // drag up or down (160 px for the range, Shift: 1600 and exact to the resolution), arrows (Shift: x10,
  // Alt: one resolution step), click or Enter to type an exact value, double-click or Backspace to reset.
  // opts: spec() fields plus { value, onChange, key, short }
  function scrub(opts) {
    const o = { ...opts };
    const val = h('div', { class: 'pk-val mono eq-sv', role: 'slider', tabindex: '0', 'aria-label': o.label, 'aria-valuemin': o.min, 'aria-valuemax': o.max, title: (o.help || o.label) + ' Drag, use the arrow keys, or click to type a value.' });
    const wrap = h('div', { class: 'eq-f', 'data-k': o.key }, h('span', { class: 'eq-fl' }, o.short || o.label), val);
    let off = false;
    const c = core(o, {
      render(v) {
        setText(val, off ? '--' : fmtValue(v, o, false));
        val.setAttribute('aria-valuenow', v); val.setAttribute('aria-valuetext', off ? 'not used by this type' : fmtValue(v, o));
      },
      entry: () => { if (!off) c.entry(val, o.label); },
    });
    let last = 0, f0 = 0, down = false, moved = false;
    val.addEventListener('pointerdown', e => {
      if (off || e.button !== 0 || val.tagName === 'INPUT') return;
      val.setPointerCapture(e.pointerId); val.focus();
      down = true; moved = false; last = e.clientY; f0 = c.toF(c.get()); e.preventDefault();
    });
    val.addEventListener('pointermove', e => {
      if (!down) return;
      if (!moved && Math.abs(e.clientY - last) < 3) return;
      moved = true; c.st.dragging = true;
      f0 = clamp(f0 + (last - e.clientY) / (e.shiftKey ? 1600 : 160), 0, 1); last = e.clientY;
      c.user(c.fromF(f0, e.shiftKey), false);
    });
    const up = e => {
      if (!down) return; down = false;
      try { val.releasePointerCapture(e.pointerId); } catch { }
      if (moved) { c.st.dragging = false; c.user(c.get(), true); } else c.entry(val, o.label);
    };
    val.addEventListener('pointerup', up); val.addEventListener('pointercancel', up);
    val.addEventListener('dblclick', () => { if (!off) c.reset(); });
    val.addEventListener('keydown', e => { if (!off) c.keys(e); });
    c.paint();   // show the value; never send it (a stored 0.7071 Q must not become 0.71 because a band was drawn)
    // `disable`: the number does not apply (a low-pass has no gain): shown as a dash, and not editable
    const w = { el: wrap, set: v => c.set(v), busy: () => c.busy(), get: () => c.get(), focusEl: val, disable(flag) {
      off = !!flag;
      wrap.classList.toggle('off', off); val.tabIndex = off ? -1 : 0; val.setAttribute('aria-disabled', off ? 'true' : 'false');
      setText(val, off ? '--' : fmtValue(c.get(), o, false));
    } };
    Object.defineProperty(wrap, 'value', { get: () => String(c.get()), set: x => { const n = parseFloat(x); if (isFinite(n)) { c.user(n, true); } } });
    return w;
  }

  // ---- the equalizer's curve ----
  // The response of the enabled bands (EqMath: the engine's RBJ formulas at 48 kHz) and one point per enabled band, in
  // the band's colour and number. Drag a point: frequency (1 Hz steps) and gain (0.1 dB); Shift drags a tenth as far,
  // for the last digit. The arrows move it (Shift: x10, Alt: one
  // resolution step); a double-click on a point resets its gain and Q; a double-click on empty plot adds a band there.
  // opts: { get: () => the block's values, params: { key: schema param }, onChange(patch, final), onAdd(hz, db),
  // corner: an element laid over the top right of the plot }. The plot takes the size of its box (the faceplate gives it
  // the room the controls leave), so the grid and the curve are redrawn at that size and nothing is stretched.
  function eqCurve(opts) {
    let W = 560, H = 170;
    const L = 30, R = 8, T = 8, B = 18, F0 = 20, F1 = 20000, DB = 18;
    const px = f => L + (Math.log10(f / F0) / 3) * (W - L - R);
    const py = d => T + (1 - (d + DB) / (2 * DB)) * (H - T - B);
    const fx = x => F0 * Math.pow(10, 3 * clamp((x - L) / (W - L - R), 0, 1));
    const dy = y => clamp((1 - (y - T) / (H - T - B)) * 2 * DB - DB, -DB, DB);
    // ---- the analyzer: a live spectrum behind the curve (frames come from the page: feed(bytes)) ----
    // 120 bands, 1/12 octave from 20 Hz, one byte each: (dBFS + 120) * 2. Its own scale, A_LO to A_HI dBFS over the height of the
    // plot; a faint line of the highest level per band that falls 6 dB a second (or not at all while `hold` is on).
    const A_LO = -90, A_HI = -10, NB = 120, A_STALE_MS = 1500;
    const ay = d => T + (1 - (clamp(d, A_LO, A_HI) - A_LO) / (A_HI - A_LO)) * (H - T - B);
    const aHz = i => F0 * Math.pow(2, (i + 0.5) / 12);
    const store = (k, v) => { try { localStorage.setItem('eqAnalyzer.' + k, v); } catch { } };
    const load = (k, d) => { try { return localStorage.getItem('eqAnalyzer.' + k) ?? d; } catch { return d; } };
    let tap = ['off', 'in', 'out'].includes(load('tap', 'off')) ? load('tap', 'off') : 'off';
    let hold = load('hold', '0') === '1';
    let lv = null, pk = null, lastAt = 0, raf = 0;
    const anaG = sv('g', { class: 'eq-ana-g', 'data-eq-ana': '' });
    const aFill = sv('path', { class: 'eq-a-fill' }), aLine = sv('path', { class: 'eq-a-line', fill: 'none' }), aPeak = sv('path', { class: 'eq-a-peak', fill: 'none' }), aLab = sv('g', { class: 'eq-a-lab' });
    anaG.append(aFill, aPeak, aLine, aLab);
    const fresh = () => tap !== 'off' && lv && performance.now() - lastAt < A_STALE_MS;
    const drawAna = () => {
      raf = 0;
      if (!fresh()) { aFill.setAttribute('d', ''); aLine.setAttribute('d', ''); aPeak.setAttribute('d', ''); aLab.replaceChildren(); return; }
      const pts = a => { let d = ''; for (let i = 0; i < NB; i++) d += (i ? 'L' : 'M') + px(aHz(i)).toFixed(1) + ' ' + ay(a[i]).toFixed(1); return d; };
      const line = pts(lv);
      aLine.setAttribute('d', line);
      aFill.setAttribute('d', line + `L${px(aHz(NB - 1)).toFixed(1)} ${H - B}L${px(aHz(0)).toFixed(1)} ${H - B}Z`);
      aPeak.setAttribute('d', pk ? pts(pk) : '');
      aLab.replaceChildren();
      for (const d of [-80, -60, -40, -20]) aLab.append(Object.assign(sv('text', { x: W - R - 3, y: ay(d) - 2, 'text-anchor': 'end' }), { textContent: d === -80 ? d + ' dBFS' : String(d) }));
    };
    const redrawAna = () => { if (!raf) raf = requestAnimationFrame(drawAna); };
    const resetPeaks = () => { pk = lv ? Float32Array.from(lv) : null; };
    const feed = u8 => {
      if (tap === 'off' || u8.length < 6 + NB || u8[4] !== (tap === 'in' ? 0 : 1) || u8[5] !== NB) return;
      const now = performance.now(), dt = lastAt ? Math.min(1, (now - lastAt) / 1000) : 0;
      lv = lv || new Float32Array(NB);
      for (let i = 0; i < NB; i++) lv[i] = u8[6 + i] / 2 - 120;
      if (!pk) resetPeaks(); else for (let i = 0; i < NB; i++) pk[i] = Math.max(lv[i], hold ? pk[i] : pk[i] - 6 * dt);
      lastAt = now; redrawAna();
    };
    const staleTimer = setInterval(() => { if (!wrap.isConnected) { clearInterval(staleTimer); return; } if (lv && !fresh()) redrawAna(); }, 500);
    const setTap = t => {
      tap = t; store('tap', t); lv = null; pk = null; lastAt = 0; redrawAna();
      holdBtn.disabled = t === 'off'; opts.onAnalyzer?.(t);
    };
    const setHold = on => { hold = on; store('hold', on ? '1' : '0'); holdBtn.classList.toggle('on', on); holdBtn.setAttribute('aria-checked', on); if (on) resetPeaks(); };
    const holdBtn = h('button', { type: 'button', class: 'seg eq-hold', role: 'switch', 'aria-checked': hold, title: 'Peak hold: the thin line keeps the highest level of each band and does not fall. Switch it on to start from the levels now; switch it off and the line falls again, 6 dB a second.', onclick: () => setHold(!hold) }, 'hold');
    holdBtn.classList.toggle('on', hold);
    holdBtn.setAttribute('aria-checked', hold);
    const anaCtl = segmented({ label: 'Analyzer', value: tap, help: 'A live spectrum behind the curve. In: the sound entering the equalizer. Out: the sound leaving it. It costs nothing while it is off.', options: [{ value: 'off', label: 'off' }, { value: 'in', label: 'in', title: 'The sound entering the equalizer' }, { value: 'out', label: 'out', title: 'The sound leaving the equalizer' }], onChange: v => setTap(v) });
    const anaBox = h('div', { class: 'eq-ana' }, anaCtl.el, holdBtn);
    holdBtn.disabled = tap === 'off';
    // ---- the readout under the pointer: frequency, and the analyzer level there ----
    const guide = sv('line', { class: 'eq-guide', y1: T, y2: H - B, style: 'display:none' });
    const read = h('div', { class: 'eq-read mono', 'data-eq-read': '', style: 'display:none' });
    const levelAt = (arr, hz) => {
      const t = 12 * Math.log2(hz / F0) - 0.5, i0 = clamp(Math.floor(t), 0, NB - 1), i1 = clamp(i0 + 1, 0, NB - 1), f = clamp(t - i0, 0, 1);
      return arr[i0] * (1 - f) + arr[i1] * f;
    };
    const hideRead = () => { read.style.display = 'none'; guide.style.display = 'none'; };
    const hover = e => {
      const r = svg.getBoundingClientRect(), x = (e.clientX - r.left) * W / r.width, y = (e.clientY - r.top) * H / r.height;
      if (x < L || x > W - R || y < T || y > H - B) { hideRead(); return; }
      const hz = fx(x);
      let txt = `${Math.round(hz)} Hz`;
      if (fresh()) { txt += `  ${levelAt(lv, hz).toFixed(1)} dBFS`; if (pk) txt += `  (peak ${levelAt(pk, hz).toFixed(1)})`; }
      setText(read, txt);
      read.style.display = ''; guide.style.display = '';
      guide.setAttribute('x1', x); guide.setAttribute('x2', x);
      const rw = read.offsetWidth || 120, left = (x / W) * wrap.clientWidth;
      read.style.left = clamp(left + 10, 4, Math.max(4, wrap.clientWidth - rw - 4)) + 'px';
      read.style.top = clamp((y / H) * wrap.clientHeight - 26, 28, Math.max(28, wrap.clientHeight - 44)) + 'px';
    };
    const grid = sv('g', { class: 'eq-grid' });
    const drawGrid = () => {
      grid.replaceChildren();
      for (const hz of [50, 100, 200, 500, 1000, 2000, 5000, 10000]) {
        grid.append(sv('line', { x1: px(hz), x2: px(hz), y1: T, y2: H - B }), Object.assign(sv('text', { x: px(hz) + 2, y: H - 5 }), { textContent: hz >= 1000 ? hz / 1000 + 'k' : String(hz) }));
      }
      for (const d of [-12, -6, 0, 6, 12]) grid.append(sv('line', { class: d === 0 ? 'zero' : '', x1: L, x2: W - R, y1: py(d), y2: py(d) }), Object.assign(sv('text', { x: 2, y: py(d) + 3 }), { textContent: (d > 0 ? '+' : '') + d }));
    };
    drawGrid();
    const path = sv('path', { class: 'eq-line', fill: 'none' });
    const handles = Array.from({ length: EqMath.MAX_BANDS }, (_, i) => {
      const g = sv('g', { class: `eq-h eq-c${i + 1}`, tabindex: 0, role: 'slider', 'data-band': i + 1 });
      g.append(sv('circle', { r: 8 }), Object.assign(sv('text', { y: 3.5, 'text-anchor': 'middle' }), { textContent: String(i + 1) }));
      return g;
    });
    const hg = sv('g', {}, handles);
    const bg = sv('rect', { class: 'eq-bg', x: 0, y: 0, width: W, height: H, fill: 'transparent' });
    const svg = sv('svg', { class: 'eq-svg', viewBox: `0 0 ${W} ${H}`, role: 'group', 'aria-label': 'Equalizer curve: one point for each band that is on' }, bg, anaG, grid, guide, path, hg);
    svg.addEventListener('pointermove', hover); svg.addEventListener('pointerdown', hover); svg.addEventListener('pointerleave', hideRead);
    const wrap = h('div', { class: 'eq-wrap pelastic' }, svg, opts.corner ? h('div', { class: 'eq-corner' }, opts.corner) : null, anaBox, read);
    const ro = new ResizeObserver(() => {
      const w = Math.round(wrap.clientWidth), hh = Math.round(wrap.clientHeight);
      if (w < 40 || hh < 40 || (w === W && hh === H)) return;
      W = w; H = hh; svg.setAttribute('viewBox', `0 0 ${W} ${H}`); bg.setAttribute('width', W); bg.setAttribute('height', H); guide.setAttribute('y2', H - B); drawGrid(); paint(); redrawAna();
    });
    ro.observe(wrap);
    const P = k => opts.params[k];
    const key = (n, s) => `b${n}_${s}`;
    const paint = () => {
      const v = opts.get();
      const bands = EqMath.bandsOf(v, true);
      let d = '';
      const resp = EqMath.responseDb(v, Array.from({ length: 161 }, (_, i) => F0 * Math.pow(10, 3 * i / 160)));
      resp.forEach((m, i) => { const f = F0 * Math.pow(10, 3 * i / 160); d += (i ? 'L' : 'M') + px(f).toFixed(1) + ' ' + py(clamp(m, -DB, DB)).toFixed(1); });
      path.setAttribute('d', d);
      handles.forEach((g, i) => {
        const b = bands[i];
        g.style.display = b && b.on ? '' : 'none';
        if (!b || !b.on) return;
        const gain = EqMath.hasGain(b.type);
        g.setAttribute('transform', `translate(${px(b.hz).toFixed(1)} ${py(gain ? b.db : 0).toFixed(1)})`);
        const t = `Band ${b.n}, ${(EqMath.TYPES.find(x => x[0] === b.type) || [])[1] || b.type}: ${b.hz} Hz` + (gain ? `, ${b.db > 0 ? '+' : ''}${b.db} dB` : '') + `, Q ${b.q}`;
        g.setAttribute('aria-label', t + '. Arrow keys move frequency and gain.');
        g.setAttribute('aria-valuetext', t); g.setAttribute('aria-valuenow', gain ? b.db : 0); g.setAttribute('aria-valuemin', -DB); g.setAttribute('aria-valuemax', DB);
        g.querySelector('title')?.remove(); g.append(Object.assign(sv('title'), { textContent: t }));
      });
    };
    let drag = null, tm = null, busyUntil = 0;
    const local = {};
    const origGet = opts.get;
    opts = { ...opts, get: () => ({ ...origGet(), ...local }) };
    const push = final => {
      if (final) { clearTimeout(tm); tm = null; const patch = { ...local }; if (Object.keys(patch).length) opts.onChange(patch, true); for (const k of Object.keys(local)) delete local[k]; }
      else if (!tm) tm = setTimeout(() => { tm = null; opts.onChange({ ...local }, false); }, 100);
      busyUntil = Date.now() + 400;
    };
    const r1 = x => Math.round(x * 10) / 10;
    const setPt = (n, hz, db, final) => {
      const ph = P(key(n, 'hz')), pd = P(key(n, 'db'));
      local[key(n, 'hz')] = clamp(Math.round(hz), ph.min, ph.max);
      if (EqMath.hasGain(opts.get()[key(n, 'type')])) local[key(n, 'db')] = clamp(r1(db), pd.min, pd.max);
      paint(); push(final);
    };
    const setQ = (n, q, final) => {
      const pq = P(key(n, 'q'));
      local[key(n, 'q')] = clamp(Math.round(q * 100) / 100, pq.min, pq.max);
      paint(); push(final);
    };
    handles.forEach((g, i) => {
      const n = i + 1;
      let o0 = null;
      g.addEventListener('pointerdown', e => {
        if (e.button !== 0) return;
        g.setPointerCapture(e.pointerId); g.focus(); drag = n; e.preventDefault();
        const v = opts.get(), gain = EqMath.hasGain(v[key(n, 'type')]);
        o0 = { x: e.clientX, y: e.clientY, X: px(v[key(n, 'hz')]), Y: py(gain ? v[key(n, 'db')] : 0) };
      });
      g.addEventListener('pointermove', e => {
        if (drag !== n) return;
        const r = svg.getBoundingClientRect(), sx = W / r.width, sy = H / r.height;
        // Shift: a tenth as far as the pointer moved, from where the point was when Shift went down
        if (e.shiftKey && !o0.fine) { o0 = { ...o0, x: e.clientX, y: e.clientY, X: px(opts.get()[key(n, 'hz')]), Y: py(EqMath.hasGain(opts.get()[key(n, 'type')]) ? opts.get()[key(n, 'db')] : 0), fine: true }; }
        if (!e.shiftKey && o0.fine) { o0 = { ...o0, x: e.clientX, y: e.clientY, X: px(opts.get()[key(n, 'hz')]), Y: py(EqMath.hasGain(opts.get()[key(n, 'type')]) ? opts.get()[key(n, 'db')] : 0), fine: false }; }
        const k = e.shiftKey ? 0.1 : 1;
        setPt(n, fx(o0.X + (e.clientX - o0.x) * sx * k), dy(o0.Y + (e.clientY - o0.y) * sy * k), false);
      });
      const up = e => { if (drag !== n) return; drag = null; o0 = null; try { g.releasePointerCapture(e.pointerId); } catch { } push(true); };
      g.addEventListener('pointerup', up); g.addEventListener('pointercancel', up);
      g.addEventListener('dblclick', e => {
        e.stopPropagation();
        const t = opts.get()[key(n, 'type')];
        if (EqMath.hasGain(t)) local[key(n, 'db')] = 0;
        local[key(n, 'q')] = EqMath.defaultQ(t);
        paint(); push(true);
      });
      g.addEventListener('keydown', e => {
        const v = opts.get(), big = e.shiftKey ? 10 : 1;
        let hz = v[key(n, 'hz')], db = v[key(n, 'db')], hit = true;
        const fstep = e.altKey ? 1 : Math.max(1, Math.round(hz * 0.01)) * big;
        if (e.key === 'ArrowRight') hz += fstep; else if (e.key === 'ArrowLeft') hz -= fstep;
        else if (e.key === 'ArrowUp') db += (e.altKey ? 0.1 : 0.5) * big; else if (e.key === 'ArrowDown') db -= (e.altKey ? 0.1 : 0.5) * big;
        else if (e.key === 'PageUp' || e.key === 'PageDown') { setQ(n, v[key(n, 'q')] * (e.key === 'PageUp' ? 1.05 : 1 / 1.05), false); e.preventDefault(); clearTimeout(g._k); g._k = setTimeout(() => push(true), 250); return; }
        else hit = false;
        if (!hit) return;
        e.preventDefault(); setPt(n, hz, db, false); clearTimeout(g._k); g._k = setTimeout(() => push(true), 250);
      });
    });
    // a double-click on empty plot adds a band there
    bg.addEventListener('dblclick', e => {
      if (!opts.onAdd) return;
      const r = svg.getBoundingClientRect();
      opts.onAdd(Math.round(fx((e.clientX - r.left) * W / r.width)), r1(dy((e.clientY - r.top) * H / r.height)));
    });
    paint();
    if (tap !== 'off') opts.onAnalyzer?.(tap);
    wrap._ana = () => ({ tap, hold, n: lv ? lv.length : 0, max: lv ? Math.max(...lv) : null, fresh: !!fresh() });
    return { el: wrap, paint, feed, analyzerTap: () => tap, busy: () => drag != null || Object.keys(local).length > 0 || Date.now() < busyUntil };
  }

  return { knob, fader, scrub, segmented, toggle, level, gr, mini, history, eqCurve, spec, fmtValue };
})();
