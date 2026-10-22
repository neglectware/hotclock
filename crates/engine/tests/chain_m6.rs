//! M6 acceptance tests of the processing chain's blocks (offline: the real chain code, no audio device).
//! Each test prints its measured value next to its limit; BUILDLOG copies them.

#![allow(clippy::needless_range_loop, clippy::manual_is_multiple_of)]

use common::chain::{preset_config, ChainConfig};
use engine::chain::{Chain, ChainMeters};
use serde_json::{json, Value};
use std::f64::consts::PI;
use std::sync::Arc;

const RATE: u32 = 48_000;
const R: f64 = 48_000.0;

// ---- helpers -----------------------------------------------------------------------------------

fn cfg_with(base: &str, edits: &[(&str, Value)]) -> ChainConfig {
    let mut c = preset_config(base).unwrap();
    for (b, v) in edits {
        c.apply_block(b, v.as_object().unwrap()).unwrap();
    }
    c
}

fn build(cfg: &ChainConfig) -> Chain {
    Chain::build(cfg, RATE, Arc::new(ChainMeters::default()))
}

/// A stereo sine, `db` dBFS peak.
fn sine(hz: f64, db: f64, secs: f64, phase: f64) -> Vec<f32> {
    let a = 10f64.powf(db / 20.0);
    (0..(secs * R) as usize)
        .flat_map(|i| {
            let v = (a * (2.0 * PI * hz * i as f64 / R + phase).sin()) as f32;
            [v, v]
        })
        .collect()
}

fn process_in_chunks(ch: &mut Chain, x: &mut [f32], chunk: usize) {
    for c in x.chunks_mut(chunk * 2) {
        ch.process(c, true);
    }
}

/// RMS of the left channel over `[from, to)` seconds, dBFS.
fn rms_db(x: &[f32], from: f64, to: f64) -> f64 {
    let (a, b) = ((from * R) as usize * 2, (to * R) as usize * 2);
    let s: f64 = x[a..b]
        .chunks_exact(2)
        .map(|f| f64::from(f[0]).powi(2))
        .sum();
    10.0 * (s / ((b - a) / 2) as f64).max(1e-30).log10()
}

/// The magnitude (dB) of `f` in the left channel over whole periods of the tail, by correlation.
fn mag_db_at(x: &[f32], f: f64, from: f64, to: f64) -> f64 {
    let (a, b) = ((from * R) as usize, (to * R) as usize);
    let (mut s, mut c) = (0.0, 0.0);
    for i in a..b {
        let v = f64::from(x[i * 2]);
        let p = 2.0 * PI * f * i as f64 / R;
        s += v * p.sin();
        c += v * p.cos();
    }
    let n = (b - a) as f64;
    20.0 * (2.0 * (s * s + c * c).sqrt() / n).max(1e-30).log10()
}

fn log_freqs(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 20.0 * 1000f64.powf(i as f64 / (n - 1) as f64))
        .collect()
}

/// The impulse response (left) of a chain whose bands are held at fixed reductions, and its magnitude (dB) at
/// each frequency by direct summation.
fn ir_response(cfg: &ChainConfig, gr: &[f32], freqs: &[f64]) -> Vec<f64> {
    let meters = Arc::new(ChainMeters::default());
    meters.force_reductions(Some(gr));
    let mut ch = Chain::build(cfg, RATE, meters);
    let n = 1 << 18;
    let mut x = vec![0.0f32; n * 2];
    x[0] = 0.5;
    x[1] = 0.5;
    process_in_chunks(&mut ch, &mut x, 1024);
    let h: Vec<f64> = (0..n).map(|i| f64::from(x[i * 2]) / 0.5).collect();
    freqs
        .iter()
        .map(|&f| {
            let (mut re, mut im) = (0.0, 0.0);
            let w = 2.0 * PI * f / R;
            // Rotate a phasor instead of calling sin and cos per sample.
            let (ds, dc) = (w.sin(), w.cos());
            let (mut s, mut c) = (0.0f64, 1.0f64);
            for (i, v) in h.iter().enumerate() {
                if i % 8192 == 0 {
                    s = (w * i as f64).sin();
                    c = (w * i as f64).cos();
                }
                re += v * c;
                im -= v * s;
                let (ns, nc) = (s * dc + c * ds, c * dc - s * ds);
                s = ns;
                c = nc;
            }
            20.0 * re.hypot(im).log10()
        })
        .collect()
}

fn mb_cfg(crossovers: &[f64]) -> ChainConfig {
    let n = crossovers.len() + 1;
    let v = |x: f64| json!(vec![x; n]);
    let mut c = ChainConfig::default();
    c.apply_block(
        "multiband",
        json!({"enabled": true, "crossovers_hz": crossovers, "threshold_dbfs": v(0.0), "ratio": v(1.0),
            "knee_db": v(0.0), "attack_ms": v(10.0), "release_ms": v(100.0), "makeup_db": v(0.0)})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    c
}

const XOVERS: [&[f64]; 3] = [
    &[200.0, 4000.0],
    &[120.0, 500.0, 2000.0, 6000.0],
    &[80.0, 300.0, 1000.0, 3000.0, 9000.0],
];

// ---- 1 and 10: crossover flatness ----------------------------------------------------------------

#[test]
fn acceptance_1_and_10_crossovers_sum_flat() {
    let freqs = log_freqs(256);
    for xo in XOVERS {
        let cfg = mb_cfg(xo);
        let resp = ir_response(&cfg, &[0.0; 6], &freqs);
        let worst = resp.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        println!(
            "{} bands, crossovers {:?}: impulse-response magnitude, 256 points 20 Hz to 20 kHz: worst deviation {worst:.4} dB (limit 0.1)",
            xo.len() + 1,
            xo
        );
        assert!(worst < 0.1, "{xo:?}: {worst}");
        // The same by swept sine at 40 points, through the whole chain code.
        let mut worst_sine = 0.0f64;
        for f in log_freqs(40) {
            let mut ch = build(&cfg);
            let mut x = sine(f, -20.0, 2.5, 0.3);
            process_in_chunks(&mut ch, &mut x, 512);
            let m = mag_db_at(&x, f, 1.5, 2.5) + 20.0;
            worst_sine = worst_sine.max(m.abs());
        }
        println!("  sine sweep, 40 points: worst {worst_sine:.4} dB (limit 0.1)");
        assert!(worst_sine < 0.1);
    }
}

// ---- 11: curve model -----------------------------------------------------------------------------

#[test]
fn acceptance_11_the_dsp_model_matches_the_measured_response() {
    let freqs = log_freqs(256);
    let xo = XOVERS[1];
    let cases: [[f32; 5]; 3] = [
        [6.0, 0.0, 0.0, 0.0, 0.0],
        [3.0, 6.0, 9.0, 6.0, 3.0],
        [12.0; 5],
    ];
    let cfg = mb_cfg(xo);
    let model_flat = dspmodel::h_db(xo, &[0.0; 5], &[0.0; 5], 1000.0);
    println!("with all GR = 0 the model reads {model_flat:.5} dB at 1 kHz (flat within 0.1)");
    assert!(model_flat.abs() < 0.1);
    for gr in cases {
        let meas = ir_response(&cfg, &gr, &freqs);
        let neg: Vec<f64> = gr.iter().map(|g| -f64::from(*g)).collect();
        let mut worst = 0.0f64;
        let mut worst_xo = 0.0f64;
        for (i, &f) in freqs.iter().enumerate() {
            let model = dspmodel::h_db(xo, &neg, &[0.0; 5], f);
            worst = worst.max((model - meas[i]).abs());
        }
        // At each crossover frequency itself.
        let at: Vec<f64> = ir_response(&cfg, &gr, xo);
        for (i, &f) in xo.iter().enumerate() {
            worst_xo = worst_xo.max((dspmodel::h_db(xo, &neg, &[0.0; 5], f) - at[i]).abs());
        }
        println!(
            "forced GR {gr:?}: model against impulse-response measurement, 256 points: worst {worst:.4} dB, at the crossovers {worst_xo:.4} dB (limit 0.25)"
        );
        assert!(worst < 0.25 && worst_xo < 0.25);
        // A swept sine at a few points, including every crossover.
        let mut ws = 0.0f64;
        for &f in xo
            .iter()
            .chain([50.0, 300.0, 1000.0, 4000.0, 12000.0].iter())
        {
            let meters = Arc::new(ChainMeters::default());
            meters.force_reductions(Some(&gr));
            let mut ch = Chain::build(&cfg, RATE, meters);
            let mut x = sine(f, -20.0, 2.5, 0.1);
            process_in_chunks(&mut ch, &mut x, 512);
            let m = mag_db_at(&x, f, 1.5, 2.5) + 20.0;
            ws = ws.max((m - dspmodel::h_db(xo, &neg, &[0.0; 5], f)).abs());
        }
        println!("  swept sine at 10 points against the model: worst {ws:.4} dB (limit 0.25)");
        assert!(ws < 0.25);
    }
}

// ---- 2: the compressor ---------------------------------------------------------------------------

/// The static curve of 6: the gain reduction for a level `x` dB over threshold `t`, ratio `r`, knee `w`.
fn static_gr(x: f64, t: f64, r: f64, w: f64) -> f64 {
    let o = x - t;
    if 2.0 * o < -w {
        0.0
    } else if w > 0.0 && 2.0 * o.abs() <= w {
        (1.0 - 1.0 / r) * (o + w / 2.0).powi(2) / (2.0 * w)
    } else {
        (1.0 - 1.0 / r) * o
    }
}

fn wb_cfg(t: f64, r: f64, w: f64, a: f64, rel: f64) -> ChainConfig {
    cfg_with(
        "bypass",
        &[(
            "wideband",
            json!({"enabled": true, "threshold_dbfs": t, "ratio": r, "knee_db": w, "attack_ms": a, "release_ms": rel, "makeup_db": 0.0}),
        )],
    )
}

#[test]
fn acceptance_2_static_curve_on_tone_steps() {
    // The wideband compressor, and the third band of the five-band default at its centre.
    let mut worst = 0.0f64;
    for (t, r, w) in [
        (-24.0, 3.0, 6.0),
        (-30.0, 2.0, 0.0),
        (-18.0, 8.0, 12.0),
        (-20.0, 1.5, 3.0),
    ] {
        for lvl in [
            -50.0, -40.0, -30.0, -27.0, -24.0, -21.0, -18.0, -12.0, -6.0, -2.0,
        ] {
            let mut ch = build(&wb_cfg(t, r, w, 5.0, 100.0));
            let mut x = sine(1000.0, lvl, 1.5, 0.0);
            process_in_chunks(&mut ch, &mut x, 480);
            let gr = lvl - rms_db(&x, 1.0, 1.5) + 0.0 - (lvl - lvl);
            // rms_db of the output against the input's rms (lvl - 3.01)
            let measured = (lvl - 3.0103) - rms_db(&x, 1.0, 1.5);
            let _ = gr;
            let want = static_gr(lvl, t, r, w);
            worst = worst.max((measured - want).abs());
            assert!(
                (measured - want).abs() < 0.2,
                "wb t={t} r={r} w={w} lvl={lvl}: {measured:.3} vs {want:.3}"
            );
        }
    }
    println!("wideband compressor static curve, 4 settings x 10 levels (1 kHz): worst error {worst:.3} dB (limit 0.2)");
    // The multiband: two bands split at 200 Hz, a 4 kHz tone in the upper one (whose response there is flat), so
    // that the band's reduction is the whole signal's.
    let mut worst_mb = 0.0f64;
    let mut c = ChainConfig::default();
    c.apply_block(
        "multiband",
        json!({"enabled": true, "crossovers_hz": [200.0], "threshold_dbfs": [-20.0, -22.0], "ratio": [2.0, 3.0],
            "knee_db": [6.0, 6.0], "attack_ms": [30.0, 10.0], "release_ms": [300.0, 150.0], "makeup_db": [0.0, 0.0]})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    for lvl in [-40.0, -30.0, -25.0, -22.0, -19.0, -14.0, -8.0] {
        let mut ch = build(&c);
        let mut x = sine(4000.0, lvl, 2.0, 0.0);
        process_in_chunks(&mut ch, &mut x, 480);
        let measured = (lvl - 3.0103) - rms_db(&x, 1.5, 2.0);
        let want = static_gr(lvl, -22.0, 3.0, 6.0);
        worst_mb = worst_mb.max((measured - want).abs());
        assert!(
            (measured - want).abs() < 0.2,
            "mb lvl={lvl}: {measured:.3} vs {want:.3}"
        );
    }
    println!("multiband upper band static curve, 7 levels (4 kHz): worst error {worst_mb:.3} dB (limit 0.2)");
}

/// The 1 ms RMS envelope (dB) of the left channel.
fn env_db(x: &[f32], win: usize) -> Vec<f64> {
    x.chunks_exact(2)
        .map(|f| f[0])
        .collect::<Vec<_>>()
        .chunks(win)
        .map(|c| {
            let s: f64 = c.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / c.len() as f64;
            10.0 * s.max(1e-30).log10()
        })
        .collect()
}

#[test]
fn acceptance_2_attack_and_release_time_constants() {
    // A tone steps from -40 to -10 dBFS and back. Threshold -30, ratio 10, hard knee: both levels in the
    // straight part of the curve, so the reduction is a single exponential with the configured time constant.
    for (att, rel) in [(10.0, 100.0), (20.0, 250.0), (40.0, 500.0), (5.0, 60.0)] {
        let cfg = wb_cfg(-30.0, 10.0, 0.0, att, rel);
        let mut ch = build(&cfg);
        let mut x = sine(1000.0, -40.0, 1.0, 0.0);
        x.extend(sine(1000.0, -10.0, 2.0, 0.0));
        let tail = sine(1000.0, -40.0, 4.0, 0.0);
        let mut input = x.clone();
        input.extend(&tail);
        let mut out = input.clone();
        process_in_chunks(&mut ch, &mut out, 480);
        let win = 48; // 1 ms
        let ei = env_db(&input, win);
        let eo = env_db(&out, win);
        let gr: Vec<f64> = ei.iter().zip(&eo).map(|(a, b)| a - b).collect();
        // final reductions
        let g_hi = (0.9 * 1000.0 + 2000.0 * 0.9 / 2.0) as usize; // late in the loud stretch
        let g_hi = gr[g_hi.min(2900)..3000].iter().sum::<f64>() / (3000 - g_hi.min(2900)) as f64;
        // time for the reduction to reach 63.2 % of its final value after the step at 1000 ms
        let target = g_hi * (1.0 - (-1.0f64).exp());
        let t_att = gr[1000..3000].iter().position(|&g| g >= target).unwrap() as f64;
        // release: after 3000 ms the reduction falls from g_hi; time to 36.8 %
        let tgt_r = g_hi * (-1.0f64).exp();
        let t_rel = gr[3000..].iter().position(|&g| g <= tgt_r).unwrap() as f64;
        let ea = (t_att - att).abs() / att * 100.0;
        let er = (t_rel - rel).abs() / rel * 100.0;
        println!(
            "attack {att} ms: measured {t_att:.1} ms ({ea:.1} %); release {rel} ms: measured {t_rel:.1} ms ({er:.1} %) (limit 10 %); final reduction {g_hi:.2} dB"
        );
        assert!(ea < 10.0 && er < 10.0, "att {att}/{rel}");
    }
}

// ---- 3: the AGC ----------------------------------------------------------------------------------

fn agc_cfg(extra: Value) -> ChainConfig {
    let mut o = json!({"enabled": true, "target_dbfs": -20.0, "max_gain_db": 24.0, "max_reduction_db": 24.0,
        "down_db_per_s": 3.0, "up_db_per_s": 1.0, "fast_attack_ms": 0.0, "gate_db": -35.0});
    for (k, v) in extra.as_object().unwrap() {
        o[k] = v.clone();
    }
    cfg_with("bypass", &[("agc", o)])
}

/// Run `x` in 1 ms chunks, returning the AGC's gain (dB) after each.
fn gain_trace(cfg: &ChainConfig, x: &mut [f32]) -> Vec<f64> {
    let mut ch = build(cfg);
    let mut g = vec![];
    for c in x.chunks_mut(48 * 2) {
        ch.process(c, true);
        g.push(ch.agc_gain_db());
    }
    g
}

#[test]
fn acceptance_3_agc_slew_rates_and_settling() {
    // -30 -> -10 dBFS tone (K-weighted RMS -33 -> -13 dBFS... a 1 kHz sine of peak P reads P - 3.01 dB).
    let cfg = agc_cfg(json!({}));
    let mut x = sine(1000.0, -30.0, 25.0, 0.0);
    x.extend(sine(1000.0, -10.0, 14.0, 0.0));
    let g = gain_trace(&cfg, &mut x.clone());
    // Before the step the gain has settled at target - level = -20 - (-33.01) = +13.01 dB (up from 0 at 1 dB/s).
    let g_before = g[24_900];
    let want_before = -20.0 - (-30.0 - 3.0103);
    println!("settled before the step: {g_before:.3} dB (want {want_before:.3})");
    assert!((g_before - want_before).abs() < 0.1);
    // After the step: down at 3 dB/s to -20 - (-13.01) = -6.99 dB.
    let step = 25_000;
    let want_after = -20.0 - (-10.0 - 3.0103);
    let end = g[step + 13_900];
    // slope between 20 % and 80 % of the excursion
    let total = want_before - want_after;
    let i20 = (step..g.len())
        .find(|&i| g[i] <= want_before - 0.2 * total)
        .unwrap();
    let i80 = (step..g.len())
        .find(|&i| g[i] <= want_before - 0.8 * total)
        .unwrap();
    let slope = (g[i20] - g[i80]) / ((i80 - i20) as f64 / 1000.0);
    let e = (slope - 3.0).abs() / 3.0 * 100.0;
    println!("down step -30 to -10 dBFS: slope {slope:.3} dB/s (configured 3, {e:.1} %, limit 10 %); settled {end:.3} dB (want {want_after:.3})");
    assert!(e < 10.0 && (end - want_after).abs() < 0.1);
    // Level after settling is the target.
    let mut y = x.clone();
    let mut ch = build(&cfg);
    process_in_chunks(&mut ch, &mut y, 480);
    let total_s = 39.0;
    let lvl = rms_db(&y, total_s - 1.0, total_s) + 3.0103 - 3.0103; // RMS of the settled output
    println!("output RMS after settling: {lvl:.2} dBFS (target -20)");
    assert!((lvl - -20.0).abs() < 0.3, "{lvl}");
    // Up: -10 -> -30 at 1 dB/s
    let mut x2 = sine(1000.0, -10.0, 8.0, 0.0);
    x2.extend(sine(1000.0, -30.0, 30.0, 0.0));
    let g2 = gain_trace(&cfg, &mut x2);
    let (b2, a2) = (-20.0 - (-10.0 - 3.0103), -20.0 - (-30.0 - 3.0103));
    let st = 8_000;
    let tot = a2 - b2;
    let j20 = (st..g2.len()).find(|&i| g2[i] >= b2 + 0.2 * tot).unwrap();
    let j80 = (st..g2.len()).find(|&i| g2[i] >= b2 + 0.8 * tot).unwrap();
    let slope_up = (g2[j80] - g2[j20]) / ((j80 - j20) as f64 / 1000.0);
    let eu = (slope_up - 1.0).abs() * 100.0;
    println!(
        "up step -10 to -30 dBFS: slope {slope_up:.3} dB/s (configured 1, {eu:.1} %, limit 10 %)"
    );
    assert!(eu < 10.0);
}

#[test]
fn acceptance_3_the_gate_freezes_the_gain_in_silence() {
    let cfg = agc_cfg(json!({}));
    let mut x = sine(1000.0, -30.0, 8.0, 0.0);
    let after_tone = x.len() / 2;
    x.extend(vec![0.0f32; 2 * 30 * RATE as usize]);
    let g = gain_trace(&cfg, &mut x);
    let ms_tone = after_tone / 48;
    // From 1 s into the silence on (the 400 ms window has emptied) to its end.
    let (a, b) = (ms_tone + 1000, g.len() - 1);
    let (lo, hi) = g[a..b]
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    println!(
        "gain over 29 s of digital silence: from {lo:.4} to {hi:.4} dB, change {:.4} dB (limit 0.1)",
        hi - lo
    );
    assert!(hi - lo < 0.1);
    // Also a very quiet noise floor, -70 dBFS, under the gate (-55): the gain does not rise.
    let mut s = 12345u32;
    let noise: Vec<f32> = (0..2 * 30 * RATE as usize)
        .map(|_| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 8) as f32 / 8_388_608.0 - 1.0) * 10f32.powf(-70.0 / 20.0) * 1.7
        })
        .collect();
    let mut x = sine(1000.0, -30.0, 8.0, 0.0);
    let n0 = x.len() / 2 / 48;
    x.extend(noise);
    let g = gain_trace(&cfg, &mut x);
    let (lo, hi) = g[n0 + 1000..]
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    println!(
        "gain over 30 s of -70 dBFS room noise: change {:.4} dB (limit 0.1)",
        hi - lo
    );
    assert!(hi - lo < 0.1);
}

// ---- program material and the mixer rig ----------------------------------------------------------

use engine::feed::VoiceFeed;
use engine::mixer::{LoadParams, Mixer, MixerControl};
use engine::source::Source;

/// Pink-ish stereo noise (Paul Kellet's filter) with a slow wobble of +-`wobble_db`, scaled to `lufs`.
fn program(seconds: f64, lufs: f64, seed: u64, wobble_db: f32) -> Vec<f32> {
    let n = (seconds * R) as usize;
    let mut out = vec![0.0f32; n * 2];
    let mut s = seed | 1;
    for ch in 0..2 {
        let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
            (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
        for i in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let w = ((s >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.153_852;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let v = (b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362) * 0.11;
            b6 = w * 0.115926;
            let wob = 10f32.powf(
                wobble_db * (2.0 * std::f32::consts::PI * 0.3 * i as f32 / RATE as f32).sin()
                    / 20.0,
            );
            out[i * 2 + ch] = v * wob;
        }
    }
    let mut m = engine::loudness::BlockMeter::new(RATE, 3600.0);
    m.push(&out, true, true, 0);
    let l = m.gated().loudness.expect("audible");
    let g = 10f32.powf(((lufs - l) / 20.0) as f32);
    for v in out.iter_mut() {
        *v *= g;
    }
    out
}

/// A natural fade: linear in dB from 0 to -60 dB over the whole of `x`, then `silence_s` of digital silence.
fn faded(x: &[f32], silence_s: f64) -> Vec<f32> {
    let n = x.len() / 2;
    let mut out = Vec::with_capacity(x.len() + (silence_s * R) as usize * 2);
    for i in 0..n {
        let g = 10f32.powf(-60.0 * (i as f32 / n as f32) / 20.0);
        out.push(x[2 * i] * g);
        out.push(x[2 * i + 1] * g);
    }
    out.resize(out.len() + (silence_s * R) as usize * 2, 0.0);
    out
}

/// Interleaved levels (dBFS RMS, left+right) of consecutive windows of `win` frames.
fn levels(x: &[f32], win: usize) -> Vec<f64> {
    x.chunks(win * 2)
        .filter(|c| c.len() == win * 2)
        .map(|c| {
            10.0 * (c.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / c.len() as f64)
                .max(1e-14)
                .log10()
        })
        .collect()
}

/// An independent 4x-oversampled true peak (dBTP) of an interleaved stereo signal: a 96-tap Blackman-windowed
/// sinc, evaluated only around samples above 0.2 (a true peak never sits more than about 3 dB over the samples).
fn true_peak_db(x: &[f32]) -> f64 {
    let (taps, up) = (24usize, 4usize);
    let mut peak = 0.0f64;
    let n = x.len() / 2;
    for ch in 0..2 {
        for i in 0..n {
            let v = f64::from(x[i * 2 + ch]).abs();
            peak = peak.max(v);
            if v < 0.2 {
                continue;
            }
            for ph in 1..up {
                let frac = ph as f64 / up as f64;
                let mut acc = 0.0;
                for k in -(taps as i64 - 1)..=(taps as i64) {
                    let j = i as i64 + k;
                    if j < 0 || j >= n as i64 {
                        continue;
                    }
                    let t = k as f64 - frac;
                    let sinc = if t.abs() < 1e-12 {
                        1.0
                    } else {
                        (PI * t).sin() / (PI * t)
                    };
                    let w = 0.42
                        + 0.5 * (PI * t / taps as f64).cos()
                        + 0.08 * (2.0 * PI * t / taps as f64).cos();
                    acc += f64::from(x[j as usize * 2 + ch]) * sinc * w;
                }
                peak = peak.max(acc.abs());
            }
        }
    }
    20.0 * peak.max(1e-12).log10()
}

struct Trace {
    out: Vec<f32>,
    /// Per millisecond: the AGC gain, dB; the largest reduction of each band, dB.
    agc: Vec<f64>,
    band: Vec<[f32; 6]>,
}

/// Render the voices (start frame, samples) through a real mixer with `cfg` applied, 1 ms at a time.
fn mixer_render(cfg: &ChainConfig, voices: &[(u64, Vec<f32>)], total: u64) -> Trace {
    let (mut mixer, mut ctl): (Mixer, MixerControl) = Mixer::new(RATE);
    let chain = ctl.chain_control();
    chain.apply(cfg).unwrap();
    let mut keep = vec![];
    for (start, s) in voices {
        let feed = VoiceFeed::from_samples(s);
        ctl.load(
            feed,
            LoadParams {
                declick_in: false,
                ..LoadParams::at(*start)
            },
        )
        .unwrap();
        keep.push(s.len());
    }
    let mut out = vec![0.0f32; total as usize * 2];
    let (mut agc, mut band) = (vec![], vec![]);
    for c in out.chunks_mut(48 * 2) {
        mixer.render(c);
        let m = chain.meters.take();
        agc.push(f64::from(m.agc_gain_db));
        band.push(m.band_gr_db);
        ctl.pump();
    }
    Trace { out, agc, band }
}

/// A scenario of acceptance 13: song A (a `body_s` body at -18 LUFS, a 20 s natural fade, `tail_s` of silence)
/// and, `overlap_s` before A ends, song B 8 LU louder, with no envelopes.
fn scenario(
    body_s: f64,
    tail_s: f64,
    overlap_s: f64,
    wobble: f32,
) -> (Vec<(u64, Vec<f32>)>, u64, u64, u64) {
    let lead = (0.5 * R) as u64;
    let a_body = program(body_s + 20.0, -18.0, 11, wobble);
    let n_body = (body_s * R) as usize * 2;
    let mut a = a_body[..n_body].to_vec();
    a.extend(faded(&a_body[n_body..], tail_s));
    let a_frames = (a.len() / 2) as u64;
    let fade_start = lead + (body_s * R) as u64;
    let b_start = lead + a_frames - (overlap_s * R) as u64;
    let b = program(20.0, -10.0, 12, 0.0);
    let total = b_start + (b.len() / 2) as u64 + (0.5 * R) as u64;
    (vec![(lead, a), (b_start, b)], fade_start, b_start, total)
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

struct Transition {
    boost: f64,
    gate_change: f64,
    tp: f64,
    sample_peak: f64,
    agc_settle_ms: usize,
    mb_settle_ms: usize,
    band_settle_ms: usize,
    over: [f64; 4],
    steady_agc: f64,
}

fn transition_run(preset: &str) -> Transition {
    let (voices, fade_start, b_start, total) = scenario(30.0, 6.0, 1.5, 0.0);
    let on = trace_for(preset, &voices, total);
    let off = trace_for("bypass", &voices, total);
    // The gate closes where A's level reaches target + gate_db: the fade is 3 dB per second from -18 LUFS, and
    // the AGC's gate is 37 dB under that.
    let gate_close = fade_start as f64 / R + 37.0 / 3.0;
    let lv = |t: &Trace| levels(&t.out, 19_200);
    // (a) the last 8 s before the gate closes: mean of the 400 ms levels, processed against unprocessed.
    let w0 = ((gate_close - 8.0) / 0.4) as usize;
    let w1 = (gate_close / 0.4) as usize;
    let (la, lb) = (lv(&on), lv(&off));
    let boost = mean(&la[w0..w1]) - mean(&lb[w0..w1]);
    // (b) the gain inside the gated silence: from 1 s after the gate closes to B's onset - 0.1 s.
    let (g0, g1) = (
        ((gate_close + 1.0) * 1000.0) as usize,
        ((b_start as f64 / R - 0.1) * 1000.0) as usize,
    );
    let (lo, hi) = on.agc[g0..g1]
        .iter()
        .fold((f64::MAX, f64::MIN), |(l, h), &v| (l.min(v), h.max(v)));
    // (c) at B's onset.
    let on_ms = (b_start as f64 / R * 1000.0) as usize;
    let steady =
        |f: &dyn Fn(usize) -> f64| mean(&(on_ms + 2500..on_ms + 3500).map(f).collect::<Vec<_>>());
    let nb = on.band[0].len();
    let steady_agc = steady(&|i| on.agc[i]);
    let mb_mean = |i: usize| -> f64 {
        let n = on.band[i]
            .iter()
            .take(5)
            .filter(|v| **v > 0.0)
            .count()
            .max(1);
        f64::from(on.band[i].iter().take(5).sum::<f32>()) / n as f64
    };
    let _ = nb;
    let steady_mb = steady(&mb_mean);
    let settle = |f: &dyn Fn(usize) -> f64, steady: f64| -> usize {
        // The first millisecond after which the value stays within 1 dB of its steady value for 0.5 s.
        (0..3000)
            .find(|&k| (k..k + 500).all(|j| (f(on_ms + j) - steady).abs() < 1.0))
            .unwrap_or(9999)
    };
    let agc_settle_ms = settle(&|i| on.agc[i], steady_agc);
    // The multiband's summary (mean of the band reductions) read through a 20 ms moving mean.
    let mb_smooth = |i: usize| -> f64 {
        mean(
            &(i.saturating_sub(10)..=i + 10)
                .map(&mb_mean)
                .collect::<Vec<_>>(),
        )
    };
    let mb_settle_ms = settle(&mb_smooth, steady_mb);
    // Each band alone, 50 ms moving mean, for the record.
    let mut band_settle_ms = 0;
    for b in 0..on.band[0].iter().filter(|_| true).count().min(6) {
        let bf = |i: usize| -> f64 {
            mean(
                &(i.saturating_sub(25)..=i + 25)
                    .map(|j| f64::from(on.band[j][b]))
                    .collect::<Vec<_>>(),
            )
        };
        let st = steady(&|i| f64::from(on.band[i][b]));
        if st > 0.0 || on.band[on_ms][b] > 0.0 {
            band_settle_ms = band_settle_ms.max(settle(&bf, st));
        }
    }
    let b_samples = &on.out[b_start as usize * 2..(b_start as usize + 96_000) * 2];
    let tp = true_peak_db(b_samples);
    let sample_peak =
        20.0 * f64::from(b_samples.iter().fold(0.0f32, |m, v| m.max(v.abs()))).log10();
    let steady_lvl = mean(&levels(
        &on.out[(b_start as usize + 120_000) * 2..(b_start as usize + 168_000) * 2],
        19_200,
    ));
    let slide = |win: usize| -> f64 {
        let x = &on.out[b_start as usize * 2..(b_start as usize + 48_000) * 2];
        levels(x, win).iter().cloned().fold(f64::MIN, f64::max)
    };
    let over = [
        slide(19_200) - steady_lvl,
        slide(4_800) - steady_lvl,
        slide(2_400) - steady_lvl,
        slide(960) - steady_lvl,
    ];
    Transition {
        boost,
        gate_change: hi - lo,
        tp,
        sample_peak,
        agc_settle_ms,
        mb_settle_ms,
        band_settle_ms,
        over,
        steady_agc,
    }
}

fn trace_for(preset: &str, voices: &[(u64, Vec<f32>)], total: u64) -> Trace {
    let cfg = preset_config(preset).unwrap();
    mixer_render(&cfg, voices, total)
}

#[test]
fn acceptance_13_transition_behavior() {
    // radio: (a) >= 6 dB, (b) < 0.1 dB, (c) true peak <= -1.0 dBTP, AGC and multiband settle within 150 ms,
    // no upward overshoot of the level beyond 1 dB.
    let r = transition_run("radio");
    println!(
        "radio: (a) the last 8 s of the fade are {:.2} dB louder than unprocessed (limit 6); (b) AGC gain in 4.5 s or more of gated silence changes {:.4} dB (limit 0.1); \
(c) B's onset: true peak {:.2} dBTP (limit -1.0; sample peak {:.2} dBFS), AGC settles within 1 dB in {} ms (limit 150), multiband mean reduction in {} ms (limit 150; the worst single band {} ms), \
output over B's steady level: 400 ms levels {:.2} dB (limit 1), 100 ms {:.2}, 50 ms {:.2}, 20 ms {:.2}; steady AGC gain {:.2} dB",
        r.boost, r.gate_change, r.tp, r.sample_peak, r.agc_settle_ms, r.mb_settle_ms, r.band_settle_ms, r.over[0], r.over[1], r.over[2], r.over[3], r.steady_agc
    );
    assert!(r.boost >= 6.0 && r.gate_change < 0.1 && r.tp <= -1.0);
    assert!(r.agc_settle_ms <= 150 && r.mb_settle_ms <= 150 && r.over[0] <= 1.0);
    // gentle: (a) >= 3 dB, settle within 400 ms.
    let g = transition_run("gentle");
    println!(
        "gentle: (a) {:.2} dB (limit 3); (b) {:.4} dB (limit 0.1); (c) true peak {:.2} dBTP, AGC settles in {} ms (limit 400), multiband in {} ms (band worst {}), overshoot of the 400 ms levels {:.2} dB (limit 1)",
        g.boost, g.gate_change, g.tp, g.agc_settle_ms, g.mb_settle_ms, g.band_settle_ms, g.over[0]
    );
    assert!(g.boost >= 3.0 && g.gate_change < 0.1 && g.tp <= -1.0);
    assert!(g.agc_settle_ms <= 400 && g.mb_settle_ms <= 400 && g.over[0] <= 1.0);
    // bypass: (a) and (b) read 0 dB; (c) limited only by the safety limiter (a -1.0 dBFS sample ceiling).
    let b = transition_run("bypass");
    println!(
        "bypass: (a) {:.2} dB, (b) {:.4} dB; (c) sample peak {:.2} dBFS (safety limiter ceiling -1.0), true peak {:.2} dBTP",
        b.boost, b.gate_change, b.sample_peak, b.tp
    );
    assert!(b.boost.abs() < 0.01 && b.gate_change < 0.001 && b.sample_peak <= -0.999);
}

// ---- 13b: an onset after a short gap (a playlist switch fades the old item out and starts the new one) ---------------

#[test]
fn agc_onset_after_a_fade_and_a_short_gap_does_not_burst() {
    // The Eve's chain is `radio` with a few overrides. A playlist switch fades the old item over 2 s and the next one
    // starts after about 150 ms of silence, inside the AGC's 400 ms fast window: the fade's tail must not make the
    // fast level read the onset as quiet. Before the fix the clamp that takes the gain down from its ride up the
    // fade came 40 ms late (the output sat 9 dB over its steady level in between); after a gap longer than the window
    // it came at once.
    let cfg = preset_config("radio").unwrap();
    let mut worst_lag = 0usize;
    let mut worst_excess = f64::MIN;
    for gap in [0.05, 0.08, 0.15, 0.3, 1.0] {
        let lead = (0.5 * R) as u64;
        let mut a = program(22.0, -18.0, 21, 0.0);
        let n_body = (20.0 * R) as usize;
        let n_fade = (2.0 * R) as usize;
        for i in 0..n_fade {
            let g = 1.0 - i as f32 / n_fade as f32;
            a[(n_body + i) * 2] *= g;
            a[(n_body + i) * 2 + 1] *= g;
        }
        let a_frames = (a.len() / 2) as u64;
        let b_start = lead + a_frames + (gap * R) as u64;
        let b = program(8.0, -10.0, 22, 0.0);
        let total = b_start + (b.len() / 2) as u64;
        let t = mixer_render(&cfg, &[(lead, a), (b_start, b)], total);
        let on_ms = (b_start as f64 / R * 1000.0) as usize;
        let before = t.agc[on_ms - 20];
        let steady = t.agc[on_ms + 600];
        // Milliseconds after the onset until the gain is within 1 dB of where it settles.
        let lag = (0..300)
            .find(|&k| (k..k + 100).all(|j| (t.agc[on_ms + j] - steady).abs() < 1.0))
            .unwrap_or(999);
        // The loudest 10 ms in the first 100 ms against the median of the 10 ms levels from 1 to 2 s.
        let lv = levels(
            &t.out[b_start as usize * 2..(b_start as usize + 96_000) * 2],
            480,
        );
        let mut st: Vec<f64> = lv[100..200].to_vec();
        st.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let excess = lv[2..10].iter().cloned().fold(f64::MIN, f64::max) - st[st.len() / 2];
        println!(
            "radio, gap {gap:.2} s: AGC gain {before:.2} dB before B, settles at {steady:.2} dB, within 1 dB after {lag} ms (limit 25); loudest 10 ms between 20 and 100 ms over B's steady level: {excess:.2} dB"
        );
        worst_lag = worst_lag.max(lag);
        worst_excess = worst_excess.max(excess);
    }
    println!("worst: AGC settle {worst_lag} ms (limit 25), 20 to 100 ms excess {worst_excess:.2} dB (limit 4)");
    assert!(worst_lag <= 25);
    assert!(worst_excess <= 4.0);
}

// ---- 4: the limiter -----------------------------------------------------------------------------------------------

/// Pink-ish noise at `rms_dbfs`, streamed in blocks (Paul Kellet's filter, one per channel).
struct PinkStream {
    st: [[f32; 7]; 2],
    seed: u64,
    gain: f32,
}

impl PinkStream {
    fn new(rms_dbfs: f64, seed: u64) -> Self {
        let mut p = PinkStream {
            st: [[0.0; 7]; 2],
            seed: seed | 1,
            gain: 1.0,
        };
        // Calibrate on 10 s.
        let mut ms = 0.0f64;
        let mut n = 0usize;
        let mut b = vec![0.0f32; 4096];
        for _ in 0..(10 * 48_000 / 2048) {
            p.fill(&mut b);
            ms += b.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
            n += b.len();
        }
        let rms = (ms / n as f64).sqrt();
        p.st = [[0.0; 7]; 2];
        p.gain = (10f64.powf(rms_dbfs / 20.0) / rms) as f32;
        p
    }

    fn fill(&mut self, out: &mut [f32]) {
        for fr in out.chunks_exact_mut(2) {
            for ch in 0..2 {
                self.seed = self
                    .seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let w = ((self.seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                let b = &mut self.st[ch];
                b[0] = 0.99886 * b[0] + w * 0.0555179;
                b[1] = 0.99332 * b[1] + w * 0.0750759;
                b[2] = 0.96900 * b[2] + w * 0.153_852;
                b[3] = 0.86650 * b[3] + w * 0.3104856;
                b[4] = 0.55000 * b[4] + w * 0.5329522;
                b[5] = -0.7616 * b[5] - w * 0.0168980;
                let v = (b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362) * 0.11;
                b[6] = w * 0.115926;
                fr[ch] = v * self.gain;
            }
        }
    }
}

/// A streaming independent 4x true-peak meter (the 48-tap windowed sinc of `true_peak_db`), keeping 24 frames of
/// history between blocks.
struct TpMeter {
    hist: Vec<f32>,
    peak: f64,
    sample_peak: f32,
}

impl TpMeter {
    fn new() -> Self {
        TpMeter {
            hist: vec![0.0; 2 * 24],
            peak: 0.0,
            sample_peak: 0.0,
        }
    }

    fn push(&mut self, block: &[f32]) {
        let taps = 24usize;
        let mut x = std::mem::take(&mut self.hist);
        let lead = x.len() / 2;
        x.extend_from_slice(block);
        let n = x.len() / 2;
        // Frames whose neighbours on both sides are inside `x`; the rest are handled with the next block.
        for ch in 0..2 {
            for i in lead.saturating_sub(taps)..n.saturating_sub(taps) {
                let v = f64::from(x[i * 2 + ch]).abs();
                self.peak = self.peak.max(v);
                if i >= lead {
                    self.sample_peak = self.sample_peak.max(x[i * 2 + ch].abs());
                }
                if v < 0.2 {
                    continue;
                }
                for ph in 1..4 {
                    let frac = f64::from(ph) / 4.0;
                    let mut acc = 0.0;
                    for k in -(taps as i64 - 1)..=(taps as i64) {
                        let j = i as i64 + k;
                        if j < 0 || j >= n as i64 {
                            continue;
                        }
                        let t = k as f64 - frac;
                        let sinc = (PI * t).sin() / (PI * t);
                        let w = 0.42
                            + 0.5 * (PI * t / taps as f64).cos()
                            + 0.08 * (2.0 * PI * t / taps as f64).cos();
                        acc += f64::from(x[j as usize * 2 + ch]) * sinc * w;
                    }
                    self.peak = self.peak.max(acc.abs());
                }
            }
        }
        self.hist = x[(n - 2 * taps) * 2..].to_vec();
    }
}

/// Five minutes in the suite; the milestone's hour is `acceptance_4_one_hour_of_pink_noise` (ignored: about 8 minutes).
#[test]
fn acceptance_4_the_limiter_holds_the_ceiling_on_pink_noise_12_db_over() {
    limiter_run(5.0);
}

/// `cargo test -p engine --test chain_m6 -- --ignored acceptance_4`
#[test]
#[ignore]
fn acceptance_4_one_hour_of_pink_noise() {
    limiter_run(60.0);
}

fn limiter_run(minutes: f64) {
    use engine::limiter::Limiter;
    use engine::tplimiter::TpLimiter;
    let ceiling = -1.0;
    // The chain's limiter block at -1.0 dBTP, then the safety limiter (-1.0 dBFS), as in the mixer.
    let mut tp = TpLimiter::new(RATE, ceiling as f32);
    let mut safety = Limiter::new(RATE);
    // +12 dB over the ceiling: pink noise whose RMS is the ceiling plus 12 dB (peaks 12 dB higher again).
    let mut noise = PinkStream::new(ceiling + 12.0, 99);
    let mut meter = TpMeter::new();
    let mut a = vec![0.0f32; 2048];
    let mut b = vec![0.0f32; 2048];
    let mut c = vec![0.0f32; 2048];
    let blocks = (minutes * 60.0 * R / 1024.0) as usize;
    let (mut max_in, mut max_gr) = (0.0f32, 1.0f32);
    let t = std::time::Instant::now();
    for k in 0..blocks {
        noise.fill(&mut a);
        max_in = max_in.max(a.iter().fold(0.0f32, |m, v| m.max(v.abs())));
        tp.process(&a, &mut b);
        max_gr = max_gr.min(tp.take_min_gain());
        safety.process(&b, &mut c);
        // The first 20 ms are the delay lines filling.
        if k > 4 {
            meter.push(&c);
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let tp_db = 20.0 * meter.peak.max(1e-9).log10();
    let sp_db = 20.0 * f64::from(meter.sample_peak).log10();
    println!(
        "{minutes} min of pink noise at {:+.1} dBFS RMS (peaks {:+.1} dBFS, {:.0} dB over the -1.0 ceiling) through the limiter and the safety limiter: \
         largest 4x true peak {tp_db:.3} dBTP (limit -1.0), largest sample {sp_db:.3} dBFS (limit -1.0); deepest limiter reduction {:.1} dB; {secs:.1} s of CPU for {:.0} s of audio",
        ceiling + 12.0,
        20.0 * f64::from(max_in).log10(),
        20.0 * f64::from(max_in).log10() - ceiling,
        -20.0 * f64::from(max_gr).log10(),
        minutes * 60.0
    );
    assert!(tp_db <= -1.0 && sp_db <= -1.0);
}

// ---- hot swap -------------------------------------------------------------------------------------------------------

/// Render `voices` through a real mixer, applying `swaps` (frame, config) from the control side when the render
/// reaches them, 1 ms at a time. The first config is applied before the first block.
fn render_swapping(
    first: &ChainConfig,
    swaps: &[(usize, ChainConfig)],
    voices: &[(u64, Vec<f32>)],
    total: usize,
) -> Trace {
    let (mut mixer, mut ctl): (Mixer, MixerControl) = Mixer::new(RATE);
    let chain = ctl.chain_control();
    chain.apply(first).unwrap();
    for (start, s) in voices {
        ctl.load(
            VoiceFeed::from_samples(s),
            LoadParams {
                declick_in: false,
                ..LoadParams::at(*start)
            },
        )
        .unwrap();
    }
    let mut out = vec![0.0f32; total * 2];
    let (mut agc, mut band) = (vec![], vec![]);
    let mut at = 0usize;
    let mut next = 0usize;
    for c in out.chunks_mut(48 * 2) {
        if next < swaps.len() && at >= swaps[next].0 {
            chain.apply(&swaps[next].1).unwrap();
            next += 1;
        }
        mixer.render(c);
        at += 48;
        let m = chain.meters.take();
        agc.push(f64::from(m.agc_gain_db));
        band.push(m.band_gr_db);
        ctl.pump();
    }
    Trace { out, agc, band }
}

#[test]
fn hot_swap_is_click_free_and_carries_the_state() {
    let radio = preset_config("radio").unwrap();
    let gentle = preset_config("gentle").unwrap();
    let bypass = preset_config("bypass").unwrap();
    let mut all = radio.clone();
    all.apply_block(
        "multiband",
        json!({"threshold_dbfs": [-30.0, -28.0, -26.0, -26.0, -28.0]})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    // A 1 kHz sine at -12 dBFS (the steps of a sine are known: A 2 pi f / fs at most), 22 s.
    let x = sine(1000.0, -12.0, 22.0, 0.0);
    let swaps = [
        (4 * 48_000usize, bypass.clone()),
        (8 * 48_000, radio.clone()),
        (12 * 48_000, gentle.clone()),
        (16 * 48_000, radio.clone()),
        (20 * 48_000, all),
    ];
    let t = render_swapping(&radio, &swaps, &[(0, x)], 22 * 48_000);
    let step = |lo: usize, hi: usize| -> f64 {
        (lo + 1..hi)
            .map(|i| f64::from((t.out[i * 2] - t.out[(i - 1) * 2]).abs()))
            .fold(0.0, f64::max)
    };
    let amp = |lo: usize, hi: usize| -> f64 {
        t.out[lo * 2..hi * 2]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
            .into()
    };
    let mut worst = 0.0f64;
    let mut line = vec![];
    for (k, (at, _)) in swaps.iter().enumerate() {
        // The swap window: from 10 ms before the request to 150 ms after (the crossfade is 50 ms, the AGC and the
        // bands then move at their own, slow, rates).
        let (w0, w1) = (at - 480 + 336, at + 7200 + 336);
        let (before, after) = (amp(at - 4800, at - 960), amp(at + 36_000, at + 40_000));
        let allowed = before.max(after) * 2.0 * PI * 1000.0 / R;
        let got = step(w0, w1);
        worst = worst.max(got / allowed);
        line.push(format!("swap {k} at {} s: largest sample step {got:.5} (a sine of the louder level steps {allowed:.5}), ratio {:.3}", at / 48_000, got / allowed));
    }
    println!(
        "{}; worst ratio {worst:.3} (limit 1.05: no click)",
        line.join("; ")
    );
    assert!(worst < 1.05);
    // The state is carried: radio -> a radio with changed band thresholds at 20 s moves the AGC's gain by nothing.
    let ms = 20_000usize;
    let (g0, g1) = (t.agc[ms - 20], t.agc[ms + 200]);
    println!("AGC gain across a swap that keeps the AGC and changes the bands: {g0:.3} dB before, {g1:.3} dB 200 ms after (change {:.3} dB, limit 0.2: not restarted)", (g1 - g0).abs());
    assert!((g1 - g0).abs() < 0.2);
}

// ---- CPU ---------------------------------------------------------------------------------------------------------------

#[test]
fn cpu_of_the_radio_chain() {
    // The chain's own cost on this machine: 60 s of pink noise through `radio` (and with every block on), timed.
    // This build is the test profile (opt-level 1 for the workspace's crates); the release figure is in BUILDLOG.
    let mut all = preset_config("radio").unwrap();
    for b in [
        "highpass", "rotator", "stereo", "bass", "wideband", "clipper",
    ] {
        all.apply_block(b, json!({"enabled": true}).as_object().unwrap())
            .unwrap();
    }
    all.apply_block(
        "eq",
        json!({"low_db": 3.0, "mid2_db": 2.0}).as_object().unwrap(),
    )
    .unwrap();
    let mut results = vec![];
    for (name, cfg) in [
        ("radio", preset_config("radio").unwrap()),
        ("gentle", preset_config("gentle").unwrap()),
        ("every block on", all),
    ] {
        let mut ch = build(&cfg);
        let mut noise = PinkStream::new(-14.0, 5);
        let mut buf = vec![0.0f32; 1024 * 2];
        let secs_of_audio = 60.0;
        let t = std::time::Instant::now();
        for _ in 0..(secs_of_audio * R / 1024.0) as usize {
            noise.fill(&mut buf);
            ch.process(&mut buf, true);
        }
        let wall = t.elapsed().as_secs_f64();
        println!("{name}: {wall:.3} s of CPU for {secs_of_audio} s of audio = {:.2} % of one core on this machine", wall / secs_of_audio * 100.0);
        results.push((name, wall / secs_of_audio));
    }
    let _ = results;
}

// ---- the output makeup -------------------------------------------------------------------------

/// The makeup is a plain gain at rest, glides at `MAKEUP_SLEW_DB_PER_S` to a new value when a chain is started
/// from an old one (`adopt`), and cancels a loudness-controller gain that glides down at the same rate.
#[test]
fn the_makeup_glides_at_three_db_a_second_and_cancels_the_controllers_shift() {
    use engine::loudness::{Controller, Params};
    // At rest: 3.1 dB exactly, before the clipper (a -3 dBFS tone passes a -1.5 dBFS clipper at +3.1 dB: +0.1
    // over the threshold is the clipper's, so use a quieter tone here).
    let mut rest = build(&cfg_with("bypass", &[("makeup", json!({"gain_db": 3.1}))]));
    assert_eq!(rest.makeup_db(), 3.1);
    let mut x = sine(1000.0, -20.0, 0.5, 0.0);
    process_in_chunks(&mut rest, &mut x, 480);
    let at_rest = rms_db(&x, 0.1, 0.4) - (-20.0 - 3.0103);
    println!("makeup 3.1 dB at rest: the tone reads {at_rest:+.4} dB");
    assert!((at_rest - 3.1).abs() < 0.001);
    // Gliding: a chain started from a 0 dB one, going to 3.1 dB.
    let old = build(&ChainConfig::default());
    let mut new = build(&cfg_with("bypass", &[("makeup", json!({"gain_db": 3.1}))]));
    new.adopt(&old);
    assert_eq!(new.makeup_db(), 0.0, "adopt takes the old makeup");
    // The controller's side: on target at +3.1, shifting by the same 3.1 dB, ticked per 10 ms chunk.
    let mut ctl = Controller::new(
        RATE,
        Params {
            enabled: true,
            ..Params::default()
        },
        3.1,
    );
    ctl.begin_shift(3.1);
    let n = 480;
    let tone = sine(1000.0, -20.0, 2.0, 0.0);
    let mut worst: f64 = 0.0;
    let mut done_at = None;
    let before = ctl.gain_db();
    for (i, c) in tone.chunks(n * 2).enumerate() {
        let mut y = c.to_vec();
        new.process(&mut y, true);
        let g0 = ctl.gain_db();
        ctl.tick_shift(n as f64 / R);
        let g = (g0 + ctl.gain_db()) / 2.0;
        let lvl = |v: &[f32]| {
            10.0 * (v
                .chunks_exact(2)
                .map(|f| f64::from(f[0]).powi(2))
                .sum::<f64>()
                / (v.len() / 2) as f64)
                .log10()
        };
        // The level on air relative to the tone, minus where it stood before the move (the gain's 3.1 dB).
        let total = lvl(&y) + g - lvl(c) - before;
        worst = worst.max(total.abs());
        if done_at.is_none() && !ctl.shifting() {
            done_at = Some((i + 1) as f64 * n as f64 / R);
        }
    }
    println!(
        "gain {before:+.2} dB -> {:+.4} dB and makeup 0 -> {:.2} dB in {:.2} s; chunk-by-chunk the program level moved at most {worst:.4} dB (limit 0.05)",
        ctl.gain_db(),
        new.makeup_db(),
        done_at.unwrap()
    );
    assert!((done_at.unwrap() - 3.1 / 3.0).abs() < 0.02);
    assert_eq!(new.makeup_db(), 3.1);
    assert!(ctl.gain_db().abs() < 1e-9);
    assert!(worst < 0.05, "the level moved {worst} dB");
}

// ---- the AGC's state, for its card's pill -----------------------------------------------------

/// A steady program, a fade out and a loud onset: the pill reads riding, then riding a fade (the gain rises while the
/// fast level falls under the program level), then clamping (the fast attack on the onset), then riding again.
#[test]
fn the_agc_state_passes_riding_a_fade_then_clamping_then_riding() {
    use engine::chain::agc::AgcState;
    let cfg = agc_cfg(
        json!({"up_db_per_s": 4.0, "fast_attack_ms": 20.0, "max_gain_db": 12.0, "max_reduction_db": 12.0}),
    );
    let mut ch = build(&cfg);
    // 8 s at -20 dBFS, a 6 s fade to -38 dBFS, 3 s there, then a jump to -8 dBFS for 6 s.
    let mut x: Vec<f32> = vec![];
    let tone = |db: f64, secs: f64, x: &mut Vec<f32>| {
        let n0 = x.len() / 2;
        let a = 10f64.powf(db / 20.0);
        for i in 0..(secs * R) as usize {
            let v = (a * (2.0 * PI * 997.0 * (n0 + i) as f64 / R).sin()) as f32;
            x.push(v);
            x.push(v);
        }
    };
    tone(-20.0, 8.0, &mut x);
    let n0 = x.len() / 2;
    for i in 0..(6.0 * R) as usize {
        let db = -20.0 - 18.0 * i as f64 / (6.0 * R);
        let v = (10f64.powf(db / 20.0) * (2.0 * PI * 997.0 * (n0 + i) as f64 / R).sin()) as f32;
        x.push(v);
        x.push(v);
    }
    tone(-38.0, 3.0, &mut x);
    tone(-8.0, 6.0, &mut x);
    let mut seq: Vec<(f64, AgcState)> = vec![];
    for (i, c) in x.chunks_mut(48 * 2).enumerate() {
        ch.process(c, true);
        let s = ch.agc_state();
        if seq.last().map(|l| l.1) != Some(s) {
            seq.push((i as f64 * 0.001, s));
        }
    }
    println!("AGC state changes (s, state): {seq:?}");
    let names: Vec<AgcState> = seq.iter().map(|s| s.1).collect();
    let pos = |from: usize, s: AgcState| {
        names
            .iter()
            .skip(from)
            .position(|x| *x == s)
            .map(|p| p + from)
    };
    let fade = pos(0, AgcState::RidingFade).expect("no fade seen");
    let clamp = pos(fade, AgcState::Clamping).expect("no clamp after the fade");
    let after = pos(clamp, AgcState::Riding).expect("no riding after the clamp");
    assert!(
        seq[fade].0 > 8.0 && seq[fade].0 < 14.5,
        "fade at {}",
        seq[fade].0
    );
    assert!(
        seq[clamp].0 > 17.0 && seq[clamp].0 < 17.2,
        "clamp at {}",
        seq[clamp].0
    );
    assert!(seq[after].0 > seq[clamp].0);
}

/// A 997 Hz tone whose level (dBFS peak) follows `db(t)`, `secs` long.
fn tone_env(secs: f64, db: impl Fn(f64) -> f64) -> Vec<f32> {
    (0..(secs * R) as usize)
        .flat_map(|i| {
            let t = i as f64 / R;
            let v = (10f64.powf(db(t) / 20.0) * (2.0 * PI * 997.0 * t).sin()) as f32;
            [v, v]
        })
        .collect()
}

/// Run `x` through the `radio` chain in 256-frame blocks, reading the card's state every 250 ms the way
/// `GET /api/v1/chain/agc` does. Returns (time, state the card shows, state the AGC is in at that moment).
fn agc_polls(
    x: &mut [f32],
) -> Vec<(
    f64,
    engine::chain::agc::AgcState,
    engine::chain::agc::AgcState,
)> {
    let meters = Arc::new(ChainMeters::default());
    let mut ch = Chain::build(&preset_config("radio").unwrap(), RATE, meters.clone());
    let (mut done, mut next, mut out) = (0usize, 12_000usize, vec![]);
    for c in x.chunks_mut(256 * 2) {
        ch.process(c, true);
        done += c.len() / 2;
        if done >= next {
            next += 12_000;
            out.push((done as f64 / R, meters.agc_view().state, ch.agc_state()));
        }
    }
    out
}

/// A real fade (-20 dB over 3 s, linear in dB, after 8 s of steady program) reads FADE on the card while it fades.
#[test]
fn a_real_fade_reads_fade() {
    use engine::chain::agc::AgcState;
    let mut x = tone_env(13.0, |t| {
        if t < 8.0 {
            -14.0
        } else if t < 11.0 {
            -14.0 - 20.0 * (t - 8.0) / 3.0
        } else {
            -34.0
        }
    });
    let p = agc_polls(&mut x);
    let fade: Vec<f64> = p
        .iter()
        .filter(|s| s.1 == AgcState::RidingFade)
        .map(|s| s.0)
        .collect();
    println!("card reads FADE at {fade:?}");
    assert!(
        p.iter().all(|s| s.0 >= 8.0 || s.1 != AgcState::RidingFade),
        "FADE before the fade"
    );
    let during = p
        .iter()
        .filter(|s| s.0 > 10.25 && s.0 <= 11.0)
        .all(|s| s.1 == AgcState::RidingFade);
    assert!(
        during && !fade.is_empty() && fade[0] < 10.3,
        "first FADE at {:?}",
        fade.first()
    );
}

/// Music-like level steps (+-4 dB every 0.5 to 2 s: a level that moves between -22, -18 and -14 dBFS, and one that
/// alternates up and down) never read FADE, on the card or in the AGC itself.
#[test]
fn music_like_level_steps_never_read_fade() {
    use engine::chain::agc::AgcState;
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for walk in [true, false] {
        // (start s, level dBFS)
        let (mut t, mut lv, mut up, mut steps) = (0.0, -18.0, true, vec![]);
        while t < 120.0 {
            steps.push((t, lv));
            t += 0.5 + 1.5 * rnd();
            lv = if walk {
                let d = if rnd() < 0.5 { 4.0 } else { -4.0 };
                if (-22.0..=-14.0).contains(&(lv + d)) {
                    lv + d
                } else {
                    lv - d
                }
            } else {
                up = !up;
                if up {
                    lv + 4.0
                } else {
                    lv - 4.0
                }
            };
        }
        let mut x = tone_env(120.0, |t| {
            steps.iter().rev().find(|s| s.0 <= t).map_or(-18.0, |s| s.1)
        });
        let p = agc_polls(&mut x);
        let card = p.iter().filter(|s| s.1 == AgcState::RidingFade).count();
        let agc = p.iter().filter(|s| s.2 == AgcState::RidingFade).count();
        let riding = p.iter().filter(|s| s.1 == AgcState::Riding).count();
        println!(
            "steps ({}): {} polls, FADE on the card {card}, in the AGC {agc}, RIDING {riding}",
            if walk { "random walk" } else { "alternating" },
            p.len()
        );
        assert_eq!(card + agc, 0);
    }
}

/// A quieter passage (8 dB down for 6 s) is ridden up and reads RIDING, not FADE; a 100 ms dip under the gate
/// inside a poll does not make the card read GATED.
#[test]
fn a_quiet_passage_ridden_up_reads_riding() {
    use engine::chain::agc::AgcState;
    let mut x = tone_env(20.0, |t| {
        if (8.0..14.0).contains(&t) {
            -26.0
        } else if (17.05..17.15).contains(&t) {
            -80.0
        } else {
            -18.0
        }
    });
    let p = agc_polls(&mut x);
    let q: Vec<_> = p.iter().filter(|s| s.0 > 8.5 && s.0 <= 14.0).collect();
    println!(
        "quiet passage: {:?}",
        q.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>()
    );
    assert!(q.iter().all(|s| s.1 == AgcState::Riding));
    let dip: Vec<_> = p.iter().filter(|s| s.0 > 16.9 && s.0 <= 17.6).collect();
    println!(
        "dip: {:?}",
        dip.iter().map(|s| (s.0, s.1)).collect::<Vec<_>>()
    );
    assert!(dip.iter().all(|s| s.1 != AgcState::Gated));
}
