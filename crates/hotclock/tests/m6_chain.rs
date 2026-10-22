//! M6 acceptance, the meter frame and the chain through the real mixer, taps and meter code (acceptance 5, 8, 9, 10),
//! offline: no audio device, no process. The meter is driven the way the meter thread drives it, one render of 1,600
//! frames (a frame period) at a time. Each test prints its measured value next to its limit; BUILDLOG copies them.

#![allow(clippy::needless_range_loop)]

use common::chain::{preset_config, ChainConfig};
use engine::feed::VoiceFeed;
use engine::mixer::{BlockInfo, LoadParams, Mixer, MixerControl};
use engine::source::Source;
use serde_json::{json, Value};
use serve::meters::{frame_json, Meter, FRAME_LEN};
use std::f64::consts::PI;

const RATE: u32 = 48_000;
const R: f64 = 48_000.0;
const HOP: usize = 1600;

fn tone(hz: f64, db: f64, secs: f64) -> Vec<f32> {
    let a = 10f64.powf(db / 20.0);
    (0..(secs * R) as usize)
        .flat_map(|i| {
            let v = (a * (2.0 * PI * hz * i as f64 / R).sin()) as f32;
            [v, v]
        })
        .collect()
}

fn cfg(base: &str, edits: &[(&str, Value)]) -> ChainConfig {
    let mut c = preset_config(base).unwrap();
    for (b, v) in edits {
        c.apply_block(b, v.as_object().unwrap()).unwrap();
    }
    c
}

struct Rig {
    mixer: Mixer,
    ctl: MixerControl,
    tap: engine::mixer::MeterTap,
    meter: Meter,
    /// Every frame emitted, and the output audio, interleaved.
    frames: Vec<[u8; FRAME_LEN]>,
    out: Vec<f32>,
}

impl Rig {
    fn new(c: &ChainConfig) -> Rig {
        let (mixer, mut ctl) = Mixer::new(RATE);
        let tap = ctl.take_meter_tap().unwrap();
        let mut meter = Meter::new(RATE, 240.0);
        meter.set_chain(tap.chain.clone());
        meter.set_link(tap.link.clone());
        ctl.chain_control().apply(c).unwrap();
        Rig {
            mixer,
            ctl,
            tap,
            meter,
            frames: vec![],
            out: vec![],
        }
    }

    fn voice(&mut self, start: u64, s: &[f32]) {
        self.ctl
            .load(
                VoiceFeed::from_samples(s),
                LoadParams {
                    declick_in: false,
                    ..LoadParams::at(start)
                },
            )
            .unwrap();
    }

    /// Render `frames` more frames (a multiple of the hop), feeding the meter after each hop.
    fn run(&mut self, frames: usize) {
        let mut zeros = vec![0.0f32; HOP * 2];
        for _ in 0..frames / HOP {
            let mut block = vec![0.0f32; HOP * 2];
            self.mixer.render(&mut block);
            let ni = self.tap.info.slots();
            let mut infos: Vec<BlockInfo> = Vec::with_capacity(ni);
            if let Ok(c) = self.tap.info.read_chunk(ni) {
                let (a, b) = c.as_slices();
                infos.extend_from_slice(a);
                infos.extend_from_slice(b);
                c.commit_all();
            }
            let n: usize = infos.iter().map(|i| i.frames as usize).sum();
            let mut post = vec![];
            if let Ok(c) = self.tap.post.read_chunk(n * 2) {
                let (a, b) = c.as_slices();
                post.extend_from_slice(a);
                post.extend_from_slice(b);
                c.commit_all();
            }
            zeros.resize(post.len(), 0.0);
            let tpg = f32::from_bits(
                self.tap
                    .link
                    .tp_min_gain_bits
                    .swap(1.0f32.to_bits(), std::sync::atomic::Ordering::Relaxed),
            );
            self.meter.set_tp_gain(tpg.min(1.0));
            let frames = &mut self.frames;
            self.meter
                .push_with(&post, &zeros, None, &infos, true, &mut |f| frames.push(*f));
            self.out.extend_from_slice(&block);
            self.ctl.pump();
        }
    }

    fn json(&self, i: usize) -> Value {
        frame_json(&self.frames[i])
    }
}

fn peak_db(x: &[f32]) -> f64 {
    20.0 * f64::from(x.iter().fold(0.0f32, |m, v| m.max(v.abs())))
        .max(1e-9)
        .log10()
}

fn rms_db(x: &[f32]) -> f64 {
    10.0 * (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64)
        .max(1e-14)
        .log10()
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

// ---- 8: processing meters --------------------------------------------------------------------------------------

#[test]
fn acceptance_8_agc_gain_follows_the_chains_own_gain() {
    // A 1 kHz tone stepped -30, -25, -20, -15, -10 dBFS, 14 s each; only the AGC on, at the configured slew rates.
    let c = cfg(
        "bypass",
        &[(
            "agc",
            json!({"enabled": true, "max_gain_db": 24.0, "max_reduction_db": 24.0, "up_db_per_s": 1.0, "fast_attack_ms": 0.0}),
        )],
    );
    let mut rig = Rig::new(&c);
    let mut x = vec![];
    for db in [-30.0, -25.0, -20.0, -15.0, -10.0] {
        x.extend(tone(1000.0, db, 14.0));
    }
    let n = x.len() / 2;
    rig.voice(0, &x);
    rig.run(n - n % HOP);
    // The gain the chain applied, measured on the audio: the output's level over the input's, per frame period,
    // against the frame's AGC gain. The limiter's 5 ms and the safety limiter's 2 ms delay the output.
    let delay = 336;
    let (mut worst, mut count) = (0.0f64, 0);
    for (i, fr) in rig.frames.iter().enumerate().skip(40) {
        let a = i * HOP;
        let (lo, hi) = (a + delay, a + HOP + delay);
        if hi >= rig.out.len() / 2 || hi > n {
            break;
        }
        let g_audio = rms_db(&rig.out[lo * 2..hi * 2]) - rms_db(&x[a * 2..(a + HOP) * 2]);
        let j = frame_json(fr);
        let g_frame = f(&j["stages"][0]["gr"]);
        // At a step the frame's period straddles two levels: skip the periods that contain one.
        let step_here =
            (0..6).any(|k| (k * 14 * 48_000).max(1) >= a && k * 14 * 48_000 <= a + HOP + delay);
        if step_here {
            continue;
        }
        worst = worst.max((g_audio - g_frame).abs());
        count += 1;
    }
    println!(
        "AGC gain in the frame against the output over the input, {count} frames over 70 s of 1 kHz tone stepped -30 to -10 dBFS: largest difference {worst:.3} dB (limit 0.1)"
    );
    assert!(worst < 0.1);
}

#[test]
fn acceptance_8_a_low_tone_shows_reduction_in_low_only() {
    let c = cfg(
        "radio",
        &[
            ("agc", json!({"enabled": false})),
            ("eq", json!({"enabled": false})),
        ],
    );
    let mut rig = Rig::new(&c);
    let x = tone(100.0, -20.0, 6.0);
    rig.voice(0, &x);
    rig.run(6 * 48_000 / HOP * HOP);
    let j = rig.json(rig.frames.len() - 3);
    let gr: Vec<f64> = j["band_gr"].as_array().unwrap().iter().map(f).collect();
    println!(
        "100 Hz tone at -20 dBFS through the five-band radio multiband: reduction per band {:?} dB (LOW shows reduction, the others read 0 within 0.1)",
        gr.iter().map(|g| if g.is_nan() { "--".to_string() } else { format!("{g:.2}") }).collect::<Vec<_>>()
    );
    assert!(gr[0] < -0.5, "LOW {}", gr[0]);
    for k in 1..5 {
        assert!(gr[k].abs() < 0.1, "band {k}: {}", gr[k]);
    }
    assert!(gr[5].is_nan(), "the sixth slot is unused");
    assert_eq!(j["band_count"], 5);
}

#[test]
fn acceptance_8_limiter_reduction_matches_the_output_to_input_difference() {
    let c = cfg(
        "bypass",
        &[("limiter", json!({"enabled": true, "ceiling_dbtp": -1.0}))],
    );
    let mut rig = Rig::new(&c);
    for (k, db) in [0.0, 3.0, 6.0, 10.0].into_iter().enumerate() {
        let x = tone(1000.0, db, 6.0);
        let start = (k * 8 * 48_000) as u64;
        rig.voice(start, &x);
    }
    rig.run(32 * 48_000 / HOP * HOP);
    let mut worst = 0.0f64;
    let mut line = vec![];
    for (k, db) in [0.0, 3.0, 6.0, 10.0].into_iter().enumerate() {
        // a frame period late in the 6 s tone
        let a = (k * 8 + 5) * 48_000 / HOP * HOP;
        let i = a / HOP;
        let j = rig.json(i + 1);
        let gr = f(&j["stages"][3]["gr"]);
        let out = peak_db(&rig.out[(a + 336) * 2..(a + HOP + 336) * 2]);
        let diff = out - db;
        worst = worst.max((gr - diff).abs());
        line.push(format!(
            "{db:+.0} dBFS in: out {out:.2}, out-in {diff:.2}, frame {gr:.2}"
        ));
    }
    println!(
        "limiter at -1.0 dBTP, 1 kHz tone above the ceiling: {} ; largest difference of the frame's reduction from out-in {worst:.3} dB (limit 0.2)",
        line.join("; ")
    );
    assert!(worst < 0.2);
}

#[test]
fn acceptance_8_bypass_sets_the_flag_and_reports_zero_everywhere() {
    let mut rig = Rig::new(&cfg("bypass", &[]));
    rig.voice(0, &tone(1000.0, -20.0, 4.0));
    rig.run(4 * 48_000 / HOP * HOP);
    let j = rig.json(rig.frames.len() - 2);
    println!(
        "bypass: flags {} stage mask {:#010b} bands {} stage reductions {:?} band slots {:?}",
        j["flags"],
        j["stage_mask"].as_u64().unwrap(),
        j["band_count"],
        j["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["gr"].clone())
            .collect::<Vec<_>>(),
        j["band_gr"]
    );
    assert_eq!(j["flags"]["bypassed"], true);
    assert_eq!(j["band_count"], 0);
    assert_eq!(j["stage_mask"], 1 << 5, "only the safety limiter is active");
    for s in j["stages"].as_array().unwrap() {
        assert!(f(&s["gr"]).abs() < 0.1, "{s}");
    }
    assert!(j["band_gr"].as_array().unwrap().iter().all(Value::is_null));
}

// ---- 9: stage meters ---------------------------------------------------------------------------------------------

#[test]
fn acceptance_9_stage_levels_and_reductions_match_an_independent_measurement() {
    // Each stage alone, a 1 kHz tone that makes it work; the frame's in and out against the peaks of the audio
    // going in and coming out, and its reduction against their difference.
    let cases: [(&str, usize, Value, f64); 5] = [
        (
            "agc",
            0,
            json!({"enabled": true, "target_dbfs": -20.0, "fast_attack_ms": 0.0, "max_gain_db": 24.0, "up_db_per_s": 40.0, "down_db_per_s": 40.0}),
            -33.0,
        ),
        // The multiband's reduction is the mean of its bands': one tone in each of three bands, at the same level,
        // so that every band reduces alike and the mean is the whole signal's reduction.
        (
            "multiband",
            1,
            json!({"enabled": true, "crossovers_hz": [200.0, 4000.0], "threshold_dbfs": [-30.0, -30.0, -30.0], "ratio": [4.0, 4.0, 4.0], "knee_db": [0.0, 0.0, 0.0], "attack_ms": [5.0, 5.0, 5.0], "release_ms": [100.0, 100.0, 100.0], "makeup_db": [0.0, 0.0, 0.0]}),
            -99.0,
        ),
        (
            "wideband",
            2,
            json!({"enabled": true, "threshold_dbfs": -24.0, "ratio": 3.0, "knee_db": 0.0, "attack_ms": 5.0, "release_ms": 100.0}),
            -10.0,
        ),
        (
            "limiter",
            3,
            json!({"enabled": true, "ceiling_dbtp": -3.0}),
            0.0,
        ),
        (
            "clipper",
            4,
            json!({"enabled": true, "threshold_dbfs": -6.0, "softness_db": 2.0}),
            -1.0,
        ),
    ];
    let mut worst_lvl = 0.0f64;
    let mut worst_gr = 0.0f64;
    for (block, st, edit, lvl) in cases {
        let c = cfg("bypass", &[(block, edit)]);
        let mut rig = Rig::new(&c);
        let x = if block == "multiband" {
            let (a, b, c3) = (
                tone(100.0, -24.0, 24.0),
                tone(1000.0, -24.0, 24.0),
                tone(9000.0, -24.0, 24.0),
            );
            a.iter()
                .zip(&b)
                .zip(&c3)
                .map(|((p, q), r)| p + q + r)
                .collect()
        } else {
            tone(1000.0, lvl, 24.0)
        };
        rig.voice(0, &x);
        rig.run(24 * 48_000 / HOP * HOP);
        let i = rig.frames.len() - 6;
        let j = rig.json(i);
        let (a, b) = (i * HOP, (i + 1) * HOP);
        // The input is the tone; the output is the audio of the same period (minus the fixed 336-frame delay).
        let din = peak_db(&x[a * 2..b * 2]);
        let dout = peak_db(&rig.out[(a + 336) * 2..(b + 336) * 2]);
        let s = &j["stages"][st];
        let (fin, fout, fgr) = (f(&s["in"]), f(&s["out"]), f(&s["gr"]));
        // The AGC's reduction is its signed gain; the others are negative.
        let want_gr = dout - din;
        worst_lvl = worst_lvl.max((fin - din).abs()).max((fout - dout).abs());
        worst_gr = worst_gr.max((fgr - want_gr).abs());
        println!(
            "{block}: in {fin:.2} (measured {din:.2}), out {fout:.2} (measured {dout:.2}), reduction {fgr:.2} (out-in {want_gr:.2}); mask {:#010b}",
            rig.frames[i][28]
        );
        // Disabling it clears its bit and leaves the others unchanged.
        let mask_on = rig.frames[i][28];
        let off = cfg("bypass", &[(block, json!({"enabled": false}))]);
        rig.ctl.chain_control().apply(&off).unwrap();
        rig.run(2 * 48_000 / HOP * HOP);
        let mask_off = rig.frames[rig.frames.len() - 1][28];
        let bit = if block == "limiter" {
            3
        } else {
            [0, 1, 2, 3, 4][st]
        };
        assert_eq!(mask_on & (1 << bit), 1 << bit, "{block} on");
        assert_eq!(
            mask_off,
            mask_on & !(1u8 << bit),
            "{block}: only its bit clears (on {mask_on:#b}, off {mask_off:#b})"
        );
    }
    println!("stage meters, five stages: largest level difference {worst_lvl:.3} dB (limit 0.1), largest reduction difference {worst_gr:.3} dB (limit 0.2)");
    assert!(worst_lvl < 0.1 && worst_gr < 0.2);
}

// ---- 10: band count ------------------------------------------------------------------------------------------------

fn bands_cfg(xo: &[f64]) -> ChainConfig {
    let n = xo.len() + 1;
    let v = |x: f64| json!(vec![x; n]);
    cfg(
        "bypass",
        &[(
            "multiband",
            json!({"enabled": true, "crossovers_hz": xo, "threshold_dbfs": v(-30.0), "ratio": v(2.0), "knee_db": v(6.0),
                "attack_ms": v(10.0), "release_ms": v(100.0), "makeup_db": v(0.0)}),
        )],
    )
}

#[test]
fn acceptance_10_band_counts_in_the_frame_and_one_latency() {
    let sets: [&[f64]; 3] = [
        &[200.0, 4000.0],
        &[120.0, 500.0, 2000.0, 6000.0],
        &[80.0, 300.0, 1000.0, 3000.0, 9000.0],
    ];
    let mut onsets = vec![];
    for xo in sets {
        let n = xo.len() + 1;
        let mut rig = Rig::new(&bands_cfg(xo));
        rig.voice(0, &tone(1000.0, -15.0, 3.0));
        // An impulse in a second voice, 4 s in: where it comes out is the chain's latency.
        let mut imp = vec![0.0f32; 2 * 4800];
        imp[2 * 2400] = 0.5;
        imp[2 * 2400 + 1] = 0.5;
        rig.voice(4 * 48_000, &imp);
        rig.run(5 * 48_000 / HOP * HOP);
        let j = rig.json(rig.frames.len() / 2 - 20);
        let bands = j["band_count"].as_u64().unwrap() as usize;
        let slots: Vec<bool> = j["band_gr"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| !v.is_null())
            .collect();
        let want: Vec<bool> = (0..6).map(|i| i < n).collect();
        // first output frame of the impulse
        let at = rig
            .out
            .chunks_exact(2)
            .enumerate()
            .skip(4 * 48_000)
            .find(|(_, fr)| fr[0].abs() > 1e-6)
            .map(|(i, _)| i)
            .unwrap();
        onsets.push(at);
        println!("{n} bands: frame band count {bands}, used slots {slots:?}; the impulse (input frame {}) first sounds at output frame {at}", 4 * 48_000 + 2400);
        assert_eq!(bands, n);
        assert_eq!(slots, want);
    }
    println!("whole-chain latency at 3, 5 and 6 bands: first output frames {onsets:?} (limit: identical)");
    assert!(onsets.iter().all(|&o| o == onsets[0]));
}

#[test]
fn acceptance_10_a_mismatched_band_array_is_rejected_at_load() {
    // As a station file carries it.
    let text = "name = \"x\"\nlisten = \"0.0.0.0:1\"\ndevice = \"null\"\nprocessing_preset = \"radio\"\n[chain.multiband]\ncrossovers_hz = [200.0, 4000.0]\n";
    let e = common::config::StationConfig::parse(
        text,
        std::path::Path::new("/etc/hotclock/stations/x.toml"),
    )
    .unwrap_err();
    println!("a three-band crossover list with five-band arrays: {e}");
    assert!(e.to_string().contains("chain.multiband"), "{e}");
    // And as the API takes it.
    let mut c = preset_config("radio").unwrap();
    let o = json!({"crossovers_hz": [200.0, 4000.0]});
    assert!(c.apply_block("multiband", o.as_object().unwrap()).is_err());
}

// ---- 5: the chain's latency ---------------------------------------------------------------------------------------

#[test]
fn acceptance_5_latency_is_identical_with_every_combination_of_bypasses() {
    // The ten switches of the chain, 1,024 combinations: an impulse goes through the real mixer and the frame where it
    // first sounds is compared with the all-off chain.
    let switches = [
        "highpass",
        "rotator",
        "stereo",
        "agc",
        "eq",
        "bass",
        "multiband",
        "wideband",
        "clipper",
        "limiter",
    ];
    let first_sound = |c: &ChainConfig| -> usize {
        let mut rig = Rig::new(c);
        let mut x = vec![0.0f32; 2 * 9600];
        x[2 * 4800] = 0.5;
        x[2 * 4800 + 1] = 0.5;
        rig.voice(4_000, &x);
        rig.run(12 * HOP);
        rig.out
            .chunks_exact(2)
            .enumerate()
            .skip(2_000)
            .find(|(_, fr)| fr[0].abs() > 1e-7)
            .map(|(i, _)| i)
            .unwrap_or(usize::MAX)
    };
    let base = first_sound(&preset_config("bypass").unwrap());
    let mut worst = 0usize;
    let mut n = 0;
    for mask in 0u32..1 << switches.len() {
        let mut c = preset_config("bypass").unwrap();
        for (k, s) in switches.iter().enumerate() {
            if mask >> k & 1 == 1 {
                c.apply_block(s, json!({"enabled": true}).as_object().unwrap())
                    .unwrap();
            }
        }
        // EQ with all bands flat is the identity; give it a band so that the block really runs.
        if mask >> 4 & 1 == 1 {
            c.apply_block("eq", json!({"low_db": 3.0}).as_object().unwrap())
                .unwrap();
        }
        let at = first_sound(&c);
        worst = worst.max(at.abs_diff(base));
        n += 1;
    }
    println!("{n} combinations of the ten switches: the impulse first sounds at output frame {base} with everything off; the largest difference over all combinations is {worst} frames (limit 0)");
    assert_eq!(worst, 0);
}

// ---- the meter thread's cost with the chain active ---------------------------------------------------------------

#[test]
fn acceptance_8_the_meter_costs_little_with_the_chain_active() {
    let mut rig = Rig::new(&preset_config("radio").unwrap());
    rig.voice(0, &tone(440.0, -18.0, 60.0));
    // Render first (that is rt-out's work), then time only the meter on the taps' contents.
    let mut blocks: Vec<(Vec<f32>, Vec<BlockInfo>)> = vec![];
    for _ in 0..(60 * 48_000 / HOP) {
        let mut block = vec![0.0f32; HOP * 2];
        rig.mixer.render(&mut block);
        let ni = rig.tap.info.slots();
        let mut infos = vec![];
        if let Ok(c) = rig.tap.info.read_chunk(ni) {
            let (a, b) = c.as_slices();
            infos.extend_from_slice(a);
            infos.extend_from_slice(b);
            c.commit_all();
        }
        let n: usize = infos.iter().map(|i| i.frames as usize).sum();
        let mut post = vec![];
        if let Ok(c) = rig.tap.post.read_chunk(n * 2) {
            let (a, b) = c.as_slices();
            post.extend_from_slice(a);
            post.extend_from_slice(b);
            c.commit_all();
        }
        blocks.push((post, infos));
    }
    let zeros = vec![0.0f32; HOP * 2];
    let t = std::time::Instant::now();
    let mut emitted = 0;
    for (post, infos) in &blocks {
        rig.meter
            .push_with(post, &zeros[..post.len()], None, infos, true, &mut |_| {
                emitted += 1
            });
    }
    let secs = t.elapsed().as_secs_f64();
    println!("meter thread, 60 s of audio with the radio chain active and frames built: {secs:.3} s of CPU = {:.2} % of one core (limit 4 %); {emitted} frames", secs / 60.0 * 100.0);
    assert!(secs / 60.0 < 0.04);
}

#[test]
fn a_station_file_with_the_old_equalizer_keys_loads_as_bands_1_to_4() {
    // A file written before the parametric equalizer: `low_db` and the like. It must load, mean the same, and the
    // station's chain must hold the values in the new keys.
    let text = "name = \"x\"\nlisten = \"0.0.0.0:1\"\ndevice = \"null\"\nprocessing_preset = \"radio\"\n[chain.eq]\nposition = \"post\"\nlow_hz = 80.0\nlow_db = 3.0\nmid1_hz = 300.0\nmid1_db = -2.0\nmid1_q = 1.5\nmid2_db = 2.5\nhigh_db = -3.0\n";
    let cfg = common::config::StationConfig::parse(
        text,
        std::path::Path::new("/etc/hotclock/stations/x.toml"),
    )
    .unwrap();
    let c = cfg.chain_config();
    assert_eq!(
        (
            c.num("eq", "b1_hz"),
            c.num("eq", "b1_db"),
            c.num("eq", "b2_hz"),
            c.num("eq", "b2_db"),
            c.num("eq", "b2_q"),
            c.num("eq", "b3_db"),
            c.num("eq", "b4_db")
        ),
        (80.0, 3.0, 300.0, -2.0, 1.5, 2.5, -3.0)
    );
    assert_eq!(c.text("eq", "position"), "post");
    println!("old equalizer keys in a station file: bands 1 to 4 hold 80 Hz +3, 300 Hz -2 Q 1.5, 3 kHz +2.5, 10 kHz -3");
}

// ---- the rack's per-block output peaks ----------------------------------------------------------------------------

fn blocks_of(j: &Value) -> Vec<(Option<f64>, Option<f64>)> {
    j["block_out"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_f64(), p[1].as_f64()))
        .collect()
}

#[test]
fn the_frame_reports_each_blocks_output_peak_and_a_block_that_is_off_reports_its_input() {
    // 1 kHz, left at -20 dBFS and right at -26 dBFS. First the bypass preset: every block is off, so every block
    // reports the level it was given (left and right apart); then the wideband compressor alone, which turns the
    // blocks from itself on down and leaves the ones before it at the input level.
    let l = tone(1000.0, -20.0, 6.0);
    let r = tone(1000.0, -26.0, 6.0);
    let x: Vec<f32> = l
        .chunks(2)
        .zip(r.chunks(2))
        .flat_map(|(a, b)| [a[0], b[1]])
        .collect();
    let c = cfg("bypass", &[]);
    let mut rig = Rig::new(&c);
    rig.voice(0, &x);
    rig.run(6 * 48_000 / HOP * HOP);
    let b = blocks_of(&rig.json(rig.frames.len() - 4));
    assert_eq!(b.len(), 14);
    for (i, (bl, br)) in b.iter().enumerate() {
        let (bl, br) = (bl.expect("left level"), br.expect("right level"));
        println!("bypass block {i}: L {bl:.2} R {br:.2}");
        assert!((bl + 20.0).abs() < 0.15, "block {i} left {bl}");
        assert!((br + 26.0).abs() < 0.15, "block {i} right {br}");
    }
    let c = cfg(
        "bypass",
        &[(
            "wideband",
            json!({"enabled": true, "threshold_dbfs": -30.0, "ratio": 4.0, "knee_db": 0.0, "attack_ms": 5.0, "release_ms": 100.0}),
        )],
    );
    let mut rig = Rig::new(&c);
    rig.voice(0, &x);
    rig.run(6 * 48_000 / HOP * HOP);
    let b = blocks_of(&rig.json(rig.frames.len() - 4));
    for i in 0..8 {
        assert!(
            (b[i].0.unwrap() + 20.0).abs() < 0.15,
            "before the wideband: block {i} {:?}",
            b[i]
        );
    }
    let (wl, wr) = (b[8].0.unwrap(), b[8].1.unwrap());
    println!("wideband out: L {wl:.2} R {wr:.2}");
    assert!(
        wl < -22.0 && wr < -28.0,
        "the wideband brings both channels down: {wl} {wr}"
    );
    for i in 9..14 {
        assert!(
            (b[i].0.unwrap() - wl).abs() < 0.3 && (b[i].1.unwrap() - wr).abs() < 0.3,
            "after the wideband: block {i} {:?}",
            b[i]
        );
    }
}
