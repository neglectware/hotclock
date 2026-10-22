// The multiband gain curve of architecture 11.2a: a port of the `dspmodel` crate (hotclock-dsp-model), held to the
// same test vectors (crates/dspmodel/tests/vectors.json, dev/curve-vectors.mjs). The effective response is
// H(f) = | sum_i g_i B_i(f) | with g_i = 10^(GR_i / 20) and B_i the cascaded Linkwitz-Riley band filters:
// B_i = HP_0 ... HP_(i-1) LP_i AP_(i+1) ... AP_(n-2), each pair 4th order (two Butterworth sections).
const Curve = (() => {
  const cmul = (a, b) => [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]];
  const cadd = (a, b) => [a[0] + b[0], a[1] + b[1]];
  const cdiv = (a, b) => { const d = b[0] * b[0] + b[1] * b[1]; return [(a[0] * b[0] + a[1] * b[1]) / d, (a[1] * b[0] - a[0] * b[1]) / d]; };
  const F_MIN = 20, F_MAX = 20000;
  // x = log10(f / 20) / 3 across the zone width, 0 at 20 Hz and 1 at 20 kHz
  const xFrac = f => Math.log10(f / F_MIN) / 3;
  const freqAt = x => F_MIN * Math.pow(10, 3 * x);
  function butter2(fc, f) {
    const w = f / fc, den = [1 - w * w, Math.SQRT2 * w];
    return [cdiv([1, 0], den), cdiv([-w * w, 0], den)];
  }
  function lr4(fc, f) { const [lp, hp] = butter2(fc, f); return [cmul(lp, lp), cmul(hp, hp)]; }
  function bandResponse(xo, band, f) {
    let b = [1, 0];
    xo.forEach((fc, k) => {
      const [lp, hp] = lr4(fc, f);
      b = k < band ? cmul(b, hp) : k === band ? cmul(b, lp) : cmul(b, cadd(lp, hp));
    });
    return b;
  }
  function logFreqs(n) { const a = []; for (let i = 0; i < n; i++) a.push(freqAt(i / (Math.max(n, 2) - 1))); return a; }
  const gain = (gr, mk, i) => Math.pow(10, ((gr[i] || 0) + (mk[i] || 0)) / 20);
  function hDb(xo, gr, makeup, f) {
    let s = [0, 0];
    for (let i = 0; i <= xo.length; i++) { const r = bandResponse(xo, i, f); const g = gain(gr, makeup || [], i); s = cadd(s, [r[0] * g, r[1] * g]); }
    return 20 * Math.log10(Math.max(Math.hypot(s[0], s[1]), 1e-12));
  }
  // The band responses on a grid, computed once per chain configuration; a frame then costs bands x points.
  class BandTable {
    constructor(xo, freqs) {
      this.freqs = freqs; this.n = xo.length + 1;
      this.resp = Array.from({ length: this.n }, (_, b) => freqs.map(f => bandResponse(xo, b, f)));
    }
    curveDb(gr, makeup) {
      const g = Array.from({ length: this.n }, (_, i) => gain(gr, makeup || [], i));
      return this.freqs.map((_, p) => {
        let re = 0, im = 0;
        for (let i = 0; i < this.n; i++) { re += this.resp[i][p][0] * g[i]; im += this.resp[i][p][1] * g[i]; }
        return 20 * Math.log10(Math.max(Math.hypot(re, im), 1e-12));
      });
    }
  }
  return { F_MIN, F_MAX, xFrac, freqAt, bandResponse, logFreqs, hDb, BandTable };
})();
if (typeof module !== 'undefined') module.exports = Curve;
