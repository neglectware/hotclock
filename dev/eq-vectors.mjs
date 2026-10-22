// Holds the JavaScript equalizer curve (crates/serve/web/eqmath.js, what the Processing page draws) to the response the
// engine's own coefficients give (crates/engine/tests/eq_vectors.json, written by the Rust test
// `the_engine_response_equals_the_formulas_the_page_draws`, which also holds the running engine to it within 0.1 dB).
// Run: node dev/eq-vectors.mjs
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const EqMath = createRequire(import.meta.url)(join(root, 'crates/serve/web/eqmath.js'));
const vectors = JSON.parse(readFileSync(join(root, 'crates/engine/tests/eq_vectors.json'), 'utf8'));
// the block as the page holds it: the defaults of the schema under the case's own values
const defaults = { bands: 4 };
for (let n = 1; n <= 8; n++) Object.assign(defaults, { ['b' + n + '_on']: n <= 4, ['b' + n + '_type']: 'peak', ['b' + n + '_hz']: 1000, ['b' + n + '_db']: 0, ['b' + n + '_q']: 1 });
Object.assign(defaults, { b1_type: 'low_shelf', b1_hz: 100, b1_q: EqMath.Q_SHELF, b2_hz: 250, b3_hz: 3000, b4_type: 'high_shelf', b4_hz: 10000, b4_q: EqMath.Q_SHELF });
let worst = 0, points = 0, fail = 0;
for (const v of vectors) {
  const block = { ...defaults, ...v.eq };
  const js = EqMath.responseDb(block, v.freqs);
  v.freqs.forEach((f, i) => {
    const d = Math.abs(js[i] - v.db[i]);
    // a point deep in a null amplifies rounding: hold it to 1e-9 relative to the passband instead
    const tol = v.db[i] < -80 ? 1e-3 : 1e-6;
    worst = Math.max(worst, v.db[i] < -80 ? 0 : d); points++;
    if (d > tol) { console.error(`${v.name} at ${f.toFixed(1)} Hz: JS ${js[i]} vs Rust ${v.db[i]}`); fail++; }
  });
}
if (fail) process.exitCode = 1;
console.log(`${vectors.length} cases, ${points} points, worst difference ${worst.toExponential(2)} dB (limit 1e-6), ${fail} failures`);
