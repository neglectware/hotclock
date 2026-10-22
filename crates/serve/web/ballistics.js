// Meter ballistics (architecture 11.2): the JavaScript twin of `common::meters`, applied by the web meters. The frame
// carries the as-measured PPM and the un-ballisticked sample peak; the box's settings (one set, shared with the native
// display, changed on the On Air page or through PUT /api/v1/display/meters) say how a bar rises and falls.
const Ballistics = (() => {
  const FLOOR = -90;
  const DEFAULT = { preset: 'ppm_iec1', source: 'ppm', attack_ms: 0, release_db_s: 11.8, hold: true, hold_ms: 2000, hold_fall_db_s: 0, gr_release_db_s: 15, gr_hold_ms: 1000 };
  let params = { ...DEFAULT };
  class Meter {
    constructor() { this.bar = null; this.level = FLOOR; this.hold = null; this.age = 0; }
    step(p, ppm, peak, dt) {
      if (p.source === 'ppm') { this.bar = ppm; this.level = ppm == null ? FLOOR : ppm; }
      else {
        const t = peak == null ? FLOOR : peak, cur = this.level;
        const next = t > cur ? (p.attack_ms <= 0 ? t : Math.min(t, cur + 20 / (p.attack_ms / 1000) * dt)) : Math.max(t, cur - p.release_db_s * dt);
        this.level = Math.max(FLOOR, next); this.bar = next > FLOOR + 0.1 ? next : null;
      }
      if (!p.hold) { this.hold = null; return; }
      if (peak != null && (this.hold == null || peak >= this.hold)) { this.hold = peak; this.age = 0; return; }
      if (this.hold == null) return;
      this.age += dt * 1000;
      if (this.age > p.hold_ms) {
        if (p.hold_fall_db_s > 0) { const n = Math.max(this.bar == null ? FLOOR : this.bar, this.hold - p.hold_fall_db_s * dt); this.hold = n > FLOOR + 0.1 ? n : null; }
        else { this.hold = peak; this.age = 0; }
      }
    }
  }
  class Bars {
    constructor() { this.m = [new Meter(), new Meter()]; this.last = 0; }
    // returns a frame-like object whose ppmL/ppmR are the bar levels, and the hold ticks
    update(f, now) {
      const dt = this.last ? Math.min(0.25, Math.max(0, (now - this.last) / 1000)) : 0; this.last = now;
      const ch = [[f?.ppmL ?? null, f?.peakL ?? null], [f?.ppmR ?? null, f?.peakR ?? null]];
      ch.forEach(([ppm, peak], i) => this.m[i].step(params, ppm, peak, dt));
      return { frame: { ppmL: this.m[0].bar, ppmR: this.m[1].bar, peakL: null, peakR: null }, hold: [this.m[0].hold, this.m[1].hold] };
    }
  }
  // The gain-reduction bars of the processing view (the twin of `common::meters::GrMeter`): reduction in dB (0 or
  // negative) in, a bar that is positive and grows with it; the attack is instant, the release is `gr_release_db_s`,
  // and the peak tick waits `gr_hold_ms` before it falls toward the bar at the same rate.
  class GrMeter {
    constructor() { this.bar = 0; this.hold = null; this.age = 0; }
    step(p, gr, dt) {
      const g = gr == null ? 0 : Math.max(0, -gr);
      this.bar = g >= this.bar ? g : Math.max(g, this.bar - p.gr_release_db_s * dt);
      if (p.gr_hold_ms <= 0) { this.hold = null; return; }
      if (this.hold == null) { if (g > 0.05) { this.hold = g; this.age = 0; } return; }
      if (g >= this.hold) { this.hold = g; this.age = 0; return; }
      this.age += dt * 1000;
      if (this.age > p.gr_hold_ms) { const n = Math.max(this.bar, this.hold - p.gr_release_db_s * dt); this.hold = n > 0.05 ? n : null; }
    }
  }
  // Nine gain-reduction bars (bands 1 to 6, wideband, limiter, clipper) and the level into the chain, as the native
  // display's `Dynamics` keeps them. `update(frame, now)` takes a parsed meter frame or null.
  class Proc {
    constructor() { this.gr = Array.from({ length: 9 }, () => new GrMeter()); this.input = new Meter(); this.last = 0; }
    static grOf(f, i) {
      if (!f || f.bypassed) return null;
      const on = b => (f.stageMask >> b & 1) === 1;
      if (i < 6) return on(1) ? f.bandGr[i] : null;
      return on(i - 4) ? f.stages[i - 4].gr : null;   // 6: wideband (stage 2), 7: limiter (3), 8: clipper (4)
    }
    static inputPeak(f) {
      if (!f) return null;
      if (!f.bypassed) for (let i = 0; i < 5; i++) if ((f.stageMask >> i & 1) === 1 && f.stages[i].in != null) return f.stages[i].in;
      const a = f.peakL, b = f.peakR;
      return a != null && b != null ? Math.max(a, b) : a ?? b;
    }
    update(f, now) {
      const dt = this.last ? Math.min(0.25, Math.max(0, (now - this.last) / 1000)) : 0; this.last = now;
      this.gr.forEach((m, i) => m.step(params, Proc.grOf(f, i), dt));
      this.input.step({ ...params, source: 'peak' }, null, Proc.inputPeak(f), dt);
      return { gr: this.gr, input: this.input.bar, inputHold: this.input.hold };
    }
  }
  const listeners = [];
  function set(p) { params = { ...DEFAULT, ...p }; window.__ballistics = params; listeners.forEach(l => l(params)); }
  // The box's settings are a default and an override per station (`{default, stations: {name: ...}}`): a viewer uses the
  // setting of the station it draws. It follows the library service's `display` topic, so a change made anywhere reaches
  // every viewer within a second.
  const effective = (m, st) => (st && m.stations && m.stations[st]) || m.default || m;
  let started = false;
  let followed = null;
  function follow(station) {
    // the page may ask again for another station (the On Air tabs): the one socket stays, the station it applies for changes
    followed = station;
    if (started || typeof Live === 'undefined') { if (started) J('/api/v1/display/meters' + (station ? '?station=' + encodeURIComponent(station) : '')).then(set).catch(() => { }); return; }
    started = true;
    new Live(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/v1/ws?topics=display`, {
      snapshot(d) { if (d.meters) set(effective(d.meters, followed)); },
      display_meters(d) { set(effective(d, followed)); },
    });
    J('/api/v1/display/meters' + (station ? '?station=' + encodeURIComponent(station) : '')).then(set).catch(() => { });
  }
  return { Bars, Meter, GrMeter, Proc, set, follow, get params() { return params; }, onChange: f => listeners.push(f), DEFAULT };
})();
if (typeof module !== 'undefined') module.exports = Ballistics;
