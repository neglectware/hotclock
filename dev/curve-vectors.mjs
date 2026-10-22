// Holds the JavaScript port of the gain curve (crates/serve/web/curve.js) to the vectors the Rust reference model
// (crates/dspmodel) writes: the same numbers to 1e-9 dB. Run: node dev/curve-vectors.mjs
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const Curve = createRequire(import.meta.url)(join(root, 'crates/serve/web/curve.js'));
const vectors = JSON.parse(readFileSync(join(root, 'crates/dspmodel/tests/vectors.json'), 'utf8'));
let worst = 0, points = 0;
for (const v of vectors) {
  v.freqs.forEach((f, i) => {
    const d = Math.abs(Curve.hDb(v.crossovers, v.gr, [], f) - v.db[i]);
    worst = Math.max(worst, d); points++;
    if (d > 1e-9) { console.error(`${v.name} at ${f} Hz: JS ${Curve.hDb(v.crossovers, v.gr, [], f)} vs Rust ${v.db[i]}`); process.exitCode = 1; }
  });
  // the table form agrees with the pointwise form
  const t = new Curve.BandTable(v.crossovers, v.freqs).curveDb(v.gr, []);
  t.forEach((x, i) => { if (Math.abs(x - v.db[i]) > 1e-9) { console.error(`${v.name}: table differs at ${i}`); process.exitCode = 1; } });
}
// the axis: the crossover line of 2 kHz sits at log10(2000 / 20) / 3 = 2/3 of the width
if (Math.abs(Curve.xFrac(2000) - 2 / 3) > 1e-12) { console.error('xFrac'); process.exitCode = 1; }
console.log(`${vectors.length} cases, ${points} points, worst difference ${worst.toExponential(2)} dB`);
