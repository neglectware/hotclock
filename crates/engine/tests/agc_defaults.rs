//! The AGC at its defaults is bit for bit what it was before the fast clamp switch, the recovery hold and the ratio
//! were added (2026-10-12). The hashes below were taken from the build before that change, on the same signal: a
//! quiet passage, a loud onset, silence, snare hits over a quiet bed, a fade out and a loud start again. A stored
//! "fast attack 0" (the clamp off, before the switch existed) is read as the switch off and sounds the same.

use common::chain::{preset_config, ChainConfig};
use engine::chain::{Chain, ChainMeters};
use serde_json::{json, Value};
use std::f64::consts::PI;
use std::sync::Arc;

const RATE: u32 = 48_000;
const R: f64 = 48_000.0;

/// A program of a few tones (so that the K-weighting and every band see it), stereo, `db` dBFS for each tone.
fn tones(out: &mut Vec<f32>, secs: f64, db: f64, fade_to_db: Option<f64>) {
    let n = (secs * R) as usize;
    let start = out.len() / 2;
    for i in 0..n {
        let t = (start + i) as f64 / R;
        let lvl = match fade_to_db {
            Some(end) => db + (end - db) * i as f64 / n as f64,
            None => db,
        };
        let a = 10f64.powf(lvl / 20.0);
        let l = a
            * ((2.0 * PI * 110.0 * t).sin()
                + 0.6 * (2.0 * PI * 1000.0 * t).sin()
                + 0.3 * (2.0 * PI * 5300.0 * t).sin())
            / 1.9;
        let r = a
            * ((2.0 * PI * 165.0 * t).sin()
                + 0.6 * (2.0 * PI * 1200.0 * t + 0.4).sin()
                + 0.3 * (2.0 * PI * 4700.0 * t).sin())
            / 1.9;
        out.push(l as f32);
        out.push(r as f32);
    }
}

fn silence(out: &mut Vec<f32>, secs: f64) {
    out.extend(std::iter::repeat_n(0.0f32, 2 * (secs * R) as usize));
}

/// A quiet bed with a snare-like hit (30 ms of a loud burst) every half second.
fn snares(out: &mut Vec<f32>, secs: f64) {
    let hits = (secs / 0.5) as usize;
    for _ in 0..hits {
        tones(out, 0.03, -4.0, Some(-30.0));
        tones(out, 0.47, -38.0, None);
    }
}

fn program() -> Vec<f32> {
    let mut x = Vec::new();
    tones(&mut x, 4.0, -32.0, None); // a quiet passage: the gain rises
    tones(&mut x, 3.0, -8.0, None); // a loud onset: the fast clamp
    silence(&mut x, 1.0); // the gate holds
    snares(&mut x, 4.0); // hits after a quiet stretch
    tones(&mut x, 4.0, -18.0, Some(-50.0)); // a fade out, ridden up
    silence(&mut x, 0.5);
    tones(&mut x, 3.0, -6.0, None); // a loud start after the gap
    x
}

fn cfg(base: &str, edits: &[(&str, Value)]) -> ChainConfig {
    let mut c = preset_config(base).unwrap();
    for (b, v) in edits {
        c.apply_block(b, v.as_object().unwrap()).unwrap();
    }
    c
}

/// FNV-1a over the output's bits.
fn hash(c: &ChainConfig) -> u64 {
    let mut ch = Chain::build(c, RATE, Arc::new(ChainMeters::default()));
    let mut x = program();
    for part in x.chunks_mut(960) {
        ch.process(part, false);
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in &x {
        for b in s.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn cases() -> Vec<(&'static str, ChainConfig)> {
    vec![
        (
            "agc alone",
            cfg("bypass", &[("agc", json!({"enabled": true}))]),
        ),
        ("radio", cfg("radio", &[])),
        ("gentle", cfg("gentle", &[])),
        (
            "agc, fast attack 0",
            cfg(
                "bypass",
                &[("agc", json!({"enabled": true, "fast_attack_ms": 0.0}))],
            ),
        ),
    ]
}

/// Taken from the build before the change (commit d2d20ff).
const BEFORE: [(&str, u64); 4] = [
    ("agc alone", 0x5e01c2f6b89c0e08),
    ("radio", 0x64b9cee4f52bd9e8),
    ("gentle", 0x6e153dab9eb4e33d),
    ("agc, fast attack 0", 0xa0ed9b01da2806a1),
];

#[test]
fn the_agc_at_its_defaults_is_bit_identical_to_the_build_before() {
    let mut bad = vec![];
    for ((name, c), (bname, want)) in cases().iter().zip(BEFORE) {
        assert_eq!(*name, bname);
        let got = hash(c);
        println!("(\"{name}\", 0x{got:016x}),");
        if got != want {
            bad.push(*name);
        }
    }
    assert!(bad.is_empty(), "differs from the build before: {bad:?}");
}
