//! M1.6 engine pieces: the mixer's silence-run statistic (the station's program-silence detector reads
//! it) and voices played from memory (the RAM loop).

use engine::mixer::{LoadParams, Mixer};
use engine::pool::{DecoderPool, RamClip, VoiceSource};
use engine::source::Source;
use std::sync::Arc;
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;

fn clip(seconds: f64, amp: f32) -> Arc<RamClip> {
    let n = (seconds * f64::from(RATE)) as usize;
    let mut s = Vec::with_capacity(n * 2);
    for i in 0..n {
        let v = ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / RATE as f32).sin()
            * amp
            * 32767.0) as i16;
        s.push(v);
        s.push(v);
    }
    Arc::new(RamClip {
        samples: s,
        rate: RATE,
    })
}

fn render(m: &mut Mixer, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    m.render(&mut out);
    out
}

#[test]
fn a_memory_voice_plays_its_samples_and_the_silent_run_counts_the_rest() {
    let (mut mixer, mut ctl) = Mixer::new(RATE);
    ctl.set_silence_threshold_dbfs(-50.0);
    let pool = DecoderPool::new(1, RATE);
    // Before any voice: silence, and the run grows with every block.
    render(&mut mixer, 48_000);
    let st = ctl.stats();
    println!(
        "silent run after 1 s of nothing: {} frames, last block {} dBFS",
        st.silent_run_frames, st.last_block_rms_dbfs
    );
    assert_eq!(st.silent_run_frames, 48_000);
    assert!(st.last_block_rms_dbfs < -100.0);

    let c = clip(2.0, 0.3);
    let feed = pool.open(VoiceSource::Memory {
        clip: c.clone(),
        start_s: 0.0,
    });
    let status = feed.status.clone();
    let start = 60_000u64;
    ctl.load(
        feed,
        LoadParams {
            start_frame: start,
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
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "the memory source never filled the ring"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Render up to well inside the clip; the output is the clip, delayed by the limiter's lookahead.
    let out = render(&mut mixer, 60_000 + 48_000);
    let lat = mixer.limiter_latency_frames();
    let seg = &out[(start as usize + lat + 2_000) * 2..(start as usize + lat + 30_000) * 2];
    let peak = seg.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    println!(
        "memory voice: peak {peak:.4} (the clip's amplitude is 0.3), limiter latency {lat} frames"
    );
    assert!((peak - 0.3).abs() < 0.01, "{peak}");
    let st = ctl.stats();
    assert!(
        st.silent_run_frames < 2_048,
        "the tone ended the silent run: {}",
        st.silent_run_frames
    );
    assert!(st.last_block_rms_dbfs > -30.0);
    // Exactly the clip's samples (checked on the first 1,000 after the latency).
    let expect: Vec<f32> = c
        .samples
        .iter()
        .step_by(2)
        .map(|&v| f32::from(v) / 32768.0)
        .collect();
    // The mixer mixes ahead by the limiter's latency, so the voice's first sample is at its start frame.
    let base = start as usize;
    let worst = (0..1000)
        .map(|k| (out[(base + 5_000 + k) * 2] - expect[5_000 + k]).abs())
        .fold(0.0f32, f32::max);
    println!("memory voice: worst sample difference from the clip over 1,000 frames: {worst:e}");
    // Not bit-exact through the mixer's output stage (about 4 LSB of 16 bits at worst here); -70 dBFS.
    assert!(worst < 3e-4, "{worst}");
    // After the clip ends the silent run starts again.
    let _ = render(&mut mixer, 200_000);
    assert!(ctl.stats().silent_run_frames > 48_000);
}

#[test]
fn the_silence_threshold_moves() {
    let (mut mixer, mut ctl) = Mixer::new(RATE);
    let pool = DecoderPool::new(1, RATE);
    // A tone at -46 dBFS peak (about -49 RMS): silent at -40, not silent at -60.
    let c = clip(1.0, 0.005);
    let feed = pool.open(VoiceSource::Memory {
        clip: c,
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
    while status
        .pushed_frames
        .load(std::sync::atomic::Ordering::Acquire)
        < 48_000
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    ctl.set_silence_threshold_dbfs(-40.0);
    render(&mut mixer, 20_000);
    assert!(
        ctl.stats().silent_run_frames >= 19_000,
        "{}",
        ctl.stats().silent_run_frames
    );
    ctl.set_silence_threshold_dbfs(-60.0);
    render(&mut mixer, 4_096);
    assert_eq!(ctl.stats().silent_run_frames, 0);
}
