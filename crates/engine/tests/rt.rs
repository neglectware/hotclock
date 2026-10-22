//! M1.1 acceptance tests 2 and 4 (and device loss), on the `Null` backend. Each prints its
//! measured number next to its limit.

use common::{Clock, RealClock, VirtualClock};
use engine::output::NullOutput;
use engine::rt::{Command, CommandKind, EventKind, RtHandle, RtOptions, RtState};
use engine::testing::FaultyOutput;
use engine::{HwRequest, ToneSource};
use std::sync::atomic::Ordering::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn tone() -> Box<ToneSource> {
    Box::new(ToneSource::new(48_000, 1000.0, -20.0))
}

fn drain(h: &mut RtHandle) -> Vec<engine::Event> {
    let mut v = vec![];
    while let Some(e) = h.poll_event() {
        v.push(e);
    }
    v
}

/// Acceptance 2: `Null` backend, 60 s at real time; `frames_written` within 0.1 percent of
/// 60 x 48,000. The counter that is compared is the audible frame (`frames_written - delay`,
/// architecture 1.3): raw `frames_written` runs ahead of the audio by the device buffer
/// (up to 8,192 frames, 0.28 percent of 60 s), which is what a real card does too.
#[test]
fn null_backend_60s_real_time_is_within_0_1_percent() {
    let clock = Arc::new(RealClock::new());
    let out = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let t0 = Instant::now();
    let h = RtHandle::start(Box::new(out), tone(), clock, RtOptions::default()).unwrap();
    std::thread::sleep(Duration::from_secs(60));
    let stats = h.stats();
    let wall = t0.elapsed().as_secs_f64();
    h.stop();
    let expected = wall * 48_000.0;
    let audible = stats.audible_frames() as f64;
    let err_pct = (audible - expected).abs() / (60.0 * 48_000.0) * 100.0;
    println!(
        "null 60 s: wall {wall:.3} s, frames_written {}, delay {}, audible {audible}, \
         expected {expected:.0}; error {err_pct:.4} % of 2,880,000 (limit 0.1 %); \
         raw frames_written leads by {} frames; xruns {}",
        stats.frames_written,
        stats.delay_frames,
        stats.frames_written as f64 - expected,
        stats.xruns
    );
    assert!(err_pct < 0.1);
    assert_eq!(stats.xruns, 0);
    // The nominal target itself: 60 x 48,000.
    let nominal_err = (audible - 2_880_000.0).abs() / 2_880_000.0 * 100.0;
    println!("audible vs nominal 2,880,000: {nominal_err:.4} % (limit 0.1 %)");
    assert!(nominal_err < 0.1);
}

/// Acceptance 4: a writer stalled for 500 ms causes an xrun that is counted and recovered, and
/// the tone continues.
#[test]
fn stalled_writer_causes_counted_xrun_and_recovery() {
    let clock = Arc::new(RealClock::new());
    let out = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let mut h = RtHandle::start(Box::new(out), tone(), clock, RtOptions::default()).unwrap();
    std::thread::sleep(Duration::from_millis(500)); // buffer full, steady state
    assert_eq!(h.stats().xruns, 0);
    assert!(h.send(Command::now(CommandKind::Stall { ms: 500 })));
    std::thread::sleep(Duration::from_millis(1200));
    let s1 = h.stats();
    let before = s1.frames_written;
    std::thread::sleep(Duration::from_millis(500));
    let s2 = h.stats();
    let events = drain(&mut h);
    println!(
        "stall 500 ms (buffer 170.7 ms): xruns {} (expect 1); frames advanced {} in 0.5 s after \
         recovery (expect ~24000); max write {:.1} ms; state {:?}",
        s2.xruns,
        s2.frames_written - before,
        s2.max_write_ns as f64 / 1e6,
        s2.state
    );
    assert_eq!(s2.xruns, 1);
    assert!(events.iter().any(|e| e.kind == EventKind::XrunRecovered));
    assert_eq!(s2.state, RtState::Playing);
    assert!(s2.frames_written - before > 20_000, "tone did not continue");
    // The source does not advance during the gap: silence frames are written on top.
    assert_eq!(s2.frames_written - s2.source_frames, 8 * 1024);
    h.stop();
}

/// A short stall inside the buffer is not an xrun.
#[test]
fn stall_shorter_than_the_buffer_is_not_an_xrun() {
    let clock = Arc::new(RealClock::new());
    let out = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let mut h = RtHandle::start(Box::new(out), tone(), clock, RtOptions::default()).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    h.send(Command::now(CommandKind::Stall { ms: 100 }));
    std::thread::sleep(Duration::from_millis(600));
    let s = h.stats();
    println!("stall 100 ms: xruns {} (expect 0)", s.xruns);
    assert_eq!(s.xruns, 0);
    h.stop();
}

/// Device loss: the process keeps running, the station clock pauses, and playback resumes on
/// reopen. Retry interval shortened to keep the test quick; the default is 2 s (architecture 1.5).
#[test]
fn device_loss_pauses_clock_and_resumes() {
    let clock = Arc::new(RealClock::new());
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl) = FaultyOutput::new(Box::new(null));
    let opts = RtOptions {
        recovery_retry_ns: 200_000_000,
        ..RtOptions::default()
    };
    let mut h = RtHandle::start(Box::new(faulty), tone(), clock, opts).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    ctl.unplugged.store(true, Release);
    let t_unplug = Instant::now();
    std::thread::sleep(Duration::from_millis(800));
    let lost = h.stats();
    assert_eq!(lost.state, RtState::DeviceLost);
    assert_eq!(lost.device_lost, 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        h.stats().frames_written,
        lost.frames_written,
        "clock must pause"
    );
    let attempts = ctl.reopen_attempts.load(Acquire);
    assert!(attempts >= 3, "reopen attempts {attempts}");

    ctl.unplugged.store(false, Release);
    let t_plug = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.stats().state != RtState::Playing && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let resumed_after = t_plug.elapsed();
    let s = h.stats();
    println!(
        "device loss: lost at +{:.1} s, replugged, playing again {:.2} s later (limit 5 s); \
         outage {} ms; reopen attempts {}",
        t_unplug.elapsed().as_secs_f64() - resumed_after.as_secs_f64(),
        resumed_after.as_secs_f64(),
        s.outage_ms_total,
        attempts
    );
    assert_eq!(s.state, RtState::Playing);
    assert!(resumed_after < Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.stats().frames_written > lost.frames_written + 5_000);
    let events = drain(&mut h);
    assert!(events.iter().any(|e| e.kind == EventKind::DeviceLost));
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, EventKind::DeviceRestored { .. })));
    h.stop();
}

/// Stopping while the device is gone still returns promptly.
#[test]
fn stop_while_device_lost() {
    let clock = Arc::new(RealClock::new());
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl) = FaultyOutput::new(Box::new(null));
    let opts = RtOptions {
        recovery_retry_ns: 5_000_000_000,
        ..RtOptions::default()
    };
    let h = RtHandle::start(Box::new(faulty), tone(), clock, opts).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    ctl.unplugged.store(true, Release);
    std::thread::sleep(Duration::from_millis(300));
    let t = Instant::now();
    h.stop();
    let took = t.elapsed();
    println!(
        "stop while lost took {:.0} ms (limit 500 ms)",
        took.as_secs_f64() * 1e3
    );
    assert!(took < Duration::from_millis(500));
}

/// An injected xrun on a virtual clock: counted once, one buffer of silence, source not skipped.
#[test]
fn injected_xrun_does_not_skip_source_audio() {
    let clock = Arc::new(VirtualClock::new(0));
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl) = FaultyOutput::new(Box::new(null));
    let opts = RtOptions {
        max_frames: Some(48_000),
        ..RtOptions::default()
    };
    ctl.xrun_next.store(1, Release);
    let h = RtHandle::start(Box::new(faulty), tone(), clock.clone(), opts).unwrap();
    while !h.is_finished() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let s = h.stats();
    println!(
        "virtual clock: xruns {}, source_frames {} (limit exactly 48000), frames_written {}",
        s.xruns, s.source_frames, s.frames_written
    );
    assert_eq!(s.xruns, 1);
    assert_eq!(s.source_frames, 48_000);
    assert_eq!(s.frames_written, 48_000 + 8 * 1024);
    let _ = clock.monotonic_ns();
    h.wait();
}

// ---- Review 2026-09-29: no frame sent twice or skipped across faults, and no step at the edges ----

use engine::fault::Fault;
use engine::negotiate::Granted;
use engine::rt::Stats;
use engine::{Output, OutputError, Source};
use std::sync::Mutex;

/// Left channel = the source's frame index, right channel = 1.0. Any gain applied downstream
/// scales both, so `left / right` recovers the index of every non-silent frame.
struct Counter(u64);

impl Source for Counter {
    fn render(&mut self, out: &mut [f32]) {
        for f in out.chunks_exact_mut(2) {
            f[0] = self.0 as f32;
            f[1] = 1.0;
            self.0 += 1;
        }
    }
}

/// An output that records what the device took. `script[n]` makes write number `n` (counting
/// every call) take `k` frames and then fail with the given fault, as a partial `snd_pcm_writei`
/// followed by a failing one does.
struct Recorder {
    granted: Granted,
    calls: usize,
    script: Vec<(usize, usize, Fault)>,
    taken: Arc<Mutex<Vec<[f32; 2]>>>,
}

impl Recorder {
    fn new(script: Vec<(usize, usize, Fault)>) -> (Self, Arc<Mutex<Vec<[f32; 2]>>>) {
        let taken = Arc::new(Mutex::new(Vec::with_capacity(200_000)));
        let r = Self {
            granted: Granted {
                format: common::config::SampleFormat::S32Le,
                rate: 48_000,
                channels: 2,
                period_frames: 1024,
                periods: 8,
                buffer_frames: 8192,
            },
            calls: 0,
            script,
            taken: taken.clone(),
        };
        (r, taken)
    }
}

impl Output for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }
    fn granted(&self) -> &Granted {
        &self.granted
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError> {
        let call = self.calls;
        self.calls += 1;
        let fail = self.script.iter().find(|s| s.0 == call).copied();
        let take = fail.map_or(samples.len() / 2, |(_, k, _)| k);
        let mut t = self.taken.lock().unwrap();
        for f in samples[..take * 2].chunks_exact(2) {
            t.push([f[0], f[1]]);
        }
        match fail {
            Some((_, k, fault)) => {
                Err(OutputError::new(fault, "scripted").with_frames_done(k as u32))
            }
            None => Ok(()),
        }
    }
    fn delay_frames(&mut self) -> Option<i64> {
        Some(0)
    }
    fn recover(&mut self, _: Fault) -> Result<(), OutputError> {
        Ok(())
    }
    fn reopen(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Runs `frames` source frames through a scripted `Recorder`; returns what the device took.
fn run_scripted(script: Vec<(usize, usize, Fault)>, frames: u64) -> (Vec<[f32; 2]>, Stats) {
    let clock = Arc::new(VirtualClock::new(0));
    let (rec, taken) = Recorder::new(script);
    let opts = RtOptions {
        max_frames: Some(frames),
        recovery_retry_ns: 1_000_000,
        ..RtOptions::default()
    };
    let h = RtHandle::start(Box::new(rec), Box::new(Counter(0)), clock, opts).unwrap();
    while !h.is_finished() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let s = h.stats();
    h.wait();
    let t = taken.lock().unwrap().clone();
    (t, s)
}

/// Every source frame reaches the device exactly once and in order; silence is exact zero.
fn check_each_frame_once(taken: &[[f32; 2]], frames: u64) {
    let mut next = 0u64;
    for (i, f) in taken.iter().enumerate() {
        if f[1] == 0.0 {
            assert_eq!(f[0], 0.0, "frame {i}: silence must be silent");
            continue;
        }
        let idx = (f64::from(f[0]) / f64::from(f[1])).round() as u64;
        assert_eq!(
            idx, next,
            "device frame {i}: source frame {idx}, expected {next}"
        );
        next += 1;
    }
    assert_eq!(next, frames, "source frames that reached the device");
}

#[test]
fn partial_write_before_xrun_is_not_sent_twice_and_is_declicked() {
    // Write 20 takes 300 of 1024 frames, then underruns.
    let frames = 48_000;
    let (taken, s) = run_scripted(vec![(20, 300, Fault::Xrun)], frames);
    check_each_frame_once(&taken, frames);
    assert_eq!(s.xruns, 1);
    // One buffer of silence was inserted, nothing else.
    let silent = taken.iter().filter(|f| f[1] == 0.0).count();
    println!(
        "partial xrun: {} device frames, {silent} silent (expect 8192), frames_written {}",
        taken.len(),
        s.frames_written
    );
    assert_eq!(silent, 8192);
    assert_eq!(s.frames_written, frames + 8192);
    assert_eq!(taken.len() as u64, s.frames_written);
    // The audio after the gap fades in over 10 ms (480 frames): no step from silence.
    let first = 20 * 1024 + 300 + 8192;
    assert_eq!(taken[first - 1][1], 0.0);
    let g0 = taken[first][1];
    let g_mid = taken[first + 239][1];
    let g_end = taken[first + 479][1];
    println!("gain after the gap: {g0:.5}, {g_mid:.4} at 5 ms, {g_end:.4} at 10 ms");
    assert!(g0 <= 1.0 / 400.0, "first frame after the gap has gain {g0}");
    assert!((g_mid - 0.5).abs() < 0.01);
    assert_eq!(g_end, 1.0);
}

#[test]
fn partial_write_before_device_loss_is_not_sent_twice_and_fades_in() {
    let frames = 48_000;
    let (taken, s) = run_scripted(vec![(12, 700, Fault::Lost)], frames);
    check_each_frame_once(&taken, frames);
    assert_eq!((s.device_lost, s.device_restored), (1, 1));
    // No silence on a device loss; the resumed audio fades in over 150 ms (7,200 frames).
    let first = 12 * 1024 + 700;
    let g0 = taken[first][1];
    let g_half = taken[first + 3599][1];
    println!("gain after the loss: {g0:.6}, {g_half:.4} at 75 ms");
    assert!(g0 <= 1.0 / 7000.0);
    assert!((g_half - 0.5).abs() < 0.01);
    assert_eq!(taken[first + 7199][1], 1.0);
}

#[test]
fn fault_while_fading_in_does_not_restart_or_repeat_the_fade() {
    // An xrun with nothing taken, then a partial one on the first block after the silence
    // refill (inside the first fade): no frame repeats or goes missing.
    let frames = 60_000;
    let script = vec![(10, 0, Fault::Xrun), (10 + 1 + 8 + 1, 100, Fault::Xrun)];
    let (taken, s) = run_scripted(script, frames);
    check_each_frame_once(&taken, frames);
    assert_eq!(s.xruns, 2);
}
