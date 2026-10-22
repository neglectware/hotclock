//! M1.2 acceptance tests 1 to 5 and 7, plus envelope, gapless-continuity and preload checks.
//! Each test prints its measured numbers next to the limit; BUILDLOG copies them.
//! The mixer is driven directly (no rt thread) where determinism matters, and through the real
//! rt-out thread where the integration matters.

#![allow(clippy::needless_range_loop, clippy::manual_is_multiple_of)]

mod support;

use engine::envelope::{Curve, Segment};
use engine::feed::VoiceFeed;
use engine::mixer::{EndReason, LoadParams, Mixer, MixerEvent, StopFade};
use engine::pool::{DecoderPool, VoiceSource, PRELOAD_SECONDS};
use engine::rt::{CommandKind, RtHandle, RtOptions};
use engine::source::Source;
use std::sync::Arc;
use support::*;

fn sine_samples(hz: f64, amp: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| {
            let s = (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin()) as f32;
            [s, s]
        })
        .collect()
}

/// Render `total` frames from `mixer` in blocks of the given sizes (cycled).
fn render(mixer: &mut Mixer, total: usize, sizes: &[usize]) -> Vec<f32> {
    let mut out = Vec::with_capacity(total * 2);
    let mut i = 0;
    while out.len() < total * 2 {
        let n = sizes[i % sizes.len()].min(total - out.len() / 2);
        i += 1;
        let start = out.len();
        out.resize(start + n * 2, 0.0);
        mixer.render(&mut out[start..]);
    }
    out
}

fn drain_events(ctl: &mut engine::mixer::MixerControl) -> Vec<MixerEvent> {
    let mut v = vec![];
    while let Some(e) = ctl.poll_event() {
        v.push(e);
    }
    v
}

// ---- acceptance 1 -------------------------------------------------------------------------

/// `rate_44k.flac` and `rate_96k.flac` rendered at 48 kHz: 1 kHz within 0.1 Hz, THD+N below
/// -90 dB. Measured over the interior (0.3 s trimmed at each end): the file's own start and end
/// are a step from and to silence, whose filtering by the resampler is not distortion.
#[test]
fn acceptance_1_resampled_tone_frequency_and_thdn() {
    let Some(dir) = fixture_dir(&["rate_44k.flac", "rate_96k.flac"]) else {
        return;
    };
    for (name, src_rate) in [("rate_44k.flac", 44_100u64), ("rate_96k.flac", 96_000)] {
        let (out, rep) = render_files(&[dir.join(name)]);
        let l = left(&out);
        let expect = (5 * src_rate * 48_000 + src_rate / 2) / src_rate / 5; // 5 s
        let expect = (expect as f64 / 5.0 * 5.0) as u64;
        let want_frames = ((5 * src_rate) as f64 * 48_000.0 / src_rate as f64).round() as u64;
        assert_eq!(l.len() as u64, want_frames, "{name}: length");
        let _ = expect;
        let trim = (0.3 * 48_000.0) as usize;
        let fit = fit_sine(&l[trim..l.len() - trim], 48_000, 1000.0, 5.0);
        println!(
            "{name} -> 48 kHz: {} frames (exact {want_frames}); frequency {:.4} Hz (error {:.4} Hz, limit 0.1); \
             level {:.3} dBFS RMS (source -14.000); THD+N {:.1} dB (limit -90); glitches {}",
            l.len(),
            fit.freq_hz,
            (fit.freq_hz - 1000.0).abs(),
            fit.level_dbfs,
            fit.thdn_db,
            rep.files[0].glitches
        );
        assert!((fit.freq_hz - 1000.0).abs() < 0.1, "{name}");
        assert!(fit.thdn_db < -90.0, "{name}: {}", fit.thdn_db);
        assert_eq!(rep.files[0].glitches, 0);
    }
}

// ---- acceptance 2 -------------------------------------------------------------------------

/// `mono.wav` (44.1 kHz, 1 kHz at -20 dBFS RMS) renders with identical left and right, and its
/// level within 0.05 dB of the source.
#[test]
fn acceptance_2_mono_is_dual_mono_at_source_level() {
    let Some(dir) = fixture_dir(&["mono.wav"]) else {
        return;
    };
    let (out, _) = render_files(&[dir.join("mono.wav")]);
    let l = left(&out);
    let identical = out.chunks_exact(2).all(|f| f[0] == f[1]);
    let trim = (0.3 * 48_000.0) as usize;
    let mid = &l[trim..l.len() - trim];
    let level = rms_db(mid);
    let fit = fit_sine(mid, 48_000, 1000.0, 5.0);
    println!(
        "mono.wav: {} frames; left == right for every frame: {identical}; level {level:.4} dBFS \
         (source -20.0000, error {:.4} dB, limit 0.05); fitted {:.4} dBFS at {:.3} Hz",
        l.len(),
        (level + 20.0).abs(),
        fit.level_dbfs,
        fit.freq_hz
    );
    assert!(identical);
    assert!((level + 20.0).abs() < 0.05);
    assert_eq!(l.len(), (10.0f64 * 48_000.0).round() as usize);
}

// ---- acceptance 3 -------------------------------------------------------------------------

fn first_nonzero(out: &[f32]) -> Option<usize> {
    out.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0)
}

/// A voice commanded to start at frame F has its first non-zero sample at F, in 100 randomized
/// trials (random F, random block sizes, with and without the declick in).
#[test]
fn acceptance_3_start_is_sample_exact_in_100_random_trials() {
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut rnd = move |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    let mut worst = 0i64;
    for trial in 0..100 {
        let (mut mixer, mut ctl) = Mixer::new(48_000);
        // Includes starts inside the limiter lookahead of frame 0 (frame 0 itself too).
        let f = if trial < 5 { trial as u64 } else { rnd(30_000) };
        let dc = vec![0.25f32; 2 * 4000];
        let declick = trial % 2 == 0;
        let id = ctl
            .load(
                VoiceFeed::from_samples(&dc),
                LoadParams {
                    declick_in: declick,
                    ..LoadParams::at(f)
                },
            )
            .unwrap();
        let sizes: Vec<usize> = (0..8).map(|_| 1 + rnd(1024) as usize).collect();
        let total = f as usize + 4200;
        let out = render(&mut mixer, total, &sizes);
        let got = first_nonzero(&out).expect("voice never sounded") as i64;
        worst = worst.max((got - f as i64).abs());
        assert_eq!(
            got, f as i64,
            "trial {trial}: commanded {f}, first sample at {got}"
        );
        // No sound before F, and the event says the same.
        let ev = drain_events(&mut ctl);
        let started = ev
            .iter()
            .find_map(|e| match e {
                MixerEvent::Started { frame, voice, gen }
                    if *voice == id.slot && *gen == id.gen =>
                {
                    Some(*frame)
                }
                _ => None,
            })
            .expect("Started event");
        assert_eq!(started, f, "trial {trial}");
        // The whole 4000 frames played, and the voice ended exactly there.
        let ended = ev
            .iter()
            .find_map(|e| match e {
                MixerEvent::Ended {
                    frame,
                    reason,
                    played,
                    ..
                } => Some((*frame, *reason, *played)),
                _ => None,
            })
            .expect("Ended event");
        assert_eq!(ended, (f + 4000, EndReason::Eof, 4000), "trial {trial}");
    }
    println!("100 random starts: worst error {worst} frames (limit 0)");
    assert_eq!(worst, 0);
}

/// The same through the real rt-out thread and the `File` output: voices loaded before the
/// thread starts, first non-zero sample read back from the WAV.
#[test]
fn acceptance_3_start_is_sample_exact_through_rt_out() {
    use engine::output::FileOutput;
    use engine::wav::{read_wav, WavFormat};
    let mut worst = 0i64;
    for (trial, f) in [0u64, 1, 95, 96, 97, 1023, 1024, 1025, 4999, 12_345, 20_000]
        .into_iter()
        .enumerate()
    {
        let (mixer, mut ctl) = Mixer::new(48_000);
        ctl.load(
            VoiceFeed::from_samples(&vec![0.5f32; 2 * 3000]),
            LoadParams::at(f),
        )
        .unwrap();
        let path = out_dir().join(format!("start-{trial}.wav"));
        let clock = Arc::new(common::RealClock::new());
        let out = FileOutput::create(&path, &engine::HwRequest::defaults(), WavFormat::F32, None)
            .unwrap();
        let opts = RtOptions {
            max_frames: Some(f + 3500),
            ..RtOptions::default()
        };
        let h = RtHandle::start(Box::new(out), Box::new(mixer), clock, opts).unwrap();
        let mut o = h.wait();
        o.finish().unwrap();
        let wav = read_wav(&path).unwrap();
        let got = first_nonzero(&wav.samples).unwrap() as i64;
        worst = worst.max((got - f as i64).abs());
        assert_eq!(got, f as i64, "commanded {f}");
    }
    println!("rt-out start trials: worst error {worst} frames (limit 0)");
}

// ---- acceptance 4 -------------------------------------------------------------------------

/// A voice stopped mid-sine shows no sample-to-sample step larger than the sine's own maximum
/// step plus 0.01, at the stop and at the start; a cut without the declick would.
#[test]
fn acceptance_4_declick_leaves_no_step() {
    let amp = 0.5f64;
    let hz = 1000.0;
    let own_max_step = amp * 2.0 * (std::f64::consts::PI * hz / 48_000.0).sin();
    let limit = own_max_step + 0.01;
    let mut worst_declick = 0.0f64;
    let mut worst_cut = 0.0f64;
    // Stop at many phases of the sine, in particular at a positive peak (frame 12 = 1/4 cycle).
    for k in 0..48 {
        let stop_at = 10_000 + k as u64; // 48 different phases of a 48-frame cycle
        for (fade, is_declick) in [(StopFade::Declick, true), (StopFade::Cut, false)] {
            let (mut mixer, mut ctl) = Mixer::new(48_000);
            let id = ctl
                .load(
                    VoiceFeed::from_samples(&sine_samples(hz, amp, 40_000)),
                    LoadParams {
                        stop: Some((stop_at, fade)),
                        ..LoadParams::at(500)
                    },
                )
                .unwrap();
            let _ = id;
            let out = render(&mut mixer, 12_000, &[1024]);
            let l = left(&out);
            let max_step = l
                .windows(2)
                .map(|w| f64::from((w[1] - w[0]).abs()))
                .fold(0.0, f64::max);
            if is_declick {
                worst_declick = worst_declick.max(max_step);
                // Silent from the end of the fade on, and the voice ended at that frame.
                let end = stop_at as usize + 144;
                assert!(l[end..].iter().all(|&v| v == 0.0), "stop {stop_at}");
                let ev = drain_events(&mut ctl);
                assert!(ev.iter().any(|e| matches!(
                    e,
                    MixerEvent::Ended { frame, reason: EndReason::Stopped, .. } if *frame == end as u64
                )));
            } else {
                worst_cut = worst_cut.max(max_step);
            }
        }
    }
    println!(
        "declick: worst step {worst_declick:.5} (sine's own max {own_max_step:.5} + 0.01 = {limit:.5}); \
         without declick (Cut) worst step {worst_cut:.5}"
    );
    assert!(worst_declick <= limit);
    assert!(worst_cut > limit, "the test must be able to see a click");
}

// ---- acceptance 5 -------------------------------------------------------------------------

/// `corrupt.mp3` plays to its end with a decode_glitch count above zero and no panic.
#[test]
fn acceptance_5_corrupt_mp3_plays_to_the_end() {
    let Some(dir) = fixture_dir(&["corrupt.mp3"]) else {
        return;
    };
    let (out, rep) = render_files(&[dir.join("corrupt.mp3")]);
    let f = &rep.files[0];
    println!(
        "corrupt.mp3: glitches {} (limit > 0); ended {:?}; {} frames decoded and played; error {:?}",
        f.glitches,
        f.end_reason,
        out.len() / 2,
        f.error
    );
    assert!(f.glitches > 0);
    assert_eq!(f.end_reason, Some(EndReason::Eof));
    assert!(f.error.is_none());
    // The bad packet was replaced by silence of its duration, so the length is the full file's.
    assert!(out.len() / 2 > 1_000_000);
    assert_eq!(out.len() / 2, f.decoded_frames.unwrap() as usize);
}

// ---- acceptance 7 -------------------------------------------------------------------------

/// Two full-scale sines summed never exceed -1.0 dBFS in the output.
#[test]
fn acceptance_7_safety_limiter_holds_the_ceiling() {
    let ceiling = 10f64.powf(-1.0 / 20.0) as f32;
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    // Full scale: amplitude 1.0 (the file formats cannot hold exactly 1.0, the ring can).
    ctl.load(
        VoiceFeed::from_samples(&sine_samples(1000.0, 1.0, 96_000)),
        LoadParams::at(0),
    )
    .unwrap();
    ctl.load(
        VoiceFeed::from_samples(&sine_samples(1300.0, 1.0, 96_000)),
        LoadParams::at(0),
    )
    .unwrap();
    let out = render(&mut mixer, 100_000, &[1024]);
    let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let unlimited_peak = 2.0f32;
    let st = ctl.stats();
    println!(
        "two full-scale sines: output peak {:.5} = {:.4} dBFS (limit -1.0000 dBFS = {ceiling:.5}); \
         input sum peaks at {unlimited_peak} ({:.2} dBFS); limiter min gain {:.3}, \
         active {} frames, over 1 dB for {} frames",
        peak,
        20.0 * f64::from(peak).log10(),
        20.0 * f64::from(unlimited_peak).log10(),
        st.limiter_min_gain,
        st.limiter_active_frames,
        st.limiter_over_1db_frames
    );
    assert!(peak <= ceiling * 1.000_001);
    assert!(st.limiter_active_frames > 0);
    // And the level is not needlessly low: the limiter rides the peaks, it does not squash.
    assert!(peak > 0.85);
}

// ---- envelopes, stops, master --------------------------------------------------------------

fn dc_voice(
    mixer_ctl: &mut engine::mixer::MixerControl,
    frames: usize,
    start: u64,
) -> engine::mixer::VoiceId {
    dc_voice_amp(mixer_ctl, frames, start, 1.0)
}

fn dc_voice_amp(
    mixer_ctl: &mut engine::mixer::MixerControl,
    frames: usize,
    start: u64,
    amp: f32,
) -> engine::mixer::VoiceId {
    mixer_ctl
        .load(
            VoiceFeed::from_samples(&vec![amp; frames * 2]),
            LoadParams {
                declick_in: false,
                ..LoadParams::at(start)
            },
        )
        .unwrap()
}

#[test]
fn envelope_curves_match_their_formulas_to_the_frame() {
    for curve in [Curve::Linear, Curve::Cosine, Curve::EqualPower] {
        let (mut mixer, mut ctl) = Mixer::new(48_000);
        let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
        let seg = Segment {
            start: 3000,
            end: 9000,
            from: 1.0,
            to: 0.2,
            curve,
        };
        mixer.command(&id.envelope(seg).kind);
        let out = render(&mut mixer, 12_000, &[1000, 333, 1024]);
        let l = left(&out);
        let mut worst = 0.0f64;
        for (f, &v) in l.iter().enumerate() {
            let want = 0.5
                * if f < 3000 {
                    1.0
                } else if f < 9000 {
                    f64::from(seg.value(f as u64))
                } else {
                    0.2
                };
            worst = worst.max((f64::from(v) - want).abs());
        }
        println!(
            "envelope {curve:?}: worst error {worst:.2e} against the segment formula (limit 1e-6)"
        );
        assert!(worst < 1e-6, "{curve:?}");
        assert_eq!(l[2999], 0.5);
        assert!(
            (f64::from(l[3000]) - 0.5).abs() < 1e-6,
            "starts at `from` exactly on `start`"
        );
        assert!(
            (f64::from(l[9000]) - 0.1).abs() < 1e-6,
            "holds `to` from `end`"
        );
    }
}

#[test]
fn equal_power_crossfade_sums_to_constant_power() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let a = dc_voice_amp(&mut ctl, 20_000, 0, 0.4);
    let b = dc_voice_amp(&mut ctl, 20_000, 0, 0.4);
    let (s, e) = (4000u64, 12_000u64);
    for (id, from, to) in [(a, 1.0, 0.0), (b, 0.0, 1.0)] {
        mixer.command(
            &id.envelope(Segment {
                start: s,
                end: e,
                from,
                to,
                curve: Curve::EqualPower,
            })
            .kind,
        );
    }
    let out = render(&mut mixer, 14_000, &[1024]);
    let l = left(&out);
    let mut worst = 0.0f64;
    for f in (s as usize + 1500)..(e as usize - 1500) {
        let x = (f as f64 - s as f64) / (e - s) as f64;
        // gains: cos and sin of x pi/2; the summed DC is cos + sin; power sums to 1.
        let ga = f64::from(l[f]);
        let want = 0.4
            * ((x * std::f64::consts::FRAC_PI_2).cos() + (x * std::f64::consts::FRAC_PI_2).sin());
        worst = worst.max((ga - want).abs());
    }
    println!("equal-power crossfade: worst deviation {worst:.2e} from cos+sin (limit 1e-5)");
    assert!(worst < 1e-5);
}

#[test]
fn master_ramp_acts_after_the_limiter_and_stale_commands_are_ignored() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice(&mut ctl, 30_000, 0);
    let seg = Segment {
        start: 5000,
        end: 15_000,
        from: 1.0,
        to: 0.0,
        curve: Curve::Cosine,
    };
    mixer.command(&engine::mixer::master_ramp(seg).kind);
    // A command for a voice generation that does not exist is ignored and counted.
    let stale = engine::mixer::VoiceId {
        slot: id.slot,
        gen: id.gen + 7,
    };
    mixer.command(&stale.stop(100, StopFade::Declick).kind);
    let out = render(&mut mixer, 20_000, &[1024]);
    let l = left(&out);
    // DC 1.0 is above the ceiling: the limiter holds it to 0.891251 before the master gain.
    let ceil = 10f64.powf(-1.0 / 20.0);
    let mut worst = 0.0f64;
    for f in 2000..20_000usize {
        let g = if f < 5000 {
            1.0
        } else if f < 15_000 {
            f64::from(seg.value(f as u64))
        } else {
            0.0
        };
        // Skip the limiter's own settling at the very start.
        worst = worst.max((f64::from(l[f]) - ceil * g).abs());
    }
    println!(
        "master ramp: worst deviation {worst:.2e} from ceiling x ramp (limit 1e-5); stale commands {}",
        ctl.stats().stale_commands
    );
    assert!(worst < 1e-5);
    assert_eq!(ctl.stats().stale_commands, 1);
    assert!(l[4999] > 0.89);
}

#[test]
fn a_stop_before_the_start_cancels_the_voice() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice(&mut ctl, 5000, 10_000);
    mixer.command(&id.stop(9_000, StopFade::Declick).kind);
    let out = render(&mut mixer, 12_000, &[1024]);
    assert!(out.iter().all(|&v| v == 0.0));
    let ev = drain_events(&mut ctl);
    assert!(ev.iter().any(|e| matches!(
        e,
        MixerEvent::Ended {
            reason: EndReason::Cancelled,
            ..
        }
    )));
    assert_eq!(ctl.busy_voices(), 0);
}

#[test]
fn late_commands_start_at_the_earliest_frame_and_are_reported() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    render(&mut mixer, 3000, &[1024]);
    // Frame 100 has long passed (and the limiters' lookahead is 96 + 240 frames).
    dc_voice(&mut ctl, 500, 100);
    let out = render(&mut mixer, 2000, &[1024]);
    let ev = drain_events(&mut ctl);
    assert!(ev.iter().any(|e| matches!(e, MixerEvent::Late { .. })));
    assert_eq!(ctl.stats().late_starts, 1);
    // It sounded from the earliest frame the mixer could still fill: the bus runs the limiters'
    // lookahead (the safety limiter's 96 frames and the true-peak limiter's 240) ahead of the
    // output, so output frame 3000 + 336.
    assert_eq!(first_nonzero(&out), Some(mixer.limiter_latency_frames()));
    assert_eq!(mixer.limiter_latency_frames(), 96 + 240);
}

#[test]
fn starvation_is_silence_counted_and_the_audio_arrives_late_not_lost() {
    use engine::feed::{new_ring, FeedStatus};
    let (mut prod, ring) = new_ring(10_000);
    let status = Arc::new(FeedStatus::default());
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    ctl.load(
        VoiceFeed {
            ring,
            status: status.clone(),
        },
        LoadParams {
            declick_in: false,
            ..LoadParams::at(0)
        },
    )
    .unwrap();
    let push = |prod: &mut rtrb::Producer<f32>, n: usize, v: f32| {
        let c = prod.write_chunk_uninit(n * 2).unwrap();
        c.fill_from_iter(std::iter::repeat_n(v, n * 2));
    };
    push(&mut prod, 1000, 0.5);
    let a = render(&mut mixer, 2000, &[1000]); // 1000 frames of audio, then dry
    push(&mut prod, 500, 0.25);
    status.eof.store(true, std::sync::atomic::Ordering::Release);
    let b = render(&mut mixer, 2000, &[1000]);
    let st = ctl.stats();
    let ev = drain_events(&mut ctl);
    println!(
        "starvation: starved frames {}, episodes {}",
        st.starved_frames, st.starve_episodes
    );
    assert_eq!(left(&a)[999], 0.5);
    assert_eq!(left(&a)[1000], 0.0);
    // The late audio is played whole, from the next block on: nothing was dropped.
    let late: Vec<f32> = left(&b).into_iter().filter(|&v| v != 0.0).collect();
    assert_eq!(late.len(), 500);
    assert!(ev
        .iter()
        .any(|e| matches!(e, MixerEvent::Starved { frame: 1000, .. })));
    assert_eq!(st.starve_episodes, 1);
    assert!(ev.iter().any(|e| matches!(
        e,
        MixerEvent::Ended {
            reason: EndReason::Eof,
            played: 1500,
            ..
        }
    )));
}

#[test]
fn a_ninth_voice_is_refused_and_slots_are_reused_after_release() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    for _ in 0..8 {
        dc_voice(&mut ctl, 100, 0);
    }
    assert_eq!(
        ctl.load(VoiceFeed::from_samples(&[0.0; 8]), LoadParams::at(0))
            .unwrap_err(),
        engine::mixer::LoadError::NoFreeVoice
    );
    render(&mut mixer, 2048, &[1024]);
    assert_eq!(ctl.busy_voices(), 0, "all eight ended and were released");
    let again = dc_voice(&mut ctl, 100, 3000);
    assert_eq!(again.slot, 0);
    assert_eq!(again.gen, 2);
    assert_eq!(ctl.stats().garbage_leaks, 0);
}

// ---- gapless continuity ---------------------------------------------------------------------

/// Two consecutive files of one signal, played in continuity (no overlap, no gap, no ramp on
/// either side), reproduce the whole signal bit for bit; and at 44.1 kHz the joint is at
/// exactly `S_A + total_A`.
#[test]
fn continuity_joint_is_sample_exact_and_ramp_free() {
    let dir = out_dir().join("continuity");
    std::fs::create_dir_all(&dir).unwrap();
    // A 48 kHz signal split at an awkward frame.
    let n = 3 * 48_000usize;
    let split = 100_003usize;
    let full: Vec<f64> = (0..n)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / 48_000.0).sin())
        .collect();
    fixtures::files::write_wav24(&dir.join("whole.wav"), &full, 48_000, true).unwrap();
    fixtures::files::write_wav24(&dir.join("a.wav"), &full[..split], 48_000, true).unwrap();
    fixtures::files::write_wav24(&dir.join("b.wav"), &full[split..], 48_000, true).unwrap();
    let (whole, _) = render_files(&[dir.join("whole.wav")]);
    let (joined, rep) = render_files(&[dir.join("a.wav"), dir.join("b.wav")]);
    assert_eq!(rep.files[1].start_frame, split as u64);
    assert_eq!(joined.len(), whole.len());
    let differing = whole.iter().zip(&joined).filter(|(a, b)| a != b).count();
    println!(
        "continuity at 48 kHz: joint at frame {} (file A is {split} frames); {} of {} samples differ from the unsplit file (limit 0)",
        rep.files[1].start_frame,
        differing,
        whole.len()
    );
    assert_eq!(differing, 0);

    // 44.1 kHz: A's decoded length is round(n * 48000 / 44100); B starts on the next frame.
    let full44: Vec<f64> = (0..3 * 44_100)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / 44_100.0).sin())
        .collect();
    fixtures::files::write_wav24(&dir.join("a44.wav"), &full44[..100_003], 44_100, true).unwrap();
    fixtures::files::write_wav24(&dir.join("b44.wav"), &full44[100_003..], 44_100, true).unwrap();
    let (out, rep) = render_files(&[dir.join("a44.wav"), dir.join("b44.wav")]);
    let a_len = (100_003f64 * 48_000.0 / 44_100.0).round() as u64;
    let b_len = ((3 * 44_100 - 100_003) as f64 * 48_000.0 / 44_100.0).round() as u64;
    println!(
        "continuity at 44.1 kHz: A decoded {:?} frames (exact {a_len}); B starts at {} (exact {a_len}); total {} (exact {})",
        rep.files[0].decoded_frames,
        rep.files[1].start_frame,
        out.len() / 2,
        a_len + b_len
    );
    assert_eq!(rep.files[0].decoded_frames, Some(a_len));
    assert_eq!(rep.files[1].start_frame, a_len);
    assert_eq!(out.len() as u64 / 2, a_len + b_len);
    // No dead air at the joint: the signal is 997 Hz at 0.5, so around the joint every frame in
    // a short window is ordinary sine, never a run of zeros.
    let l = left(&out);
    let j = a_len as usize;
    assert!(l[j - 20..j + 20].iter().any(|&v| v.abs() > 0.1));
    let zero_run = l[j - 200..j + 200]
        .windows(3)
        .filter(|w| w.iter().all(|&v| v == 0.0))
        .count();
    assert_eq!(zero_run, 0);
}

// ---- head preload ---------------------------------------------------------------------------

/// A voice started from a 30 s head preload plays the same samples as one decoded directly.
#[test]
fn preload_then_continue_is_identical_to_direct_decode() {
    let Some(dir) = fixture_dir(&["rate_44k.flac", "flac16.flac"]) else {
        return;
    };
    for name in ["rate_44k.flac", "flac16.flac"] {
        let path = dir.join(name);
        let (direct, _) = render_files(std::slice::from_ref(&path));
        let pool = DecoderPool::new(3, 48_000);
        // A preload shorter than the file, so the continuation path is exercised.
        let h = pool.preload(path.clone(), 0.0, 1.5);
        let t0 = std::time::Instant::now();
        let pre = loop {
            if let Some(r) = h.take() {
                break r.unwrap();
            }
            assert!(t0.elapsed().as_secs() < 20);
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        assert!(pre.frames() >= (1.5 * 48_000.0) as u64);
        let feed = pool.open(VoiceSource::Preload(pre));
        let status = feed.status.clone();
        let (mut mixer, mut ctl) = Mixer::new(48_000);
        ctl.load(
            feed,
            LoadParams {
                declick_in: false,
                ..LoadParams::at(0)
            },
        )
        .unwrap();
        let mut out = Vec::new();
        while out.len() < direct.len() {
            let avail = status
                .pushed_frames
                .load(std::sync::atomic::Ordering::Acquire)
                - status
                    .consumed_frames
                    .load(std::sync::atomic::Ordering::Acquire);
            if avail < 4096 && !status.is_eof() {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            let n = 1024.min((direct.len() - out.len()) / 2);
            let s = out.len();
            out.resize(s + n * 2, 0.0);
            mixer.render(&mut out[s..]);
        }
        println!("{name}: preload (1.5 s of {PRELOAD_SECONDS} s max) + continuation vs direct decode: identical = {}", out == direct);
        assert_eq!(out, direct);
    }
}

// ---- start offset ---------------------------------------------------------------------------

#[test]
fn start_offset_decodes_from_the_cue_in() {
    use engine::decode::FileDecoder;
    let Some(dir) = fixture_dir(&["flac16.flac"]) else {
        return;
    };
    let path = dir.join("flac16.flac"); // 8 s at 48 kHz
    let mut whole = Vec::new();
    let mut d = FileDecoder::open(&path, 48_000, 0.0).unwrap();
    while d.decode_next(&mut whole).unwrap() {}
    let mut tail = Vec::new();
    let mut d = FileDecoder::open(&path, 48_000, 2.0).unwrap();
    assert_eq!(d.expected_frames(), Some(6 * 48_000));
    while d.decode_next(&mut tail).unwrap() {}
    println!(
        "start offset 2.0 s: {} frames (exact {})",
        tail.len() / 2,
        6 * 48_000
    );
    assert_eq!(tail.len() / 2, 6 * 48_000);
    assert_eq!(&whole[2 * 96_000..], &tail[..]);
}

#[test]
fn stall_command_kind_is_still_ignored_by_the_mixer() {
    let (mut mixer, _ctl) = Mixer::new(48_000);
    mixer.command(&CommandKind::Stall { ms: 1 });
    mixer.command(&CommandKind::SetGain(0.5));
    let out = render(&mut mixer, 2048, &[1024]);
    assert!(out.iter().all(|&v| v == 0.0));
}

/// How much of a 1,024-frame block (21.3 ms) the worst case takes: eight voices, each with an
/// envelope segment and a stop, the limiter working, on this machine.
#[test]
fn render_cost_of_eight_voices_is_a_small_fraction_of_the_block() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let mut ids = vec![];
    for k in 0..8 {
        ids.push(
            ctl.load(
                VoiceFeed::from_samples(&sine_samples(300.0 + 110.0 * k as f64, 0.6, 500_000)),
                LoadParams::at(0),
            )
            .unwrap(),
        );
    }
    for (k, id) in ids.iter().enumerate() {
        mixer.command(
            &id.envelope(Segment {
                start: 1000 * k as u64,
                end: 400_000,
                from: 1.0,
                to: 0.5,
                curve: Curve::Cosine,
            })
            .kind,
        );
        mixer.command(&id.stop(480_000, StopFade::Declick).kind);
    }
    let mut buf = vec![0.0f32; 2048];
    let mut worst = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    let blocks = 450;
    for _ in 0..blocks {
        let t = std::time::Instant::now();
        mixer.render(&mut buf);
        let d = t.elapsed();
        total += d;
        worst = worst.max(d);
    }
    let block_ms = 1024.0 / 48.0;
    println!(
        "render, 8 voices with envelopes + limiter: mean {:.1} us, worst {:.1} us per 1024-frame block \
         ({block_ms:.1} ms long): {:.2} % of the block on average",
        total.as_secs_f64() * 1e6 / f64::from(blocks),
        worst.as_secs_f64() * 1e6,
        total.as_secs_f64() * 1e3 / f64::from(blocks) / block_ms * 100.0
    );
    assert!(
        worst.as_secs_f64() * 1e3 < block_ms / 2.0,
        "well inside the period"
    );
}

// ---- review (Opus), 2026-09-29 --------------------------------------------------------------

/// Largest absolute difference between consecutive left-channel frames.
fn max_step(l: &[f32]) -> f32 {
    l.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

fn ended_frame(ev: &[MixerEvent]) -> Option<u64> {
    ev.iter().find_map(|e| match e {
        MixerEvent::Ended { frame, .. } => Some(*frame),
        _ => None,
    })
}

/// A stop whose frame has already passed is played as a whole fade from now, not entered
/// half-way or cut (which was a step of the full level).
#[test]
fn a_late_stop_fades_from_now_instead_of_stepping() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    let mut out = render(&mut mixer, 3000, &[1000]);
    mixer.command(&id.stop(100, StopFade::Declick).kind);
    out.extend(render(&mut mixer, 3000, &[1000]));
    let l = left(&out);
    let step = max_step(&l);
    let now = 3000 + mixer.limiter_latency_frames() as u64;
    let end = ended_frame(&drain_events(&mut ctl));
    println!("late stop: largest step {step:.5} (limit 0.02), ended at {end:?} (want {now} + 144)");
    assert!(step < 0.02);
    assert_eq!(l[now as usize - 1], 0.5);
    assert_eq!(end, Some(now + 144));
    assert!(l[now as usize + 144..].iter().all(|&v| v == 0.0));
}

/// A second stop during a stop fade shortens it from the level reached, never raises the gain;
/// a stop that would end later than the fade under way is ignored.
#[test]
fn a_stop_during_a_stop_fade_only_shortens_it() {
    // Shortened.
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(2000, StopFade::Frames(4800)).kind);
    let mut out = render(&mut mixer, 3000, &[1000]);
    mixer.command(&id.stop(3500, StopFade::Declick).kind);
    out.extend(render(&mut mixer, 5000, &[1000]));
    let l = left(&out);
    let rises = l.windows(2).filter(|w| w[1] > w[0]).count();
    let end = ended_frame(&drain_events(&mut ctl));
    println!(
        "stop during a fade: {rises} rising frames (limit 0), largest step {:.5}, ended at {end:?} \
         (want 3644)",
        max_step(&l)
    );
    assert_eq!(rises, 0);
    assert!(max_step(&l) < 0.01);
    assert_eq!(end, Some(3644));
    assert!(l[3644..].iter().all(|&v| v == 0.0));
    assert!(l[3643] > 0.0);

    // A later end is ignored: the fade under way ends where it was going to.
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(2000, StopFade::Frames(4800)).kind);
    render(&mut mixer, 3000, &[1000]);
    mixer.command(&id.stop(9000, StopFade::Declick).kind);
    render(&mut mixer, 6000, &[1000]);
    assert_eq!(ended_frame(&drain_events(&mut ctl)), Some(6800));
}

/// Non-finite gains are refused at the door: one NaN voice would otherwise silence the whole
/// bus (the limiter zeroes non-finite samples) or, on the master, reach the converter.
#[test]
fn non_finite_gains_are_refused() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let good = dc_voice_amp(&mut ctl, 5000, 0, 0.5);
    let bad = ctl
        .load(
            VoiceFeed::from_samples(&[0.25; 2000]),
            LoadParams {
                gain: f32::NAN,
                ..LoadParams::at(0)
            },
        )
        .unwrap();
    let nan_seg = Segment {
        start: 0,
        end: 100,
        from: f32::NAN,
        to: 1.0,
        curve: Curve::Linear,
    };
    mixer.command(&good.envelope(nan_seg).kind);
    mixer.command(
        &engine::mixer::master_ramp(Segment {
            to: f32::INFINITY,
            ..nan_seg
        })
        .kind,
    );
    let out = render(&mut mixer, 3000, &[1024]);
    let l = left(&out);
    let ev = drain_events(&mut ctl);
    assert!(ev.iter().any(|e| matches!(
        e,
        MixerEvent::LoadRejected { voice, gen } if *voice == bad.slot && *gen == bad.gen
    )));
    assert_eq!(ctl.stats().envelope_dropped, 2);
    assert!(
        l[96..3000].iter().all(|&v| v == 0.5),
        "the good voice is untouched"
    );
    render(&mut mixer, 5000, &[1024]);
    assert_eq!(ctl.busy_voices(), 0);
}

/// A stop fade of `u32::MAX` frames overflowed `len + 1` (a panic in debug, a NaN gain in
/// release).
#[test]
fn the_longest_stop_fade_is_finite() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 5000, 0, 0.5);
    mixer.command(&id.stop(1000, StopFade::Frames(u32::MAX)).kind);
    let out = render(&mut mixer, 3000, &[1024]);
    assert!(out.iter().all(|v| v.is_finite() && *v <= 0.5 && *v > 0.49));
}

/// Float sources: NaN and infinities become zero, and so do denormals and anything under
/// -600 dBFS, before the resampler and after it; ordinary samples pass bit for bit.
#[test]
fn decoder_output_has_no_non_finite_or_denormal_samples() {
    use engine::decode::{FileDecoder, SAMPLE_FLOOR};
    use engine::wav::{WavFormat, WavWriter};
    let dir = out_dir().join("sanitize");
    std::fs::create_dir_all(&dir).unwrap();
    let specials = [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        1e-40, // denormal
        -1e-39,
        1e-35,
        0.25,
        -0.125,
    ];
    let mut src = Vec::new();
    for i in 0..48_000usize {
        let s = if i % 100 < 8 {
            specials[i % 100]
        } else {
            ((i as f32) * 0.05).sin() * 0.3
        };
        src.extend([s, s]);
    }
    for rate in [48_000u32, 44_100] {
        let path = dir.join(format!("specials_{rate}.wav"));
        let mut w = WavWriter::create(&path, rate, 2, WavFormat::F32).unwrap();
        let bytes: Vec<u8> = src.iter().flat_map(|v| v.to_le_bytes()).collect();
        w.write_bytes(&bytes).unwrap();
        w.finish().unwrap();
        let mut d = FileDecoder::open(&path, 48_000, 0.0).unwrap();
        let mut out = Vec::new();
        while d.decode_next(&mut out).unwrap() {}
        let bad = out
            .iter()
            .filter(|v| !v.is_finite() || (**v != 0.0 && v.abs() < SAMPLE_FLOOR))
            .count();
        println!("{rate} Hz float source: {bad} non-finite or sub-floor samples (limit 0)");
        assert_eq!(bad, 0);
        if rate == 48_000 {
            for (i, (&o, &s)) in out.iter().zip(&src).enumerate() {
                if s.is_finite() && s.abs() >= SAMPLE_FLOOR {
                    assert_eq!(o.to_bits(), s.to_bits(), "sample {i}");
                } else {
                    assert_eq!(o, 0.0, "sample {i}");
                }
            }
        }
    }
}

// ---- M1.4 additions: curved stops, composing fades, the master ceiling ------------------------

/// A stop fade takes any curve. A linear fade is a straight line, a dB-linear fade a straight
/// line in dB down to -60 dB, and both end the voice at `stop + frames`.
#[test]
fn curved_stops_follow_their_curves() {
    for (curve, mid_want) in [
        (Curve::Linear, 0.25f64),
        (Curve::DbLinear, 0.5 * 10f64.powf(-1.5)),
        (Curve::Cosine, 0.25),
    ] {
        let (mut mixer, mut ctl) = Mixer::new(48_000);
        let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
        mixer.command(
            &id.stop(
                1000,
                StopFade::Curved {
                    frames: 4800,
                    curve,
                },
            )
            .kind,
        );
        let out = render(&mut mixer, 8000, &[1000]);
        let l = left(&out);
        let mid = f64::from(l[1000 + 2399]);
        let end = ended_frame(&drain_events(&mut ctl));
        println!("{curve:?} stop: level at the middle {mid:.5} (want {mid_want:.5}); ended at {end:?} (want 5800)");
        assert!((mid - mid_want).abs() < 2e-3, "{curve:?}: {mid}");
        assert_eq!(end, Some(5800));
        assert_eq!(l[999], 0.5);
        assert!(
            l.windows(2).all(|w| w[1] <= w[0] + 1e-7),
            "{curve:?} monotonic"
        );
    }
}

/// Two `fade_out`s on one voice compose: the second, shorter one takes over from the level the
/// first had reached, and the gain never rises or steps (the M1.2 review's overlapping-ramp case).
#[test]
fn two_fades_to_silence_on_one_voice_compose() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 30_000, 0, 0.5);
    mixer.command(&id.fade_out(2000, 9600).kind);
    let mut out = render(&mut mixer, 5000, &[1000]);
    mixer.command(&id.fade_out(5500, 1000).kind);
    out.extend(render(&mut mixer, 8000, &[1000]));
    let l = left(&out);
    let rises = l.windows(2).filter(|w| w[1] > w[0]).count();
    let end = ended_frame(&drain_events(&mut ctl));
    println!(
        "two fades: {rises} rising frames (limit 0), largest step {:.5} (limit 0.01), ended at {end:?} (want 6500)",
        max_step(&l)
    );
    assert_eq!(rises, 0);
    assert!(max_step(&l) < 0.01);
    assert_eq!(end, Some(6500));
}

/// The master gain sits after the safety limiter, so the mixer clamps its segments at 1.
#[test]
fn master_gain_above_one_is_clamped() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let _id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(
        &engine::mixer::master_ramp(Segment {
            start: 100,
            end: 1100,
            from: 1.0,
            to: 4.0,
            curve: Curve::Linear,
        })
        .kind,
    );
    let out = render(&mut mixer, 3000, &[1000]);
    let peak = left(&out).iter().fold(0.0f32, |m, v| m.max(*v));
    println!("master ramp 1 to 4: peak {peak} (limit 0.5)");
    assert!(peak <= 0.5 + 1e-6);
}

/// Review M1.4: a second stop that has not begun used to *replace* the first, so a later stop
/// (an operator's 2 s fade arriving while a 3 ms cue-out declick was pending) lengthened the
/// voice past the end it had been given. A stop now only brings the end earlier, armed or
/// playing; `clear_stop` is the explicit way to move a pending stop later.
#[test]
fn a_second_stop_never_lengthens_a_voice() {
    // Playing, the first stop pending: a later one is ignored.
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(5000, StopFade::Declick).kind);
    render(&mut mixer, 1000, &[1000]);
    mixer.command(&id.stop(3000, StopFade::Frames(4800)).kind);
    render(&mut mixer, 9000, &[1000]);
    let end = ended_frame(&drain_events(&mut ctl));
    println!("pending declick at 5000, then a fade 3000 to 7800: ended at {end:?} (want 5144)");
    assert_eq!(end, Some(5144));

    // An earlier-ending one replaces it.
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(9000, StopFade::Frames(4800)).kind);
    mixer.command(&id.stop(5000, StopFade::Declick).kind);
    render(&mut mixer, 15_000, &[1000]);
    assert_eq!(ended_frame(&drain_events(&mut ctl)), Some(5144));

    // Armed: the same rule.
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 2000, 0.5);
    mixer.command(&id.stop(5000, StopFade::Declick).kind);
    mixer.command(&id.stop(8000, StopFade::Declick).kind);
    render(&mut mixer, 12_000, &[1000]);
    assert_eq!(ended_frame(&drain_events(&mut ctl)), Some(5144));
}

/// `clear_stop` removes a stop that has not begun (then a later one can be set), and leaves a
/// fade under way alone (removing it would raise the gain).
#[test]
fn clear_stop_moves_a_pending_stop_and_never_undoes_a_fade() {
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(5000, StopFade::Declick).kind);
    render(&mut mixer, 1000, &[1000]);
    mixer.command(&id.clear_stop().kind);
    mixer.command(&id.stop(9000, StopFade::Declick).kind);
    render(&mut mixer, 11_000, &[1000]);
    let end = ended_frame(&drain_events(&mut ctl));
    println!("cleared, then a stop at 9000: ended at {end:?} (want 9144)");
    assert_eq!(end, Some(9144));

    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let id = dc_voice_amp(&mut ctl, 20_000, 0, 0.5);
    mixer.command(&id.stop(2000, StopFade::Frames(4800)).kind);
    let mut out = render(&mut mixer, 3000, &[1000]);
    mixer.command(&id.clear_stop().kind);
    out.extend(render(&mut mixer, 6000, &[1000]));
    let l = left(&out);
    let rises = l.windows(2).filter(|w| w[1] > w[0]).count();
    let end = ended_frame(&drain_events(&mut ctl));
    println!("clear during a fade: {rises} rising frames (limit 0), ended at {end:?} (want 6800)");
    assert_eq!(rises, 0);
    assert_eq!(end, Some(6800));
}

// ---- the pre meter tap (M1.8, the M1.7 review's third open item) --------------------------------

/// With nobody wanting the pre-chain spectrum the mixer does not copy the pre tap at all (its ring stays
/// empty while the post tap fills); when it is wanted a run opens with its marker, naming the post frame the
/// run's audio belongs to, and the audio that follows is the bus block for block; withdrawn, it stops.
#[test]
fn the_pre_tap_is_copied_only_while_it_is_wanted() {
    use engine::mixer::{next_pre_request, pre_run_marker, PRE_SENTINEL};
    use std::sync::atomic::Ordering;
    let (mut mixer, mut ctl) = Mixer::new(48_000);
    let mut tap = ctl.take_meter_tap().unwrap();
    let mut out = vec![0.0f32; 1024 * 2];
    for _ in 0..4 {
        mixer.render(&mut out);
    }
    assert_eq!(tap.pre.slots(), 0, "the pre ring is untouched");
    assert_eq!(tap.post.slots(), 4 * 2048, "the post tap took 4 blocks");
    // Wanted: the next block opens a run at post frame 4096.
    tap.pre_wanted.store(next_pre_request(0), Ordering::Relaxed);
    for _ in 0..3 {
        mixer.render(&mut out);
    }
    let got = tap.pre.slots();
    assert_eq!(got, 3 + 3 * 2048, "one marker and three blocks");
    let c = tap.pre.read_chunk(got).unwrap();
    let (a, b) = c.as_slices();
    let all: Vec<f32> = a.iter().chain(b.iter()).copied().collect();
    c.commit_all();
    assert_eq!(all[0].to_bits(), PRE_SENTINEL);
    let m = pre_run_marker(4 * 1024);
    assert_eq!(
        &all[..3].iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
        &m.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
    );
    // Withdrawn: nothing more arrives.
    tap.pre_wanted.store(0, Ordering::Relaxed);
    for _ in 0..3 {
        mixer.render(&mut out);
    }
    assert_eq!(tap.pre.slots(), 0);
    // Asked again: a new run, with a new marker at post frame 10 * 1024.
    tap.pre_wanted.store(next_pre_request(1), Ordering::Relaxed);
    mixer.render(&mut out);
    let n = tap.pre.slots();
    assert_eq!(n, 3 + 2048);
    let c = tap.pre.read_chunk(n).unwrap();
    let (a, b) = c.as_slices();
    let all: Vec<f32> = a.iter().chain(b.iter()).copied().collect();
    c.commit_all();
    assert_eq!(all[1].to_bits(), pre_run_marker(10 * 1024)[1].to_bits());
}
