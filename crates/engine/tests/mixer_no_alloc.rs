//! M1.2 acceptance 6: no allocation on rt-out. A global allocator counts every allocation, free
//! and reallocation made by a thread that has registered as the rt thread; the test asserts the
//! count is zero after **10 minutes of mixed playback** through the real rt-out thread with the
//! mixer: voices from real decoders (WAV, FLAC, MP3, ALAC; 44.1, 48 and 96 kHz; mono and
//! stereo), decoded and resampled by the pool, overlapping, fading (cosine envelopes), stopping
//! with declick, ending at end of file, master ramps, the safety limiter working, xruns
//! injected, and a decode glitch (`corrupt.mp3`). 28,800,000 frames.

#![allow(clippy::needless_range_loop, clippy::manual_is_multiple_of)]

mod support;

use common::VirtualClock;
use engine::envelope::{Curve, Segment};
use engine::mixer::{master_ramp, LoadParams, Mixer, StopFade};
use engine::output::{NullOutput, Output, OutputError};
use engine::pool::{DecoderPool, VoiceSource};
use engine::rt::{RtHandle, RtOptions};
use engine::testing::FaultyOutput;
use engine::{Granted, HwRequest};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::Arc;

thread_local! {
    static IS_RT: Cell<bool> = const { Cell::new(false) };
}
static RT_ALLOCS: AtomicU64 = AtomicU64::new(0);
static SIZES: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
fn note(kind: u64, size: usize) {
    let i = RT_ALLOCS.fetch_add(1, Relaxed) as usize;
    if i < SIZES.len() {
        SIZES[i].store(kind << 56 | size as u64, Relaxed);
    }
}

struct Counting;

// SAFETY: forwards every call to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(1, l.size());
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(2, l.size());
        }
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if IS_RT.try_with(|f| f.get()).unwrap_or(false) {
            note(3, l.size());
        }
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Wraps an output and makes each write take a little real time, so that the virtual-clock
/// device runs at a fixed multiple of real time instead of flat out. Test only.
struct Slow {
    inner: Box<dyn Output>,
    per_write: std::time::Duration,
}

impl Output for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn granted(&self) -> &Granted {
        self.inner.granted()
    }
    fn write(&mut self, s: &[f32]) -> Result<(), OutputError> {
        std::thread::sleep(self.per_write);
        self.inner.write(s)
    }
    fn delay_frames(&mut self) -> Option<i64> {
        self.inner.delay_frames()
    }
    fn recover(&mut self, f: engine::fault::Fault) -> Result<(), OutputError> {
        self.inner.recover(f)
    }
    fn reopen(&mut self) -> Result<(), String> {
        self.inner.reopen()
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % m
    }
}

/// Files to play: always three generated WAVs (44.1 kHz mono, 48 kHz stereo, 96 kHz stereo)
/// and, when `ffmpeg` is available, the FLAC, ALAC and MP3 fixtures.
fn sources() -> Vec<PathBuf> {
    use fixtures::signals::{pink, sine};
    let dir = support::out_dir().join("alloc-test");
    std::fs::create_dir_all(&dir).unwrap();
    let mut v = vec![];
    for (name, rate, stereo, secs) in [
        ("w44_mono.wav", 44_100u32, false, 14.0),
        ("w48_st.wav", 48_000, true, 17.0),
        ("w96_st.wav", 96_000, true, 9.0),
    ] {
        let p = dir.join(name);
        let mut x = pink(rate, -16.0, secs, u64::from(rate));
        let s = sine(rate, 440.0, -16.0, secs);
        for (a, b) in x.iter_mut().zip(&s) {
            *a = 0.5 * (*a + *b);
        }
        fixtures::files::write_wav24(&p, &x, rate, stereo).unwrap();
        v.push(p);
    }
    let names = [
        "rate_44k.flac",
        "rate_96k.flac",
        "flac16.flac",
        "alac.m4a",
        "corrupt.mp3",
        "mp4_named_aac.aac",
    ];
    let (fdir, report) = fixtures::shared_dir(&names);
    if report.skipped.is_empty() {
        v.extend(names.iter().map(|n| fdir.join(n)));
    } else {
        eprintln!("note: ffmpeg fixtures unavailable, WAV sources only");
    }
    v
}

#[test]
fn rt_out_never_allocates_in_10_minutes_of_mixed_playback() {
    let files = sources();
    let rate = 48_000u32;
    let total_frames = 10 * 60 * u64::from(rate);
    let speed = 25u32; // times real time
    let block = std::time::Duration::from_secs_f64(1024.0 / f64::from(rate) / f64::from(speed));

    let clock = Arc::new(VirtualClock::new(0));
    let null = NullOutput::new(&HwRequest::defaults(), clock.clone());
    let (faulty, ctl_fault) = FaultyOutput::new(Box::new(Slow {
        inner: Box::new(null),
        per_write: block,
    }));
    let (mixer, mut ctl) = Mixer::new(rate);
    // The loudness controller's path (M6a) runs under the same allocator: gain stage, true-peak limiter at a
    // ceiling that changes, the loudness tap and the block-info ring. The main thread stands in for the meter
    // thread: it publishes a different gain every second and drains the taps in alternate 10 s stretches, so
    // the rings are exercised both draining and full (and dropping).
    let link = ctl.loudness_link();
    link.enabled.store(true, Relaxed);
    link.set_gain_db(4.0);
    link.set_ceiling_db(-1.0);
    // The processing chain (M6): every block, swapped every two seconds between six whole configurations from the
    // main thread (which allocates the new chain; rt-out only exchanges pointers, crossfades and hands the old one
    // back), so the AGC, the multiband at three band counts, the optional blocks, the EQ's two positions and the
    // clipper all run under the allocator and through swaps.
    let chain = ctl.chain_control();
    let cfgs: Vec<common::chain::ChainConfig> = {
        use common::chain::preset_config;
        let mut all = preset_config("radio").unwrap();
        for (b, v) in [
            ("highpass", serde_json::json!({"enabled": true})),
            ("rotator", serde_json::json!({"enabled": true})),
            ("stereo", serde_json::json!({"enabled": true})),
            ("bass", serde_json::json!({"enabled": true})),
            ("wideband", serde_json::json!({"enabled": true})),
            ("clipper", serde_json::json!({"enabled": true})),
            (
                "eq",
                serde_json::json!({"enabled": true, "low_db": 3.0, "mid2_db": 2.0, "position": "post"}),
            ),
            (
                "multiband",
                serde_json::json!({"crossovers_hz": [80.0, 300.0, 1000.0, 3000.0, 9000.0], "threshold_dbfs": [-26.0, -26.0, -24.0, -22.0, -22.0, -24.0], "ratio": [3.0, 3.0, 3.0, 3.0, 3.0, 3.0], "knee_db": [6.0, 6.0, 6.0, 6.0, 6.0, 6.0], "attack_ms": [40.0, 30.0, 20.0, 10.0, 6.0, 4.0], "release_ms": [400.0, 350.0, 250.0, 200.0, 150.0, 120.0], "makeup_db": [3.0, 3.0, 3.0, 3.0, 3.0, 3.0]}),
            ),
        ] {
            all.apply_block(b, v.as_object().unwrap()).unwrap();
        }
        vec![
            preset_config("radio").unwrap(),
            preset_config("gentle").unwrap(),
            all,
            preset_config("bypass").unwrap(),
            preset_config("radio").unwrap(),
            preset_config("gentle").unwrap(),
        ]
    };
    chain.apply(&cfgs[0]).unwrap();
    let mut last_cfg = 0usize;
    let mut tap = ctl.take_meter_tap().unwrap();
    let mut drained = 0u64;
    let pool = DecoderPool::new(3, rate);

    // Voices for the first seconds are loaded before start; the rest by the loop below.
    let opts = RtOptions {
        max_frames: Some(total_frames),
        on_start: Some(Arc::new(|| IS_RT.with(|f| f.set(true)))),
        on_exit: Some(Arc::new(|| IS_RT.with(|f| f.set(false)))),
        ..RtOptions::default()
    };
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut scheduled_until = 3 * u64::from(rate); // start frame of the next voice
    let mut n_loaded = 0u32;
    let mut cmds = 0u32;
    let mut next_master = 20 * u64::from(rate);
    let mut xrun_at = 30u64;

    let schedule = |ctl: &mut engine::mixer::MixerControl,
                    h: Option<&mut RtHandle>,
                    scheduled_until: &mut u64,
                    n_loaded: &mut u32,
                    cmds: &mut u32,
                    rng: &mut Rng| {
        let path = files[(*n_loaded as usize) % files.len()].clone();
        let start = *scheduled_until;
        let via_preload = *n_loaded % 3 == 0;
        let feed = if via_preload {
            // Head preload, then continue.
            let ph = pool.preload(path.clone(), 0.0, 4.0);
            let t0 = std::time::Instant::now();
            loop {
                if let Some(Ok(p)) = ph.take() {
                    break pool.open(VoiceSource::Preload(p));
                }
                if t0.elapsed().as_secs() > 20 {
                    panic!("preload of {} timed out", path.display());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        } else {
            pool.open(VoiceSource::File {
                path: path.clone(),
                start_s: 0.0,
            })
        };
        let gain = if *n_loaded % 4 == 1 { 2.5 } else { 1.0 }; // some voices drive the limiter
        let id = match ctl.load(
            feed,
            LoadParams {
                gain,
                declick_in: *n_loaded % 5 != 0,
                ..LoadParams::at(start)
            },
        ) {
            Ok(id) => id,
            Err(_) => return false,
        };
        *n_loaded += 1;
        if let Some(h) = h {
            let dur = 3 + rng.next(7); // seconds of this voice unless it ends first
            let mode = *n_loaded % 3;
            let end = start + dur * u64::from(rate);
            if mode == 0 {
                // fade out with a cosine and stop
                let fade = 2 * u64::from(rate);
                h.send(id.fade_out(end - fade, fade));
                *cmds += 1;
            } else if mode == 1 {
                h.send(id.stop(end, StopFade::Declick));
                *cmds += 1;
            } // mode 2: plays to the end of its file
            if rng.next(2) == 0 {
                h.send(id.envelope(Segment {
                    start: start + u64::from(rate),
                    end: start + 2 * u64::from(rate),
                    from: 1.0,
                    to: 0.7,
                    curve: Curve::EqualPower,
                }));
                *cmds += 1;
            }
        }
        *scheduled_until = start + (2 + rng.next(5)) * u64::from(rate);
        true
    };

    // The first voice is loaded before the thread starts (no commands: it plays to its end).
    schedule(
        &mut ctl,
        None,
        &mut scheduled_until,
        &mut n_loaded,
        &mut cmds,
        &mut rng,
    );
    let mut h = RtHandle::start(Box::new(faulty), Box::new(mixer), clock, opts).unwrap();

    let started = std::time::Instant::now();
    while !h.is_finished() {
        while h.poll_event().is_some() {}
        ctl.pump();
        let pos = h.stats().source_frames;
        let sec = pos / u64::from(rate);
        let k = (sec / 2) as usize % cfgs.len();
        if k != last_cfg {
            last_cfg = k;
            chain.apply(&cfgs[k]).unwrap();
        }
        let _ = chain.meters.take();
        link.set_gain_db(f64::from((sec % 9) as u32) - 3.0);
        link.set_ceiling_db(if (sec / 25) % 2 == 0 { -1.0 } else { -2.5 });
        link.playing.store((sec / 15) % 4 != 3, Relaxed);
        if (sec / 10) % 2 == 0 {
            let mut buf = vec![0.0f32; 1 << 14];
            while let Ok(n) = tap.post.read_chunk(tap.post.slots().min(buf.len())) {
                let k = n.len();
                if k == 0 {
                    break;
                }
                let (a, b) = n.as_slices();
                buf[..a.len()].copy_from_slice(a);
                buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                n.commit_all();
                drained += k as u64;
            }
            while let Ok(n) = tap.loud.read_chunk(tap.loud.slots()) {
                if n.is_empty() {
                    break;
                }
                n.commit_all();
            }
            while let Ok(n) = tap.info.read_chunk(tap.info.slots()) {
                if n.is_empty() {
                    break;
                }
                n.commit_all();
            }
            while let Ok(n) = tap.pre.read_chunk(tap.pre.slots()) {
                if n.is_empty() {
                    break;
                }
                n.commit_all();
            }
        }
        // Keep the schedule 12 s ahead of the output.
        while scheduled_until < pos + 12 * u64::from(rate) && ctl.busy_voices() < 7 {
            if scheduled_until < pos + 3 * u64::from(rate) {
                scheduled_until = pos + 3 * u64::from(rate); // never closer than 3 s
            }
            if !schedule(
                &mut ctl,
                Some(&mut h),
                &mut scheduled_until,
                &mut n_loaded,
                &mut cmds,
                &mut rng,
            ) {
                break;
            }
        }
        if pos >= next_master {
            h.send(master_ramp(Segment {
                start: pos + 3 * u64::from(rate),
                end: pos + 5 * u64::from(rate),
                from: 1.0,
                to: 0.8,
                curve: Curve::Cosine,
            }));
            h.send(master_ramp(Segment {
                start: pos + 6 * u64::from(rate),
                end: pos + 7 * u64::from(rate),
                from: 0.8,
                to: 1.0,
                curve: Curve::Linear,
            }));
            cmds += 2;
            next_master = pos + 40 * u64::from(rate);
        }
        // An injected xrun every 40 s of audio or so.
        if pos / (40 * u64::from(rate)) >= xrun_at / 40 && xrun_at < 600 {
            ctl_fault.xrun_next.store(1, Release);
            xrun_at += 40;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let wall = started.elapsed().as_secs_f64();
    let s = h.stats();
    let allocs = RT_ALLOCS.load(Relaxed);
    let m = ctl.stats();
    h.stop();
    ctl.pump();
    println!(
        "rt-out allocations, frees and reallocations over {} frames (10 min, {speed}x real time, {wall:.1} s wall): {allocs} (limit 0)",
        s.source_frames
    );
    let swaps = chain.meters.swaps.load(Relaxed);
    let leaks = chain.meters.leaks.load(Relaxed);
    println!("processing chain: {swaps} whole-chain swaps (AGC, 2 to 6 bands, every optional block, EQ pre and post) during the run, {leaks} chains leaked");
    assert!(swaps >= 250 && leaks == 0);
    for (i, v) in SIZES.iter().enumerate().take(allocs.min(16) as usize) {
        let v = v.load(Relaxed);
        println!(
            "  #{i}: kind {} (1 alloc, 2 free, 3 realloc) size {}",
            v >> 56,
            v & 0xFF_FFFF_FFFF_FFFF
        );
    }
    println!(
        "mixed playback: {} voices loaded, {} started, {} ended, {} commands sent; xruns {}; \
         mixer out frame {}; starved frames {} in {} episodes; late starts {}; stale commands {}; \
         garbage stalls {}, leaks {}; envelope drops {}; limiter active {} frames ({} over 1 dB), min gain {:.3}",
        n_loaded,
        m.voices_started,
        m.voices_ended,
        cmds,
        s.xruns,
        m.out_frame,
        m.starved_frames,
        m.starve_episodes,
        m.late_starts,
        m.stale_commands,
        m.garbage_stalls,
        m.garbage_leaks,
        m.envelope_dropped,
        m.limiter_active_frames,
        m.limiter_over_1db_frames,
        m.limiter_min_gain
    );
    println!(
        "controller path: {drained} tap samples drained, {} tap blocks dropped while the main thread was not draining, true-peak limiter min gain {:.3}",
        m.tap_drops,
        f32::from_bits(link.tp_min_gain_bits.load(Relaxed))
    );
    assert!(
        drained > 0 && m.tap_drops > 0,
        "both ring regimes were exercised"
    );
    assert_eq!(allocs, 0);
    // The detector itself works: an allocation on a thread marked as rt-out is counted.
    IS_RT.with(|f| f.set(true));
    let probe = std::hint::black_box(vec![0u8; 64]);
    drop(probe);
    IS_RT.with(|f| f.set(false));
    assert!(
        RT_ALLOCS.load(Relaxed) >= 2,
        "the counting allocator must see an alloc and a free"
    );
    assert_eq!(s.source_frames, total_frames);
    assert!(n_loaded >= 60, "enough voices came and went: {n_loaded}");
    assert!(s.xruns >= 10);
    // The voices that drive the limiters are caught by the true-peak limiter first (M6a: the controller is on),
    // so the safety limiter behind it need not act.
    assert!(
        m.limiter_active_frames > 0 || f32::from_bits(link.tp_min_gain_bits.load(Relaxed)) < 1.0
    );
    assert_eq!(m.garbage_leaks, 0);
    assert_eq!(m.load_rejected, 0);
}
