//! ALSA backend tests (Linux only). The first runs everywhere ALSA's `null` plugin exists (it does
//! in a plain Docker container: no sound card needed) and exercises the real hw_params
//! negotiation, sw_params, writei and prepare paths. The `#[ignore]` tests need a real card and are
//! the M1.1 hardware acceptance tests 3 and 4; see `docs/hardware-tests.md`.
#![cfg(target_os = "linux")]

use common::{Clock, RealClock};
use engine::alsa_out::AlsaOutput;
use engine::fault::Fault;
use engine::rt::{Command, CommandKind, EventKind, RtHandle, RtOptions, RtState};
use engine::{HwRequest, Output, ToneSource};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn device() -> String {
    std::env::var("HOTCLOCK_TEST_DEVICE").unwrap_or_else(|_| "hw:CARD=PCH,DEV=0".to_string())
}

#[test]
fn alsa_null_plugin_negotiates_and_writes() {
    let req = HwRequest::defaults();
    let mut out = match AlsaOutput::open("null", &req) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIPPED: ALSA `null` device not available here: {e}");
            return;
        }
    };
    let g = out.granted().clone();
    println!("ALSA null plugin granted: {}", g.describe());
    assert_eq!(g.channels, 2);
    assert!(g.period_frames > 0 && g.periods >= 2);
    assert_eq!(g.buffer_frames, g.period_frames * g.periods);
    let block = vec![0.25f32; g.period_frames as usize * 2];
    for _ in 0..20 {
        out.write(&block).unwrap();
    }
    let _ = out.delay_frames();
    out.recover(Fault::Xrun).unwrap();
    out.write(&block).unwrap();
    out.reopen().unwrap();
    out.write(&block).unwrap();
}

#[test]
fn opening_a_missing_card_fails_readably() {
    let e = match AlsaOutput::open("hw:CARD=DoesNotExist,DEV=0", &HwRequest::defaults()) {
        Ok(_) => panic!("opening a missing card must fail"),
        Err(e) => e.to_string(),
    };
    println!("{e}");
    assert!(e.contains("DoesNotExist"), "{e}");
}

/// The rt-out thread driving the real ALSA code path (through the `null` plugin).
#[test]
fn rt_thread_over_alsa_null_plugin() {
    let clock = Arc::new(RealClock::new());
    let Ok(out) = AlsaOutput::open("null", &HwRequest::defaults()) else {
        eprintln!("SKIPPED: ALSA `null` device not available here");
        return;
    };
    let opts = RtOptions {
        max_frames: Some(48_000),
        ..RtOptions::default()
    };
    let h = RtHandle::start(
        Box::new(out),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock,
        opts,
    )
    .unwrap();
    let t0 = Instant::now();
    while !h.is_finished() && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = h.stats();
    println!(
        "rt over ALSA null: {} source frames in {:.2} s, xruns {}",
        s.source_frames,
        t0.elapsed().as_secs_f64(),
        s.xruns
    );
    assert_eq!(s.source_frames, 48_000);
    h.wait();
}

// ---- hardware (the target box): cargo test -p engine --test alsa -- --ignored --nocapture ----

/// Acceptance 3, first half: a clean tone through `hw:CARD=...`; the granted parameters are
/// printed. Listen to it; the test only checks that it ran for 10 s with no xrun.
#[test]
#[ignore = "needs a sound card: set HOTCLOCK_TEST_DEVICE"]
fn hardware_tone_is_clean() {
    let clock = Arc::new(RealClock::new());
    let out = AlsaOutput::open(&device(), &HwRequest::defaults()).expect("open the card");
    println!("granted: {}", out.granted().describe());
    let rate = out.granted().rate;
    let opts = RtOptions {
        realtime_priority: Some(20),
        ..RtOptions::default()
    };
    let h = RtHandle::start(
        Box::new(out),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock.clone(),
        opts,
    )
    .unwrap();
    println!("priority: {:?}", h.priority().map(|p| p.describe()));
    let t0 = clock.monotonic_ns();
    std::thread::sleep(Duration::from_secs(10));
    let s = h.stats();
    // The published position is up to one period old; extrapolate it to the same instant as
    // the elapsed time it is compared with.
    let now = clock.monotonic_ns();
    let audible = s.audible_frames_at(now, rate) as f64;
    let expected = (now - t0) as f64 * f64::from(rate) / 1e9;
    println!(
        "10 s: audible {audible} vs {expected:.0} elapsed (limit +-0.1 %), xruns {} (limit 0), \
         max write {:.1} ms",
        s.xruns,
        s.max_write_ns as f64 / 1e6
    );
    assert_eq!(s.xruns, 0);
    assert!((audible - expected).abs() < expected * 0.001);
    h.stop();
}

/// Acceptance 4: a writer stalled for 500 ms causes one counted xrun, recovered; the tone goes on.
#[test]
#[ignore = "needs a sound card: set HOTCLOCK_TEST_DEVICE"]
fn hardware_stall_causes_one_recovered_xrun() {
    let clock = Arc::new(RealClock::new());
    let out = AlsaOutput::open(&device(), &HwRequest::defaults()).expect("open the card");
    let mut h = RtHandle::start(
        Box::new(out),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock,
        RtOptions::default(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    h.send(Command::now(CommandKind::Stall { ms: 500 }));
    std::thread::sleep(Duration::from_secs(3));
    let s = h.stats();
    println!(
        "after a 500 ms stall: xruns {} (limit exactly 1), state {:?}",
        s.xruns, s.state
    );
    assert_eq!(s.xruns, 1);
    assert_eq!(s.state, RtState::Playing);
    while let Some(e) = h.poll_event() {
        println!("event: {e:?}");
    }
    h.stop();
}

/// Acceptance 3, second half, by hand: pull the USB dongle when prompted, plug it back in. The
/// test passes if `DeviceLost` is seen, the process survives, and `DeviceRestored` follows within
/// 60 s; read the elapsed time between your plugging in and the "restored" line for the 5 s limit
/// (the retry interval is 2 s). Run with `HOTCLOCK_TEST_INTERACTIVE=1`.
#[test]
#[ignore = "interactive: unplug and replug the USB card when prompted"]
fn hardware_unplug_and_replug() {
    if std::env::var("HOTCLOCK_TEST_INTERACTIVE").is_err() {
        eprintln!("set HOTCLOCK_TEST_INTERACTIVE=1 to run this test");
        return;
    }
    let clock = Arc::new(RealClock::new());
    let out = AlsaOutput::open(&device(), &HwRequest::defaults()).expect("open the card");
    let mut h = RtHandle::start(
        Box::new(out),
        Box::new(ToneSource::new(48_000, 1000.0, -20.0)),
        clock.clone(),
        RtOptions::default(),
    )
    .unwrap();
    println!(">>> Tone is playing. PULL THE USB DONGLE now, wait ~5 s, then PLUG IT BACK IN.");
    let start = clock.monotonic_ns();
    let (mut lost_at, mut restored_at) = (None, None);
    while (clock.monotonic_ns() - start) < 90_000_000_000 && restored_at.is_none() {
        while let Some(e) = h.poll_event() {
            let t = (clock.monotonic_ns() - start) as f64 / 1e9;
            println!("[{t:6.2} s] {:?}", e.kind);
            match e.kind {
                EventKind::DeviceLost => lost_at = Some(t),
                EventKind::DeviceRestored { outage_ms } => {
                    println!("restored after an outage of {outage_ms} ms; the frame counter stood still meanwhile");
                    restored_at = Some(t)
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        lost_at.is_some(),
        "no DeviceLost seen: was the card unplugged?"
    );
    assert!(restored_at.is_some(), "the tone did not resume within 90 s");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(h.stats().state, RtState::Playing);
    h.stop();
}
