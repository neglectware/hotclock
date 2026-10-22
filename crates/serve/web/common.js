// Shared helpers for the admin app and the status display: DOM, fetch, WebSocket with sequence checking, the
// binary meter frame, formatting. No dependencies.
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
function h(tag, attrs, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (k === 'class') e.className = v;
    else if (k.startsWith('on')) e.addEventListener(k.slice(2), v);
    else if (k === 'html') e.innerHTML = v;
    else if (v === true) e.setAttribute(k, '');
    else if (v !== false && v != null) e.setAttribute(k, v);
  }
  for (const c of kids.flat()) if (c != null && c !== false) e.append(c.nodeType ? c : document.createTextNode(String(c)));
  return e;
}
class ApiError extends Error { constructor(status, body) { super(body?.error?.message || `HTTP ${status}`); this.status = status; this.code = body?.error?.code; } }
async function J(url, opts) {
  const r = await fetch(url, opts);
  let body = null;
  const text = await r.text();
  try { body = text ? JSON.parse(text) : null; } catch { body = null; }
  if (!r.ok) throw new ApiError(r.status, body);
  return body;
}
const jsend = (url, body, method = 'POST') => J(url, { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body ?? {}) });
function toast(msg, err) {
  const t = h('div', { class: 'toast' + (err ? ' err' : '') }, msg);
  document.body.append(t);
  setTimeout(() => t.remove(), err ? 6000 : 2500);
}
// assigning a text node's value is a DOM mutation even when it does not change: only write what changed
const setText = (el, t) => { if (el && el.textContent !== t) el.textContent = t; };
// A state chip as wide as the widest of its labels, the label centred, so a change of state never moves the text after
// it: every label sits in the same grid cell, hidden, and sets the width. Write the label with `setText(chip.label, t)`.
function fixedChip(labels, attrs) {
  const label = h('span', { class: 'fixw-v' });
  const el = h('span', attrs, label, ...labels.map(t => h('span', { class: 'fixw-g', 'aria-hidden': 'true' }, t)));
  el.classList.add('fixw');
  el.label = label;
  return el;
}
const pad = (n, w = 2) => String(n).padStart(w, '0');
function mmss(ms) {
  if (ms == null || !isFinite(ms)) return '--:--';
  const neg = ms < 0; ms = Math.abs(Math.round(ms / 1000));
  const m = Math.floor(ms / 60), s = ms % 60;
  return (neg ? '-' : '') + (m >= 60 ? Math.floor(m / 60) + ':' + pad(m % 60) : m) + ':' + pad(s);
}
function clock(d) { return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`; }
function hhmm(iso) { if (!iso) return ''; const d = new Date(iso); return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`; }
function dur(ms) { return mmss(ms); }
function esc(s) { return String(s ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }

// ---- the meter frame, version 3, 220 bytes, little endian (architecture 11.3) ----
function parseFrame(buf) {
  const v = new DataView(buf);
  if (buf.byteLength < 208 || v.getUint8(0) < 2) return null;
  const lv = o => { const x = v.getInt16(o, true); return x === -32768 ? null : x / 100; };
  const v3 = buf.byteLength >= 220;
  const db = o => Float32Array.from(new Uint8Array(buf, o, 64), x => x * 90 / 255 - 90);
  const stageNames = ['agc', 'multiband', 'wideband', 'limiter', 'clipper', 'safety'];
  const f = {
    version: v.getUint8(0), flags: v.getUint8(1), seq: v.getUint32(4, true),
    peakL: lv(16), peakR: lv(18), ppmL: lv(20), ppmR: lv(22), momentary: lv(24), shortTerm: lv(26),
    post: new Uint8Array(buf, 80, 64), pre: new Uint8Array(buf, 144, 64), postDb: db(80), preDb: db(144),
    stageMask: v.getUint8(28), bandCount: v.getUint8(29), chainRev: v.getUint16(30, true),
    stages: stageNames.map((name, i) => ({ name, in: lv(32 + i * 6), out: lv(34 + i * 6), gr: lv(36 + i * 6) })),
    bandGr: [0, 1, 2, 3, 4, 5].map(i => lv(68 + i * 2)),
    blk: buf.byteLength >= 276 ? Array.from({ length: 14 }, (_, i) => [lv(220 + i * 4), lv(222 + i * 4)]) : null,
    integrated: v3 ? lv(208) : null, window: v3 ? lv(210) : null,
    target: v3 ? lv(212) : null, gain: v3 ? v.getInt16(214, true) / 100 : 0, lstate: v3 ? v.getUint8(216) : 0,
    bypassed: (v.getUint8(1) & 1) !== 0, safetyActive: (v.getUint8(1) & 8) !== 0,
  };
  return f;
}

// ---- a WebSocket that reconnects, checks `seq`, and asks for a snapshot after a gap ----
class Live {
  constructor(url, handlers) {
    this.url = url; this.h = handlers; this.seq = null; this.closed = false; this.delay = 500; this.topics = new Set(); this.connect();
  }
  // Extra topics for this connection beyond the ones in the address (the equalizer analyzer's `eqspec_in` / `eqspec_out`):
  // `want(list)` makes the set exactly `list`, now and after a reconnect.
  want(list) {
    const next = new Set(list), add = [...next].filter(t => !this.topics.has(t)), del = [...this.topics].filter(t => !next.has(t));
    this.topics = next;
    if (this.ws && this.ws.readyState === 1) {
      if (del.length) this.ws.send(JSON.stringify({ cmd: 'unsubscribe', topics: del }));
      if (add.length) this.ws.send(JSON.stringify({ cmd: 'subscribe', topics: add }));
    }
  }
  connect() {
    const ws = new WebSocket(this.url); ws.binaryType = 'arraybuffer'; this.ws = ws;
    ws.onopen = () => { this.delay = 500; this.seq = null; if (this.topics.size) ws.send(JSON.stringify({ cmd: 'subscribe', topics: [...this.topics] })); this.h.open?.(); };
    ws.onmessage = ev => {
      if (typeof ev.data !== 'string') {
        // an analyzer frame starts with the bytes EQS1; a meter frame is 220 bytes
        const u = new Uint8Array(ev.data, 0, Math.min(4, ev.data.byteLength));
        if (u.length === 4 && u[0] === 0x45 && u[1] === 0x51 && u[2] === 0x53 && u[3] === 0x31) { this.h.spectrum?.(new Uint8Array(ev.data)); return; }
        this.h.meters?.(parseFrame(ev.data)); return;
      }
      const m = JSON.parse(ev.data);
      if (m.type !== 'snapshot' && this.seq != null && m.seq > this.seq + 1) ws.send('{"cmd":"snapshot"}');
      if (m.seq != null) this.seq = m.seq;
      (this.h[m.type] || (() => {}))(m.data, m);
    };
    ws.onclose = () => { this.h.close?.(); if (!this.closed) setTimeout(() => this.connect(), this.delay = Math.min(this.delay * 2, 8000)); };
  }
  stop() { this.closed = true; this.ws.close(); }
}

// ---- drawing ----
function dbX(db, w, lo = -60) { return Math.max(0, Math.min(1, (db - lo) / -lo)) * w; }
// The loudness controller's bar (`opts.lc`, drawn while the controller runs): centred on 0 dB, filling right for boost and left
// for cut, on a scale of the controller's own gain range (its limits are the grey ticks). Its colour is the state:
// grey OFF, blue CONVERGING, green ON TARGET, amber FROZEN or AT LIMIT.
const LC_COLORS = ['#4a5262', '#5aa9ff', '#3ecf8e', '#f5a623', '#f5a623'];
function drawMeters(cv, f, peakHold, opts) {
  const c = cv.getContext('2d'), W = cv.width, H = cv.height;
  c.clearRect(0, 0, W, H);
  const bars = [['L', f?.ppmL, f?.peakL], ['R', f?.ppmR, f?.peakR]];
  // Compact (the 7-inch panel, 11.2b): two bars filling the height, ticks at -20 and 0, no text at all
  // (nothing drawn on the panel may be under 31 px). With the controller's bar, three bars share the height.
  const compact = !!opts?.compact;
  const lc = opts?.lc || null;
  const bh = H * (compact ? (lc ? 0.26 : 0.4) : (lc ? 0.18 : 0.28)), top = H * (compact ? (lc ? 0.04 : 0.05) : (lc ? 0.02 : 0.1));
  const gap = H * (compact ? (lc ? 0.09 : 0.1) : (lc ? 0.06 : 0.1));
  bars.forEach(([name, ppm, pk], i) => {
    const y = top + i * (bh + gap);
    c.fillStyle = '#0b0e13'; c.fillRect(0, y, W, bh);
    if (ppm != null) {
      const x = dbX(ppm, W);
      const g = c.createLinearGradient(0, 0, W, 0);
      g.addColorStop(0, '#3ecf8e'); g.addColorStop(0.75, '#3ecf8e'); g.addColorStop(0.9, '#f5a623'); g.addColorStop(1, '#ff5c5c');
      c.fillStyle = g; c.fillRect(0, y, x, bh);
    }
    const hold = peakHold?.[i];
    if (hold != null) { c.fillStyle = '#fff'; c.fillRect(Math.min(W - 2, dbX(hold, W)), y, 2, bh); }
    if (compact) {
      c.fillStyle = '#97a1b2';
      for (const t of [-20, 0]) c.fillRect(Math.min(W - 2, dbX(t, W)), y, 2, bh);
      return;
    }
    c.fillStyle = '#97a1b2'; c.font = `${Math.round(H * 0.11)}px sans-serif`; c.textBaseline = 'middle'; c.fillText(name, 4, y + bh / 2);
  });
  if (lc) {
    // compact: the third bar; full: under the level scale
    const y = compact ? top + 2 * (bh + gap) : top + 2 * bh + gap + H * 0.2;
    c.fillStyle = '#0b0e13'; c.fillRect(0, y, W, bh);
    const range = Math.max(1, Math.ceil(Math.max(Math.abs(lc.min), Math.abs(lc.max))));
    const xOf = db => W / 2 + Math.max(-1, Math.min(1, db / range)) * W / 2;
    if (lc.gain != null) { const x = xOf(lc.gain); c.fillStyle = LC_COLORS[lc.lstate] || LC_COLORS[0]; c.fillRect(Math.min(W / 2, x), y, Math.abs(x - W / 2), bh); }
    c.fillStyle = '#97a1b2';
    for (const t of [lc.min, lc.max]) c.fillRect(Math.max(0, Math.min(W - 2, xOf(t) - 1)), y, 2, bh);
    c.fillStyle = '#e8ebf0'; c.fillRect(W / 2 - 1, y, 2, bh);
    if (!compact) { c.fillStyle = '#97a1b2'; c.font = `${Math.round(H * 0.11)}px sans-serif`; c.textBaseline = 'middle'; c.fillText('LC', 4, y + bh / 2); }
    cv.dataset.lc = JSON.stringify({ y: y / H, h: bh / H, range, x0: W / 2, x: lc.gain != null ? xOf(lc.gain) : null, color: LC_COLORS[lc.lstate] || LC_COLORS[0] });
  } else delete cv.dataset.lc;
  if (compact) return;
  c.fillStyle = '#97a1b2'; c.font = `${Math.round(H * 0.09)}px sans-serif`; c.textBaseline = 'top';
  for (const t of [-60, -40, -20, -10, 0]) {
    const x = dbX(t, W); c.fillRect(x - 0.5, top + 2 * bh + gap, 1, 6);
    c.textAlign = t === 0 ? 'right' : t === -60 ? 'left' : 'center'; c.fillText(String(t), x, top + 2 * bh + gap + 8);
  }
  c.textAlign = 'left';
}
function drawSpectrum(cv, f, peaks) {
  const c = cv.getContext('2d'), W = cv.width, H = cv.height;
  c.clearRect(0, 0, W, H);
  c.fillStyle = '#0b0e13'; c.fillRect(0, 0, W, H);
  c.strokeStyle = '#242c3a'; c.lineWidth = 1;
  for (let i = 1; i < 6; i++) { const y = H * i / 6; c.beginPath(); c.moveTo(0, y); c.lineTo(W, y); c.stroke(); }
  // Gridlines at 100 Hz, 1 kHz, 10 kHz on the log axis (20 Hz to 20 kHz across 64 bands).
  c.fillStyle = '#97a1b2'; c.font = `${Math.round(H * 0.075)}px sans-serif`; c.textBaseline = 'bottom';
  for (const [hz, label] of [[100, '100'], [1000, '1k'], [10000, '10k']]) {
    const x = W * Math.log(hz / 20) / Math.log(1000); c.strokeStyle = '#2e3646'; c.beginPath(); c.moveTo(x, 0); c.lineTo(x, H); c.stroke(); c.fillText(label, x + 3, H - 2);
  }
  if (!f) return;
  const bw = W / 64;
  for (let i = 0; i < 64; i++) {
    const v = f.post[i] / 255, hgt = v * H;
    c.fillStyle = `hsl(${40 - v * 40}, 90%, ${45 + v * 10}%)`; c.fillRect(i * bw + 1, H - hgt, bw - 2, hgt);
    if (peaks) { const p = peaks[i] / 255 * H; c.fillStyle = '#fff8'; c.fillRect(i * bw + 1, H - p - 2, bw - 2, 2); }
  }
}

// Scrolling never changes a setting. The wheel only scrolls the page: a number box that has the focus would take a
// turn of the wheel as a step (the browser's own), so it is blurred first and the page scrolls on.
document.addEventListener('wheel', () => { const a = document.activeElement; if (a && a.tagName === 'INPUT' && a.type === 'number') a.blur(); }, { capture: true, passive: true });

// ---- guarded keys: the On Air page's Stop, Cut and the End items (and any other action that must not fire by accident) ----
// holdToFire(el, ms, fn, opts): `fn` runs only after the pointer, or the Space or Enter key, has been held on `el` for `ms`
//   milliseconds. A click, a tap or a short press does nothing, so the guard cannot be skipped from the page. While held,
//   `el` has the class `holding` and an `i.fill` inside it runs from 0 to 100 % over `ms`. Letting go early, moving the
//   pointer off the key, losing focus or pressing Escape cancels. A browser's own long-press menu is suppressed (the
//   context menu is prevented; the CSS sets -webkit-touch-callout: none and user-select: none on the keys). From the
//   keyboard the rule is the same: Enter or Space must be HELD for `ms`; a tap on Enter does nothing. Give the key an
//   aria-label that says so ("Stop, hold for 1 second"). `opts.when()` may switch the guard off (the Stop key becomes Play
//   when the station is stopped: then the key's own click handler acts and this function stays out of the way).
//   Returns { cancel }; call it when the page goes away (the timer is the only thing that outlives the element).
function holdToFire(el, ms, fn, opts = {}) {
  const fill = el.querySelector(':scope > .fill') || el.appendChild(h('i', { class: 'fill', 'aria-hidden': 'true' }));
  let timer = 0, holding = false, fired = false;
  const stop = () => {
    if (!holding) return;
    holding = false; clearTimeout(timer); timer = 0;
    el.classList.remove('holding'); fill.style.transition = 'none'; fill.style.width = '0';
  };
  const start = () => {
    if (holding || el.disabled || (opts.when && !opts.when())) return;
    fired = false; holding = true; el.classList.add('holding');
    fill.style.transition = 'none'; fill.style.width = '0'; void fill.offsetWidth;
    fill.style.transition = `width ${ms}ms linear`; fill.style.width = '100%';
    timer = setTimeout(() => { timer = 0; stop(); fired = true; setTimeout(() => { fired = false; }, 600); fn(); }, ms);
  };
  // the click that ends the press which has just fired must not reach the key's own click handler (Stop turns into Play)
  el.addEventListener('click', ev => { if (fired) { fired = false; ev.stopImmediatePropagation(); ev.preventDefault(); } }, true);
  el.addEventListener('pointerdown', ev => {
    if (ev.pointerType === 'mouse' && ev.button !== 0) return;
    if (opts.when && !opts.when()) return;
    start(); try { el.setPointerCapture(ev.pointerId); } catch { }
  });
  for (const t of ['pointerup', 'pointercancel', 'lostpointercapture', 'pointerleave']) el.addEventListener(t, () => stop());
  el.addEventListener('pointermove', ev => {
    if (!holding) return;
    const r = el.getBoundingClientRect();
    if (ev.clientX < r.left || ev.clientX > r.right || ev.clientY < r.top || ev.clientY > r.bottom) stop();
  });
  el.addEventListener('keydown', ev => {
    if (ev.key === 'Escape') { stop(); return; }
    if ((ev.key === 'Enter' || ev.key === ' ') && !(opts.when && !opts.when())) { ev.preventDefault(); if (!ev.repeat) start(); }
  });
  el.addEventListener('keyup', ev => { if (ev.key === 'Enter' || ev.key === ' ') stop(); });
  el.addEventListener('blur', () => stop());
  el.addEventListener('contextmenu', ev => ev.preventDefault());
  return { cancel: stop };
}
// coverKey(el, armMs, fn, opts): a key under a cover. The first press (click, tap, Enter or Space) lifts the cover: `el` gets
//   the class `armed` and, if `opts.text(secondsLeft)` is given, the label (`opts.label` or `el`) shows it as a countdown.
//   A second press within `armMs` runs `fn`; after that time, after the second press or on Escape the cover closes again.
//   Used for Cut, where a hold would spoil the timing, and for "Fire" in the rundown (a two-step key).
function coverKey(el, armMs, fn, opts = {}) {
  const lab = opts.label || el;
  let armed = false, timer = 0, tick = 0, saved = '', until = 0;
  const paint = () => { if (opts.text) lab.textContent = opts.text(Math.max(0, Math.ceil((until - Date.now()) / 1000))); };
  const disarm = () => {
    if (!armed) return;
    armed = false; clearTimeout(timer); clearInterval(tick); timer = tick = 0;
    el.classList.remove('armed'); el.removeAttribute('data-armed'); lab.textContent = saved;
    if (opts.restore) opts.restore();
  };
  const arm = () => {
    armed = true; saved = lab.textContent; until = Date.now() + armMs;
    el.classList.add('armed'); el.setAttribute('data-armed', '1'); paint();
    tick = setInterval(paint, 250); timer = setTimeout(disarm, armMs);
  };
  el.addEventListener('click', () => { if (el.disabled) return; if (!armed) { arm(); return; } disarm(); fn(); });
  el.addEventListener('keydown', ev => { if (ev.key === 'Escape') disarm(); });
  return { cancel: disarm, get armed() { return armed; } };
}
