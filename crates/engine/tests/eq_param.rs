//! The parametric equalizer (BUILDLOG "Parametric EQ"): old settings sound exactly as before, every band type is
//! stable, the engine's response equals the formulas the Processing page draws, a drag is click-free, and the cost.
//! Each test prints its measured value next to its limit.

#![allow(clippy::needless_range_loop)]

use common::chain::{preset_config, ChainConfig};
use engine::chain::{Chain, ChainMeters};
use serde_json::{json, Value};
use std::f64::consts::PI;
use std::sync::Arc;

const RATE: u32 = 48_000;
const R: f64 = 48_000.0;

fn build(cfg: &ChainConfig) -> Chain {
    Chain::build(cfg, RATE, Arc::new(ChainMeters::default()))
}

fn cfg_with(base: &str, edits: &[(&str, Value)]) -> ChainConfig {
    let mut c = preset_config(base).unwrap();
    for (b, v) in edits {
        c.apply_block(b, v.as_object().unwrap()).unwrap();
    }
    c
}

/// A fixed stereo test signal: noise (LCG, different per channel) plus two sines, 2 s, peaks near -6 dBFS.
fn program() -> Vec<f32> {
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((s >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
    };
    (0..(2.0 * R) as usize)
        .flat_map(|i| {
            let t = i as f64 / R;
            let tone = 0.15 * (2.0 * PI * 220.0 * t).sin() + 0.1 * (2.0 * PI * 3_150.0 * t).sin();
            let l = (tone + 0.12 * next()) as f32;
            let r = (tone * 0.8 + 0.12 * next()) as f32;
            [l, r]
        })
        .collect()
}

fn run(cfg: &ChainConfig) -> Vec<f32> {
    let mut ch = build(cfg);
    let mut x = program();
    for c in x.chunks_mut(1024 * 2) {
        ch.process(c, true);
    }
    x
}

/// FNV-1a over the bit patterns: equal hashes mean equal bytes.
fn hash(x: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in x {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The old fixed-band settings (the keys the equalizer had before the parametric one) and the hash of the audio the
/// chain made from them before the change (recorded from the code at main e790ec4).
fn old_cases() -> Vec<(&'static str, ChainConfig)> {
    let eq = |v: Value| vec![("eq", v)];
    vec![
        ("default (all bypassed)", ChainConfig::default()),
        ("radio", preset_config("radio").unwrap()),
        ("gentle", preset_config("gentle").unwrap()),
        (
            "eq on, flat",
            cfg_with("radio", &eq(json!({"enabled": true}))),
        ),
        (
            "warm",
            cfg_with("radio", &eq(json!({"low_db": 2.5, "high_db": -2.0}))),
        ),
        (
            "presence post",
            cfg_with(
                "radio",
                &eq(json!({"mid2_db": 2.5, "mid2_q": 0.9, "position": "post"})),
            ),
        ),
        (
            "smile",
            cfg_with(
                "gentle",
                &eq(json!({"low_db": 3.0, "mid1_db": -2.0, "high_db": 3.0})),
            ),
        ),
        (
            "all four, moved",
            cfg_with(
                "radio",
                &eq(
                    json!({"low_hz": 60.0, "low_db": -7.5, "mid1_hz": 333.0, "mid1_db": 4.5, "mid1_q": 2.5,
                           "mid2_hz": 5200.0, "mid2_db": -6.0, "mid2_q": 0.4, "high_hz": 14000.0, "high_db": 9.0}),
                ),
            ),
        ),
    ]
}

const OLD_HASHES: &[u64] = &[
    0x8aae8d81813b8ea5,
    0x2df9dd9e4ed1e80b,
    0xe3cbd557f92a3470,
    0x2df9dd9e4ed1e80b,
    0x9378334714930ba8,
    0x9661cae7ea8d4c9f,
    0xd55cdb4cb020effd,
    0x07b5c0a7f857d6dd,
];

#[test]
fn old_settings_make_byte_identical_audio() {
    let cases = old_cases();
    for (i, (name, cfg)) in cases.iter().enumerate() {
        let h = hash(&run(cfg));
        println!("OLDHASH {i} {name}: {h:#018x}");
        assert_eq!(h, OLD_HASHES[i], "{name}");
    }
}

// ---- the bands ------------------------------------------------------------------------------------------------------

/// An equalizer block from bands `(type, hz, db, q)`; the first `n` are on.
fn eq_block(bands: &[(&str, f64, f64, f64)]) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("enabled".into(), json!(true));
    o.insert("bands".into(), json!(bands.len()));
    for (i, (t, hz, db, q)) in bands.iter().enumerate() {
        let n = i + 1;
        o.insert(format!("b{n}_on"), json!(true));
        o.insert(format!("b{n}_type"), json!(t));
        o.insert(format!("b{n}_hz"), json!(hz));
        o.insert(format!("b{n}_db"), json!(db));
        o.insert(format!("b{n}_q"), json!(q));
    }
    Value::Object(o)
}

fn eq_only(block: &Value) -> ChainConfig {
    let mut c = ChainConfig::default();
    c.apply_block("eq", block.as_object().unwrap()).unwrap();
    c
}

const Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// the owner's rolloff: a peak at 2080 Hz -3.6 dB Q 1.57, a peak at 4087 Hz -1.7 dB Q 1.0, a high shelf at 8043 Hz -6.4 dB.
fn marks() -> Value {
    eq_block(&[
        ("peak", 2080.0, -3.6, 1.57),
        ("peak", 4087.0, -1.7, 1.0),
        ("high_shelf", 8043.0, -6.4, Q),
    ])
}

fn eq_cases() -> Vec<(&'static str, Value)> {
    vec![
        ("flat default", json!({"enabled": true})),
        ("the owner's rolloff", marks()),
        (
            "the old four, moved",
            eq_block(&[
                ("low_shelf", 60.0, -7.5, Q),
                ("peak", 333.0, 4.5, 2.5),
                ("peak", 5200.0, -6.0, 0.4),
                ("high_shelf", 14000.0, 9.0, Q),
            ]),
        ),
        (
            "every type",
            eq_block(&[
                ("low_shelf", 200.0, 6.0, 0.5),
                ("high_pass", 80.0, 0.0, Q),
                ("notch", 1000.0, 0.0, 5.0),
                ("band_pass", 2500.0, 0.0, 2.0),
                ("low_pass", 12000.0, 0.0, Q),
                ("high_shelf", 6000.0, -9.0, 2.0),
                ("peak", 440.0, 12.5, 0.1),
                ("peak", 9000.0, -17.9, 10.0),
            ]),
        ),
        (
            "extremes",
            eq_block(&[
                ("high_pass", 20.0, 0.0, 10.0),
                ("high_shelf", 20000.0, 18.0, 10.0),
                ("notch", 20000.0, 0.0, 10.0),
                ("low_shelf", 20.0, -18.0, 0.1),
                ("peak", 20.0, 18.0, 10.0),
                ("peak", 20000.0, -18.0, 0.1),
            ]),
        ),
        (
            "extreme filters",
            eq_block(&[
                ("low_pass", 20.0, 0.0, 10.0),
                ("band_pass", 20.0, 0.0, 10.0),
            ]),
        ),
    ]
}

fn log_freqs(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 20.0 * 1000f64.powf(i as f64 / (n - 1) as f64))
        .collect()
}

/// The analytic response (dB) of the equalizer block at `f`, from the engine's coefficients.
fn analytic_db(block: &Value, f: f64) -> f64 {
    let c = eq_only(block);
    let n = c.num("eq", "bands") as usize;
    let mut sum = 0.0;
    for i in 1..=n {
        if !c.flag("eq", &format!("b{i}_on")) {
            continue;
        }
        let k = |s: &str| format!("b{i}_{s}");
        let co = engine::chain::blocks::eq_band_coef(
            &c.text("eq", &k("type")),
            c.num("eq", &k("hz")),
            c.num("eq", &k("db")),
            c.num("eq", &k("q")),
            R,
        );
        let m = engine::chain::dsp::magnitude(&co, f, R);
        sum += 20.0 * m.max(1e-12).log10();
    }
    sum
}

/// The response the running engine makes: an impulse through `Chain::process`, summed at each frequency.
fn measured_db(block: &Value, freqs: &[f64]) -> Vec<f64> {
    let mut ch = build(&eq_only(block));
    let n = 1 << 18;
    let mut x = vec![0.0f32; n * 2];
    x[0] = 0.5;
    x[1] = 0.5;
    for c in x.chunks_mut(1024 * 2) {
        ch.process(c, true);
    }
    let h: Vec<f64> = (0..n).map(|i| f64::from(x[i * 2]) / 0.5).collect();
    freqs
        .iter()
        .map(|&f| {
            let (mut re, mut im) = (0.0, 0.0);
            let w = 2.0 * PI * f / R;
            let (ds, dc) = (w.sin(), w.cos());
            let (mut s, mut c) = (0.0f64, 1.0f64);
            for (i, v) in h.iter().enumerate() {
                if i % 4096 == 0 {
                    s = (w * i as f64).sin();
                    c = (w * i as f64).cos();
                }
                re += v * c;
                im -= v * s;
                (s, c) = (s * dc + c * ds, c * dc - s * ds);
            }
            20.0 * re.hypot(im).max(1e-12).log10()
        })
        .collect()
}

#[test]
fn the_engine_response_equals_the_formulas_the_page_draws() {
    // `eq_vectors.json` holds the analytic response of each case at 256 points; dev/eq-vectors.mjs holds the
    // JavaScript curve (crates/serve/web/eqmath.js) to it, and this test holds the running engine to it.
    // Regenerate by deleting the file (or with `UPDATE_EQ_VECTORS=1` outside the container).
    let freqs = log_freqs(256);
    let now: Vec<Value> = eq_cases()
        .iter()
        .map(|(name, b)| {
            json!({"name": name, "eq": b, "freqs": freqs,
                   "db": freqs.iter().map(|&f| analytic_db(b, f)).collect::<Vec<_>>()})
        })
        .collect();
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eq_vectors.json");
    if std::env::var_os("UPDATE_EQ_VECTORS").is_some() || !p.exists() {
        std::fs::write(&p, serde_json::to_string(&now).unwrap() + "\n").unwrap();
    }
    let have: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(&p).expect("eq_vectors.json")).unwrap();
    assert_eq!(have.len(), now.len());
    for (h, n) in have.iter().zip(&now) {
        assert_eq!(h["eq"], n["eq"], "{}", n["name"]);
        for (a, b) in h["db"]
            .as_array()
            .unwrap()
            .iter()
            .zip(n["db"].as_array().unwrap())
        {
            assert!(
                (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 1e-9,
                "{} (run with UPDATE_EQ_VECTORS=1)",
                n["name"]
            );
        }
    }
    // The engine, measured. A point more than 50 dB under the passband is a null where an f32 impulse is not
    // exact; those are skipped (and counted).
    for (name, b) in eq_cases() {
        let m = measured_db(&b, &freqs);
        let (mut worst, mut skipped) = (0.0f64, 0);
        for (i, &f) in freqs.iter().enumerate() {
            let a = analytic_db(&b, f);
            if a < -50.0 {
                skipped += 1;
                continue;
            }
            worst = worst.max((m[i] - a).abs());
        }
        println!("response, {name}: engine against formula at 256 points, worst {worst:.5} dB (limit 0.1), {skipped} nulls skipped");
        assert!(worst < 0.1, "{name}: {worst}");
    }
}

#[test]
fn marks_rolloff_has_the_shape_he_asked_for() {
    let b = marks();
    let at = |f: f64| analytic_db(&b, f);
    // Flat well under the first dip, the dip near its centre, and the top end down by the shelf.
    println!(
        "the owner's rolloff: 200 Hz {:+.3} dB, 2080 Hz {:+.3} dB, 4087 Hz {:+.3} dB, 8043 Hz {:+.3} dB, 20 kHz {:+.3} dB",
        at(200.0), at(2080.0), at(4087.0), at(8043.0), at(20000.0)
    );
    assert!(at(200.0).abs() < 0.15);
    assert!(at(2080.0) < -3.6 && at(2080.0) > -6.5);
    assert!(at(20000.0) < -6.0 && at(20000.0) > -7.0);
    // Each stored value is exactly what was typed: no snapping to a coarser step.
    let c = eq_only(&b);
    assert_eq!(c.num("eq", "b1_hz"), 2080.0);
    assert_eq!(c.num("eq", "b2_hz"), 4087.0);
    assert_eq!(c.num("eq", "b2_db"), -1.7);
    assert_eq!(c.num("eq", "b1_q"), 1.57);
    assert_eq!(c.num("eq", "b3_hz"), 8043.0);
    assert_eq!(c.num("eq", "b3_db"), -6.4);
}

#[test]
fn the_shipped_tv_era_preset_is_marks_rolloff() {
    let p = common::chain::effect_presets("eq")
        .into_iter()
        .find(|p| p.name == "TV-era rolloff")
        .expect("shipped preset");
    let mut c = ChainConfig::default();
    c.set_block_params("eq", p.params.as_object().unwrap())
        .unwrap();
    c.apply_block("eq", json!({"enabled": true}).as_object().unwrap())
        .unwrap();
    let worst = log_freqs(256)
        .iter()
        .map(|&f| {
            let mut b = serde_json::Map::new();
            for (k, v) in c.block_json("eq").as_object().unwrap() {
                b.insert(k.clone(), v.clone());
            }
            (analytic_db(&Value::Object(b), f) - analytic_db(&marks(), f)).abs()
        })
        .fold(0.0, f64::max);
    println!("TV-era rolloff preset against the owner's settings at 256 points: worst {worst:.2e} dB");
    assert!(worst < 1e-9);
}

#[test]
fn every_band_type_is_stable_at_every_extreme() {
    let types = [
        "peak",
        "low_shelf",
        "high_shelf",
        "low_pass",
        "high_pass",
        "notch",
        "band_pass",
    ];
    let freqs = [
        20.0, 21.0, 50.0, 100.0, 1000.0, 5000.0, 10000.0, 15000.0, 19000.0, 20000.0,
    ];
    let (mut worst, mut n) = (0.0f64, 0);
    for t in types {
        for &hz in &freqs {
            for q in [0.1, 0.5, Q, 1.0, 3.0, 10.0] {
                for db in [-18.0, 18.0] {
                    let c = engine::chain::blocks::eq_band_coef(t, hz, db, q, R);
                    // Poles of z^2 + a1 z + a2: inside the unit circle iff |a2| < 1 and |a1| < 1 + a2.
                    let (a1, a2) = (c.a[0], c.a[1]);
                    assert!(
                        a2.abs() < 1.0 && a1.abs() < 1.0 + a2,
                        "{t} {hz} Hz Q {q} {db} dB: a1 {a1} a2 {a2}"
                    );
                    // The pole radius: sqrt(a2) when complex, else the larger real root.
                    let disc = a1 * a1 - 4.0 * a2;
                    let r = if disc < 0.0 {
                        a2.sqrt()
                    } else {
                        ((-a1.abs() + disc.sqrt()).abs() / 2.0)
                            .max((-a1.abs() - disc.sqrt()).abs() / 2.0)
                    };
                    worst = worst.max(r);
                    n += 1;
                }
            }
        }
    }
    println!("stability: {n} designs (7 types, 10 frequencies, 6 Q, 2 gains), largest pole radius {worst:.9} (limit < 1)");
    assert!(worst < 1.0);
    // And the whole eight-band block at the worst settings runs on noise without a non-finite sample.
    let mut ch = build(&eq_only(
        &eq_cases()
            .into_iter()
            .find(|c| c.0 == "extremes")
            .unwrap()
            .1,
    ));
    let mut x = program();
    let mut peak = 0.0f32;
    for c in x.chunks_mut(1024 * 2) {
        ch.process(c, true);
        for v in c.iter() {
            assert!(v.is_finite());
            peak = peak.max(v.abs());
        }
    }
    println!("extremes through the engine on noise: peak {peak:.3} (finite)");
}

// ---- a drag is click-free ---------------------------------------------------------------------------------------------

/// The Processing page sends a changed value at most every 100 ms; each is a chain swap with a 50 ms crossfade. This
/// drags a bell's gain from 0 to +12 dB across a 1 kHz sine in 0.1 dB steps, one step every 25 ms (4x the page's
/// rate), and measures what a zipper would add: energy away from the tone.
#[test]
fn a_continuous_drag_makes_no_zipper_noise() {
    use engine::chain::{new_pair, ChainHost};
    let (mut host, ctl): (ChainHost, _) = new_pair(RATE, 480);
    let secs = 3.0;
    let n = (secs * R) as usize;
    let a = 0.1;
    let input: Vec<f32> = (0..n)
        .flat_map(|i| {
            let v = (a * (2.0 * PI * 1000.0 * i as f64 / R).sin()) as f32;
            [v, v]
        })
        .collect();
    let mut out = vec![0.0f32; n * 2];
    let block = 480; // 10 ms
    let mut step = 0;
    for (bi, (inp, o)) in input
        .chunks(block * 2)
        .zip(out.chunks_mut(block * 2))
        .enumerate()
    {
        // every 25 ms: not a multiple of the block, so use 20 ms (every other block) to stay simple
        if bi % 2 == 0 && step <= 120 {
            let db = step as f64 * 0.1;
            let mut c = ChainConfig::default();
            c.apply_block(
                "eq",
                eq_block(&[("peak", 1000.0, db, 4.0)]).as_object().unwrap(),
            )
            .unwrap();
            ctl.apply(&c).unwrap();
            step += 1;
        }
        host.process(inp, o);
        ctl.pump();
    }
    // The tone's own growth is the point of the sweep; take it out by fitting amplitude and phase over 5 ms windows
    // and measuring what is left (the residual), against the same measure on a hard-switched (no crossfade) bell.
    let resid = |y: &[f32]| -> f64 {
        // Per 5 ms window, least squares on sin, cos, t*sin, t*cos (a tone whose amplitude and phase move
        // linearly); what is left is a step or a click.
        let win = 240;
        let mut worst = 0.0f64;
        for w in (4800..n - win).step_by(win) {
            let mut m = [[0.0f64; 5]; 4];
            let basis = |i: usize| {
                let p = 2.0 * PI * 1000.0 * i as f64 / R;
                let t = (i - w) as f64 / win as f64 - 0.5;
                [p.sin(), p.cos(), t * p.sin(), t * p.cos()]
            };
            for i in w..w + win {
                let bv = basis(i);
                let v = f64::from(y[i * 2]);
                for r in 0..4 {
                    for c in 0..4 {
                        m[r][c] += bv[r] * bv[c];
                    }
                    m[r][4] += bv[r] * v;
                }
            }
            for col in 0..4 {
                let piv = (col..4)
                    .max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))
                    .unwrap();
                m.swap(col, piv);
                for r in 0..4 {
                    if r != col {
                        let f = m[r][col] / m[col][col];
                        for c in col..5 {
                            m[r][c] -= f * m[col][c];
                        }
                    }
                }
            }
            let coef: Vec<f64> = (0..4).map(|r| m[r][4] / m[r][r]).collect();
            let e: f64 = (w..w + win)
                .map(|i| {
                    let bv = basis(i);
                    let fit: f64 = (0..4).map(|k| coef[k] * bv[k]).sum();
                    (f64::from(y[i * 2]) - fit).powi(2)
                })
                .sum::<f64>()
                / win as f64;
            worst = worst.max(e);
        }
        10.0 * worst.max(1e-30).log10()
    };
    let r_cross = resid(&out);
    // The same drag with every step applied abruptly: the filter changes under the signal from one sample to the
    // next (state kept). That is what a zipper is.
    let mut ch = build(&eq_only(&eq_block(&[("peak", 1000.0, 0.0, 4.0)])));
    let mut hard = input.clone();
    let mut step = 0;
    for (bi, c) in hard.chunks_mut(block * 2).enumerate() {
        if bi % 2 == 0 && step <= 120 {
            let mut next = build(&eq_only(&eq_block(&[(
                "peak",
                1000.0,
                step as f64 * 0.1,
                4.0,
            )])));
            next.adopt(&ch);
            ch = next;
            step += 1;
        }
        ch.process(c, true);
    }
    let r_hard = resid(&hard);
    println!("zipper residual (worst 5 ms window, power in dB, full scale 1.0; the tone is at -20 dBFS peak): crossfaded swaps {r_cross:.1}, abrupt coefficient steps {r_hard:.1}");
    assert!(
        r_cross < -85.0 && r_cross < r_hard - 10.0,
        "{r_cross} against {r_hard}"
    );
}

// ---- CPU ----------------------------------------------------------------------------------------------------------------

#[test]
fn cpu_of_four_bands_against_eight() {
    let four = eq_block(&[
        ("low_shelf", 100.0, 3.0, Q),
        ("peak", 250.0, -2.0, 1.0),
        ("peak", 3000.0, 2.5, 1.0),
        ("high_shelf", 10000.0, -3.0, Q),
    ]);
    let eight = eq_block(&[
        ("low_shelf", 100.0, 3.0, Q),
        ("peak", 250.0, -2.0, 1.0),
        ("peak", 500.0, 1.5, 1.0),
        ("notch", 1000.0, 0.0, 4.0),
        ("peak", 3000.0, 2.5, 1.0),
        ("peak", 5000.0, -1.5, 2.0),
        ("high_pass", 40.0, 0.0, Q),
        ("high_shelf", 10000.0, -3.0, Q),
    ]);
    // The equalizer alone, and inside the `radio` chain (AGC, equalizer, five-band compressor, limiter).
    let mut per = vec![];
    for (name, base) in [
        ("equalizer alone", ChainConfig::default()),
        ("radio chain", preset_config("radio").unwrap()),
    ] {
        for (label, b) in [
            ("no equalizer", json!({"enabled": false})),
            ("4 bands", four.clone()),
            ("8 bands", eight.clone()),
        ] {
            let mut cfg = base.clone();
            cfg.apply_block("eq", b.as_object().unwrap()).unwrap();
            let mut best = f64::MAX;
            for _ in 0..5 {
                let mut ch = build(&cfg);
                let mut buf = program();
                let secs = buf.len() as f64 / 2.0 / R;
                let t = std::time::Instant::now();
                for _ in 0..10 {
                    for c in buf.chunks_mut(1024 * 2) {
                        ch.process(c, true);
                    }
                }
                best = best.min(t.elapsed().as_secs_f64() / (secs * 10.0));
            }
            println!(
                "{name}, {label}: {:.3} % of one core (best of 5, 20 s of audio each)",
                best * 100.0
            );
            per.push(best);
        }
    }
    assert!(per[2] < 0.05, "8 bands must stay a small part of a core");
    assert!(per[5] - per[4] < 0.02);
}
