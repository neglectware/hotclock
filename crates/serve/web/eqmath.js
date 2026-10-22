// The parametric equalizer's filter maths for the Processing page's curve: the RBJ cookbook biquads exactly as
// crates/engine/src/chain/dsp.rs builds them (peak, low and high shelf, low-pass, high-pass, notch, band-pass), at the
// engine's 48 kHz. dev/eq-vectors.mjs holds this file to the response the Rust side computes from the engine's own
// coefficients (crates/engine/tests/eq_vectors.json), so the curve on the page is the sound.
// In a page it is the global `EqMath`; under node, `require()` returns the same object.
(function (root, factory) {
  const m = factory();
  if (typeof module === 'object' && module.exports) module.exports = m; else root.EqMath = m;
})(typeof self !== 'undefined' ? self : this, function () {
  const RATE = 48000;
  const MAX_BANDS = 8;
  const Q_SHELF = 1 / Math.SQRT2;
  // [value, name in the menu, has a gain, default Q, short name for a point's tooltip]
  const TYPES = [
    ['peak', 'Bell', true, 1],
    ['low_shelf', 'Low shelf', true, Q_SHELF],
    ['high_shelf', 'High shelf', true, Q_SHELF],
    ['low_pass', 'Low-pass', false, Q_SHELF],
    ['high_pass', 'High-pass', false, Q_SHELF],
    ['notch', 'Notch', false, 1],
    ['band_pass', 'Band-pass', false, 1],
  ];
  const info = type => TYPES.find(t => t[0] === type) || TYPES[0];
  const hasGain = type => info(type)[2];
  const defaultQ = type => info(type)[3];
  const norm = (b, a) => ({ b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]], a: [a[1] / a[0], a[2] / a[0]] });
  const wcs = (f, rate) => { const w = 2 * Math.PI * Math.min(Math.max(f, 1), rate * 0.49) / rate; return [Math.cos(w), Math.sin(w)]; };

  function coef(type, f, q, db, rate = RATE) {
    const [c, s] = wcs(f, rate);
    const a = Math.pow(10, db / 40);
    const al = s / (2 * q);
    switch (type) {
      case 'low_shelf': {
        const t = 2 * Math.sqrt(a) * al;
        return norm([a * ((a + 1) - (a - 1) * c + t), 2 * a * ((a - 1) - (a + 1) * c), a * ((a + 1) - (a - 1) * c - t)],
          [(a + 1) + (a - 1) * c + t, -2 * ((a - 1) + (a + 1) * c), (a + 1) + (a - 1) * c - t]);
      }
      case 'high_shelf': {
        const t = 2 * Math.sqrt(a) * al;
        return norm([a * ((a + 1) + (a - 1) * c + t), -2 * a * ((a - 1) + (a + 1) * c), a * ((a + 1) + (a - 1) * c - t)],
          [(a + 1) - (a - 1) * c + t, 2 * ((a - 1) - (a + 1) * c), (a + 1) - (a - 1) * c - t]);
      }
      case 'low_pass': return norm([(1 - c) / 2, 1 - c, (1 - c) / 2], [1 + al, -2 * c, 1 - al]);
      case 'high_pass': return norm([(1 + c) / 2, -(1 + c), (1 + c) / 2], [1 + al, -2 * c, 1 - al]);
      case 'notch': return norm([1, -2 * c, 1], [1 + al, -2 * c, 1 - al]);
      case 'band_pass': return norm([al, 0, -al], [1 + al, -2 * c, 1 - al]);
      default: return norm([1 + al * a, -2 * c, 1 - al * a], [1 + al / a, -2 * c, 1 - al / a]);
    }
  }
  // 20 log10 |H| of a section at `f`
  function magDb(k, f, rate = RATE) {
    const w = 2 * Math.PI * f / rate, c1 = Math.cos(w), s1 = Math.sin(w), c2 = Math.cos(2 * w), s2 = Math.sin(2 * w);
    const nr = k.b[0] + k.b[1] * c1 + k.b[2] * c2, ni = -(k.b[1] * s1 + k.b[2] * s2);
    const dr = 1 + k.a[0] * c1 + k.a[1] * c2, di = -(k.a[0] * s1 + k.a[1] * s2);
    return 10 * Math.log10(Math.max((nr * nr + ni * ni) / (dr * dr + di * di), 1e-24));
  }
  // The bands of an equalizer block (`bands`, `b<n>_on`, `b<n>_type`, `b<n>_hz`, `b<n>_db`, `b<n>_q`), as objects.
  function bandsOf(v, includeOff) {
    const n = Math.min(MAX_BANDS, Math.max(1, v.bands | 0)), out = [];
    for (let i = 1; i <= n; i++) {
      const on = v['b' + i + '_on'] !== false;
      if (!on && !includeOff) continue;
      out.push({ n: i, on, type: v['b' + i + '_type'], hz: v['b' + i + '_hz'], db: v['b' + i + '_db'], q: v['b' + i + '_q'] });
    }
    return out;
  }
  const bandCoef = b => coef(b.type, b.hz, b.q, hasGain(b.type) ? b.db : 0);
  // The sections that do something: a bell or shelf at 0 dB is left out, as the engine leaves it out.
  const active = b => !hasGain(b.type) || Math.abs(b.db) > 1e-3;
  function sections(v) { return bandsOf(v, false).filter(active).map(bandCoef); }
  // The whole block's response (dB) at `f`, or at every frequency of a list.
  function responseDb(v, f) {
    const ks = sections(v);
    return Array.isArray(f) ? f.map(x => ks.reduce((s, k) => s + magDb(k, x), 0)) : ks.reduce((s, k) => s + magDb(k, f), 0);
  }
  return { RATE, MAX_BANDS, Q_SHELF, TYPES, hasGain, defaultQ, coef, magDb, bandsOf, bandCoef, sections, responseDb };
});
