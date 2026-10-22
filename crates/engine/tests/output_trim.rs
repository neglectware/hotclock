//! The output trim (architecture 2.4): one gain after the safety limiter and the master gain, on everything the
//! mixer outputs (the sound card's buffer and the meter/stream tap alike), ramped so a change makes no click.

use engine::mixer::{LoadParams, Mixer, MixerControl};
use engine::pool::{DecoderPool, RamClip, VoiceSource};
use engine::source::Source;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
const AMP: f32 = 0.1; // -20 dBFS peak
const HZ: f32 = 440.0;

fn setup() -> (Mixer, MixerControl, DecoderPool) {
    let (mixer, mut ctl) = Mixer::new(RATE);
    let pool = DecoderPool::new(1, RATE);
    let n = RATE as usize * 10;
    let mut s = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = ((i as f32 * 2.0 * std::f32::consts::PI * HZ / RATE as f32).sin() * AMP * 32767.0)
            .round() as i16;
        s.push(v);
        s.push(v);
    }
    let feed = pool.open(VoiceSource::Memory {
        clip: Arc::new(RamClip {
            samples: s,
            rate: RATE,
        }),
        start_s: 0.0,
    });
    let status = feed.status.clone();
    ctl.load(
        feed,
        LoadParams {
            start_frame: 0,
            gain: 1.0,
            declick_in: false,
            stop: None,
        },
    )
    .unwrap();
    let t = Instant::now();
    while status
        .pushed_frames
        .load(std::sync::atomic::Ordering::Acquire)
        < 96_000
    {
        assert!(t.elapsed() < Duration::from_secs(5), "ring never filled");
        std::thread::sleep(Duration::from_millis(5));
    }
    (mixer, ctl, pool)
}

fn render(m: &mut Mixer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    m.render(&mut out);
    out
}

/// RMS of the left channel over `frames` frames from `from`, dBFS.
fn rms_dbfs(out: &[f32], from: usize, frames: usize) -> f64 {
    let ms: f64 = (from..from + frames)
        .map(|i| f64::from(out[i * 2]).powi(2))
        .sum::<f64>()
        / frames as f64;
    10.0 * ms.log10()
}

#[test]
fn a_trim_of_minus_6_db_measures_minus_26_dbfs_on_the_output_and_the_tap() {
    let mut results = vec![];
    for trim in [0.0f32, -6.0] {
        let (mut mixer, mut ctl, _pool) = setup();
        let mut tap = ctl.take_meter_tap().unwrap();
        ctl.set_output_trim_db(trim);
        let out = render(&mut mixer, 48_000);
        // A -20 dBFS peak sine is -23.01 dBFS RMS; 1 s of whole cycles after the limiter's lookahead.
        let r = rms_dbfs(&out, 4_800, 43_200);
        // The tap carries the same samples as the output buffer (the stream reads the tap).
        let mut tapped = vec![];
        while let Ok(c) = tap.post.read_chunk(tap.post.slots()) {
            if c.is_empty() {
                break;
            }
            tapped.extend(c.as_slices().0.iter().copied());
            tapped.extend(c.as_slices().1.iter().copied());
            c.commit_all();
        }
        let rt = rms_dbfs(&tapped, 4_800, 43_200);
        println!("trim {trim:+.1} dB: output {r:.3} dBFS RMS, tap {rt:.3} dBFS RMS");
        results.push((r, rt));
    }
    let d_out = results[1].0 - results[0].0;
    let d_tap = results[1].1 - results[0].1;
    println!("difference: output {d_out:.4} dB, tap {d_tap:.4} dB (want -6 within 0.05)");
    assert!((d_out + 6.0).abs() < 0.05, "{d_out}");
    assert!((d_tap + 6.0).abs() < 0.05, "{d_tap}");
    assert!(
        (results[0].0 + 23.01).abs() < 0.05,
        "reference level {}",
        results[0].0
    );
    assert!((results[1].0 + 29.03).abs() < 0.05);
}

#[test]
fn a_change_in_the_middle_of_a_tone_ramps_without_a_discontinuity() {
    let (mut mixer, ctl, _pool) = setup();
    let a = render(&mut mixer, 48_000);
    ctl.set_output_trim_db(-20.0);
    let b = render(&mut mixer, 48_000);
    let all: Vec<f32> = a.iter().chain(b.iter()).copied().collect();
    // The largest step between neighbouring samples: the tone's own is AMP * 2 pi f / rate = 0.00576. A
    // hard gain step of 0.1 -> 1 at the peak would add 0.09; the ramp's slope adds under 4e-5.
    let tone_step = f64::from(AMP) * 2.0 * std::f64::consts::PI * f64::from(HZ) / f64::from(RATE);
    let worst = (1..all.len() / 2)
        .map(|i| f64::from(all[i * 2] - all[(i - 1) * 2]).abs())
        .fold(0.0, f64::max);
    println!("largest step {worst:.6} (the untrimmed tone's own {tone_step:.6})");
    // 16-bit rounding of the clip adds up to 6e-5 and the ramp's own slope 4e-5: allow 1.5e-4 over the tone's step.
    assert!(worst <= tone_step + 1.5e-4, "{worst}");
    // And it lands: 100 ms after the change the level is down 20 dB; before it, not.
    let before = rms_dbfs(&a, 4_800, 40_000);
    let after = rms_dbfs(&b, 9_600, 36_000);
    println!("level before {before:.2} dBFS, after {after:.2} dBFS");
    assert!(((before - after) - 20.0).abs() < 0.05, "{}", before - after);
    // The ramp takes about 50 ms: halfway through it the gain is about halfway between.
    let mid = rms_dbfs(&b, 1_200 - 240, 480);
    assert!(mid < before - 1.0 && mid > after + 1.0, "mid {mid}");
}

#[test]
fn a_trim_of_zero_is_bit_exact() {
    let (mut m0, _c0, _p0) = setup();
    let (mut m1, c1, _p1) = setup();
    c1.set_output_trim_db(0.0);
    let a = render(&mut m0, 20_000);
    let b = render(&mut m1, 20_000);
    assert!(a == b, "unity trim changed the samples");
}
