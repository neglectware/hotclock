//! M1.4 acceptance tests (milestones.md, "M1.4 Segue executor and normalization"): offline
//! renders through the real conductor and mixer, asserted numerically. Every assertion in this
//! file runs under `preset = sam`, whose numbers it tests (the other presets are `transitions_m14a`).
//! Each test prints its measured numbers next to the limit; BUILDLOG copies them.
//!
//! Two techniques keep the measurements independent of the code under test: overlapping voices are
//! separated by frequency (whole-cycle correlation, `tone_amp`) or by subtracting a solo render of
//! the other voice at the same start frame, and every frame position is checked both in the
//! sidecar and in the audio.

#![allow(clippy::needless_range_loop)]

mod support;

use station::item::{gain_db, run_gain, Item, ItemBuild};
use support::*;

const SAM: &str = "preset: sam";

fn fade_out_gain(k: u64, len: u64) -> f64 {
    0.5 + 0.5 * (std::f64::consts::PI * (k + 1) as f64 / (len as f64 + 1.0)).cos()
}

/// `sum(a*b) / sum(b*b)`: the gain that turns `b` into `a` over a window.
fn gain_of(a: &[f32], b: &[f32]) -> f64 {
    let num: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let den: f64 = b.iter().map(|y| f64::from(*y).powi(2)).sum();
    num / den
}

// ---- 1: attack alignment ------------------------------------------------------------------

#[test]
fn m14_1_attack_alignment() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: pickup.flac }";
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let r = run(&yaml, &dir);
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    let d = b.attack_frame as i64 - a.segue_frame as i64;

    // Independent: locate the -32 to -14 dBFS step of pickup (500 Hz) in the render. A 96-sample
    // window is one cycle of 500 Hz and two of tone_fade's 1 kHz, so tone_fade cancels; the
    // amplitude is linear in the part of the window after the step, so the step is 48 samples
    // after the first window that reaches the midpoint.
    let l = left(&r.samples);
    let seg = a.segue_frame as usize;
    let amp = |s: usize| tone_amp(&l, 500.0, s, 96);
    let low = (0..20).map(|k| amp(seg - 30_000 + k * 96)).sum::<f64>() / 20.0;
    let high = (0..20).map(|k| amp(seg + 4_800 + k * 96)).sum::<f64>() / 20.0;
    let mid = (low + high) / 2.0;
    let s0 = (seg - 6000..seg + 6000).find(|&s| amp(s) >= mid).unwrap();
    let step = s0 + 48;
    println!(
        "tone_fade -> pickup (sam): pickup starts at frame {} ({:.1} ms before the segue); \
         sidecar: attack {} vs segue {} (difference {d} frames, limit 1); \
         measured step at frame {step} ({:.3} ms from the segue frame, limit 1 ms); \
         levels low {:.1} dB high {:.1} dB (step {:.1} dB, expected 18)",
        b.start_frame,
        ms(seg as u64 - b.start_frame),
        b.attack_frame,
        a.segue_frame,
        (step as f64 - seg as f64) * 1000.0 / 48_000.0,
        db(low),
        db(high),
        db(high) - db(low)
    );
    assert!(d.abs() <= 1);
    assert!((step as i64 - seg as i64).abs() <= 48);
    assert!(
        (db(high) - db(low) - 18.0).abs() < 0.5,
        "the step is the -32 to -14 dB step"
    );
}

// ---- 2 and 3: cold start never faded in; outgoing not faded above the threshold ------------

/// `album_t2` is the 800 Hz `cold_start`; a different tone from tone_fade's 1 kHz so the two
/// can be told apart while they overlap.
#[test]
fn m14_2_cold_start_is_never_faded_in() {
    let Some(dir) = fx(&["tone_fade.flac", "album_t2.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: album_t2.flac }";
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let r = run(&yaml, &dir);
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    let l = left(&r.samples);
    let sb = b.start_frame as usize;
    let steady: f64 = (0..10)
        .map(|k| tone_amp(&l, 800.0, sb + 96_000 + k * 480, 480))
        .sum::<f64>()
        / 10.0;
    let mut worst = 0.0f64;
    for k in 0..10 {
        // The first 100 ms after the 3 ms (144-frame) declick, in 10 ms windows.
        let dev = (db(tone_amp(&l, 800.0, sb + 144 + k * 480, 480)) - db(steady)).abs();
        worst = worst.max(dev);
    }
    // Exactly: B is the file's own audio times its gain from its first frame plus the declick.
    let src = decode_left(&dir.join("album_t2.flac"));
    let g = 10f32.powf(b.gain_db as f32 / 20.0);
    let from = (b.file_start_ms * 48) as usize;
    let mut exact = 0.0f32;
    let solo = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
    let ls = left(&solo.samples);
    for i in 144..48_000 {
        exact = exact.max((ls[b.start_frame as usize + i] - src[from + i] * g).abs());
    }
    println!(
        "tone_fade -> album_t2 (cold start): B starts at {} (segue {}, difference {} frames, limit 1); \
         level over the first 100 ms after the declick within {worst:.4} dB of steady state (limit 0.1); \
         solo render equals source x gain exactly from frame 144: largest difference {exact} (limit 0)",
        b.start_frame,
        a.segue_frame,
        b.start_frame as i64 - a.segue_frame as i64
    );
    assert!((b.start_frame as i64 - a.segue_frame as i64).abs() <= 1);
    assert!(worst < 0.1);
    assert_eq!(exact, 0.0);
}

#[test]
fn m14_3_outgoing_is_not_faded_above_its_segue() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: pickup.flac }";
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let r = run(&yaml, &dir);
    let a = voice(&r, "a");
    let l = left(&r.samples);
    // tone_fade's own curve: its file scaled by its gain, placed where its voice plays.
    let src = decode_left(&dir.join("tone_fade.flac"));
    let g = 10f32.powf(a.gain_db as f32 / 20.0);
    let from = (a.file_start_ms * 48) as usize;
    let sa = a.start_frame as usize;
    let mut worst = 0.0f64;
    let mut n = 0;
    let mut s = sa + 4_800;
    while s + 480 <= a.segue_frame as usize {
        let mine = tone_amp(&l, 1000.0, s, 480);
        let want: Vec<f32> = src[from + (s - sa)..from + (s - sa) + 480]
            .iter()
            .map(|v| v * g)
            .collect();
        // The reference window is cut out, so its phase reference differs: compare amplitudes.
        let want = tone_amp(&want, 1000.0, 0, 480);
        worst = worst.max((db(mine) - db(want)).abs());
        n += 1;
        s += 480;
    }
    println!(
        "tone_fade up to its segue frame {}: {n} windows of 10 ms compared with its own curve, \
         largest difference {worst:.5} dB (limit 0.1)",
        a.segue_frame
    );
    assert!(n > 1000);
    assert!(worst < 0.1);
}

// ---- 4: tail cap ------------------------------------------------------------------------------

#[test]
fn m14_4_tail_cap_is_a_two_second_cosine_ending_twelve_seconds_after_the_segue() {
    let Some(dir) = fx(&["long_tail.flac", "album_t2.flac"]) else {
        return;
    };
    // The cap only cuts a tail longer than 12 s. With the shipped markers long_tail's segue is
    // near 35 s and its cue-out near 46.75 s, which is right at the limit; when the tail is not
    // longer than 12 s the cue-out is moved to 49.5 s (still inside the 50 s file) so the cap has a
    // tail to cut.
    let fa = analysis::analyze::analyze_file(
        &dir.join("long_tail.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let ovr = if fa.auto.cue_out - fa.auto.segue > 12_000 {
        ""
    } else {
        ", overrides: { cue_out: 49500 }"
    };
    println!(
        "long_tail markers: segue {} ms, cue-out {} ms (tail {} ms){}",
        fa.auto.segue,
        fa.auto.cue_out,
        fa.auto.cue_out - fa.auto.segue,
        if ovr.is_empty() {
            ""
        } else {
            "; cue-out override 49500 ms so that the 12 s cap has a tail to cut"
        }
    );
    let items = format!("  a: {{ file: long_tail.flac{ovr} }}\n  b: {{ file: album_t2.flac }}");
    let items = items.as_str();
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let x = run(&yaml, &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    // B alone at the same frame, and A alone at its own.
    let y = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
    let z = run(&solo_yaml(items, "a", a.start_frame, SAM), &dir);
    let (lx, ly, lz) = (left(&x.samples), left(&y.samples), left(&z.samples));
    let n = lx.len();
    let contrib: Vec<f32> = (0..n)
        .map(|i| lx[i] - ly.get(i).copied().unwrap_or(0.0))
        .collect();
    let stop = a.stop.clone().expect("a stop");
    let cap_end = a.segue_frame + 12 * 48_000;
    let after = &contrib[a.ended_frame.unwrap() as usize..];
    let after_peak = after.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    // Unchanged before the cap starts.
    let pre = &contrib[a.start_frame as usize..stop.at as usize];
    let pre_diff = max_abs_diff(pre, &lz[a.start_frame as usize..stop.at as usize]);
    // The fade: five checkpoints against the cosine, 96-sample windows.
    let mut worst = 0.0f64;
    for x_ in [0.1, 0.3, 0.5, 0.7, 0.85] {
        let k = (x_ * f64::from(stop.frames)) as u64;
        let s = (stop.at + k) as usize;
        let got = gain_of(&contrib[s..s + 96], &lz[s..s + 96]);
        let want = fade_out_gain(k + 48, u64::from(stop.frames));
        worst = worst.max((db(got) - db(want)).abs());
    }
    println!(
        "long_tail -> album_t2: A's stop is a {} of {} frames from {} to {} (segue {} + 12 s = {cap_end}); \
         A ended at {:?}; A's audio after its end peaks at {:.1} dBFS (limit -90); \
         before the fade A equals its solo render within {pre_diff} (limit 1e-6); \
         the fade follows the cosine within {worst:.4} dB at 5 checkpoints (limit 0.1)",
        stop.curve,
        stop.frames,
        stop.at,
        stop.at + u64::from(stop.frames),
        a.segue_frame,
        a.ended_frame,
        db(f64::from(after_peak))
    );
    assert_eq!(stop.curve, "cosine");
    assert_eq!(stop.frames, 96_000);
    assert!((stop.at + u64::from(stop.frames)).abs_diff(cap_end) <= 1);
    assert!(a.ended_frame.unwrap().abs_diff(cap_end) <= 1);
    assert!(db(f64::from(after_peak)) < -90.0);
    assert!(pre_diff < 1e-6);
    assert!(worst < 0.1);
}

// ---- 5: liner accepts no lead ------------------------------------------------------------------

#[test]
fn m14_5_a_liner_accepts_no_lead() {
    let Some(dir) = fx(&["liner_8s.flac", "pickup.flac"]) else {
        return;
    };
    let items = "  l: { file: liner_8s.flac, category: liner }\n  b: { file: pickup.flac }";
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: l }}\n  - {{ item: b }}\n"
    );
    let x = run(&yaml, &dir);
    let (l, b) = (voice(&x, "l"), voice(&x, "b"));
    // Pickup alone at the same frame: its first non-silence.
    let y = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
    let ly = left(&y.samples);
    let first = (b.start_frame as usize..ly.len())
        .find(|&i| ly[i].abs() > 1e-3)
        .unwrap();
    println!(
        "liner_8s -> pickup: liner segue at frame {}, pickup starts at {} and first sounds at {first} \
         (limit: at or after {}); the pickup's own lead would have been {:.0} ms",
        l.segue_frame,
        b.start_frame,
        l.segue_frame,
        ms(b.attack_frame - b.start_frame)
    );
    assert!(b.start_frame >= l.segue_frame);
    assert!(first as u64 >= l.segue_frame);
    assert_eq!(x.sidecar.boundaries[0].lead_frames, 0);
}

// ---- 6: normalization --------------------------------------------------------------------------

#[test]
fn m14_6_normalization_levels_and_the_peak_rule() {
    let Some(dir) = fx(&["quiet_track.flac", "tone_fade.flac"]) else {
        return;
    };
    let items = "  q: { file: quiet_track.flac }\n  t: { file: tone_fade.flac }";
    let mut body = vec![];
    let mut gains = vec![];
    for it in ["q", "t"] {
        let r = run(&solo_yaml(items, it, 0, SAM), &dir);
        let l = left(&r.samples);
        // The 20 s body runs from 0.5 s: measure 3 s to 13 s of the file (the 0.48 s cue-in
        // moves the voice, so index from the voice's own start).
        let v = voice(&r, it);
        let s = v.start_frame as usize + 3 * 48_000;
        body.push(rms_db(&l[s..s + 10 * 48_000]));
        gains.push(v.gain_db);
    }
    println!(
        "normalization to -18 LUFS: quiet_track gain {:+.3} dB, tone_fade gain {:+.3} dB; body levels \
         {:.3} and {:.3} dBFS RMS, difference {:.4} dB (limit 0.2)",
        gains[0],
        gains[1],
        body[0],
        body[1],
        (body[0] - body[1]).abs()
    );
    assert!((body[0] - body[1]).abs() < 0.2);
    assert!(gains[0] > gains[1] + 10.0, "the quiet track gets the boost");

    // The peak rule (3.6): a target so hot that the gain would put the peak past the allowance.
    // Target 0 LUFS, allowance 0 dB: the gain is limited to -1 - TP exactly.
    let fa = analysis::analyze::analyze_file(
        &dir.join("tone_fade.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let want = (-1.0 - fa.true_peak).min(0.0 - fa.l_int).min(40.0);
    let r = run(
        &solo_yaml(
            items,
            "t",
            0,
            "preset: sam, target_lufs: 0, max_boost_db: 40, peak_allowance_db: 0",
        ),
        &dir,
    );
    let v = voice(&r, "t");
    let peak = left(&r.samples).iter().fold(0.0f32, |m, x| m.max(x.abs()));
    println!(
        "peak rule: tone_fade L_int {:.3} LUFS, true peak {:.3} dBTP; wanted gain {:+.3} dB, limited to \
         {want:+.9} dB by the rule (unlimited {:+.3}); applied {:+.9} dB (limit: equal); rendered sample \
         peak {:.3} dBFS (about -1.0)",
        fa.l_int,
        fa.true_peak,
        0.0 - fa.l_int,
        0.0 - fa.l_int,
        v.gain_db,
        db(f64::from(peak))
    );
    assert!((0.0 - fa.l_int) > want + 0.5, "the rule is what limits it");
    assert!((v.gain_db - want).abs() < 1e-9);
    assert!(db(f64::from(peak)) < -0.9 && db(f64::from(peak)) > -1.3);
}

// ---- 7: no dead air ----------------------------------------------------------------------------

/// A 20-item scenario alternating music fixtures and the two liners (`support::office_yaml`). A
/// silent run (below -60 dBFS, longer than 50 ms) is dead air unless it lies inside a voice's own
/// silence: some voice is active for every frame of it, and every active voice's own signal there
/// is below -60 dBFS too (`support::dead_air`).
#[test]
fn m14_7_no_dead_air_in_twenty_mixed_items() {
    let Some(dir) = fx(&OFFICE_FILES) else {
        return;
    };
    let (yaml, order) = office_yaml("sam");
    let r = run(&yaml, &dir);
    let (bad, checked) = dead_air(&r, &dir);
    println!(
        "{} items, {} frames rendered ({:.1} s): {checked} silent runs over 50 ms, all inside a voice's own \
         silence except {bad} (limit 0); starved frames {}, late starts {}",
        order.len(),
        r.sidecar.render_frames,
        ms(r.sidecar.render_frames) / 1000.0,
        r.sidecar.starved_frames,
        r.sidecar.late_starts
    );
    assert_eq!(bad, 0);
    assert_eq!(r.sidecar.voices.len(), 20);
    assert_eq!(r.sidecar.starved_frames, 0);
}

// ---- 8: manual transitions ---------------------------------------------------------------------

#[test]
fn m14_8_next_fades_the_outgoing_over_one_second_and_lands_the_attack_on_the_fade_end() {
    let Some(dir) = fx(&["tone_fade.flac", "album_t2.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: album_t2.flac }";
    for at in [5.13f64, 9.77, 13.41] {
        let yaml = format!(
            "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\nactions:\n  - {{ at_s: {at}, do: next }}\n"
        );
        let x = run(&yaml, &dir);
        let (a, b) = (voice(&x, "a"), voice(&x, "b"));
        let stop = a.stop.clone().unwrap();
        let y = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
        let z = run(&solo_yaml(items, "a", a.start_frame, SAM), &dir);
        let (lx, ly, lz) = (left(&x.samples), left(&y.samples), left(&z.samples));
        let contrib: Vec<f32> = (0..lx.len())
            .map(|i| lx[i] - ly.get(i).copied().unwrap_or(0.0))
            .collect();
        let mut worst = 0.0f64;
        for x_ in [0.1, 0.3, 0.5, 0.7, 0.9] {
            let k = (x_ * 48_000.0) as u64;
            let s = (stop.at + k) as usize;
            let got = gain_of(&contrib[s..s + 96], &lz[s..s + 96]);
            let want = fade_out_gain(k + 48, 48_000);
            worst = worst.max((db(got) - db(want)).abs());
        }
        let fade_end = stop.at + u64::from(stop.frames);
        println!(
            "next at {at} s: A fades {} of {} frames from {} (limit: cosine, 48000); within {worst:.4} dB of the \
             cosine at 5 checkpoints (limit 0.1); A ended at {:?} (fade end {fade_end}); B's attack at {} \
             (difference {} frames, limit 1)",
            stop.curve,
            stop.frames,
            stop.at,
            a.ended_frame,
            b.attack_frame,
            b.attack_frame as i64 - fade_end as i64
        );
        assert_eq!(stop.curve, "cosine");
        assert_eq!(stop.frames, 48_000);
        assert!(worst < 0.1);
        assert_eq!(a.ended_frame, Some(fade_end));
        assert!((b.attack_frame as i64 - fade_end as i64).abs() <= 1);
        assert!(
            stop.at as f64 / 48_000.0 >= at,
            "the fade starts after the button"
        );
    }
}

#[test]
fn m14_8b_the_other_manual_transitions() {
    let Some(dir) = fx(&["tone_fade.flac", "album_t2.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: album_t2.flac }";
    let q = "queue:\n  - { item: a }\n  - { item: b }\n";
    let mk = |act: &str| format!("station: {{ {SAM} }}\nitems:\n{items}\n{q}actions:\n  - {act}\n");

    // cut: A's 30 ms fade and B's start on the same frame.
    let x = run(&mk("{ at_s: 6.0, do: cut }"), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let stop = a.stop.clone().unwrap();
    println!(
        "cut: A fades {} frames ({:.1} ms) from {}, B starts at {} (difference {}, limit 0)",
        stop.frames,
        ms(u64::from(stop.frames)),
        stop.at,
        b.start_frame,
        b.start_frame as i64 - stop.at as i64
    );
    assert_eq!(stop.frames, 1440);
    assert_eq!(b.start_frame, stop.at);
    assert_eq!(a.ended_frame, Some(stop.at + 1440));

    // fade to next: 2.0 s, B's attack where the fade ends.
    let x = run(&mk("{ at_s: 6.0, do: fade_next }"), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let stop = a.stop.clone().unwrap();
    println!(
        "fade_next: A fades {} frames from {}, B's attack at {} (fade end {}, limit 1 frame)",
        stop.frames,
        stop.at,
        b.attack_frame,
        stop.at + u64::from(stop.frames)
    );
    assert_eq!(stop.frames, 96_000);
    assert!((b.attack_frame as i64 - (stop.at + 96_000) as i64).abs() <= 1);

    // hard time: B's attack lands on the second.
    let x = run(&mk("{ at_s: 12.0, do: hard_time }"), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let stop = a.stop.clone().unwrap();
    println!(
        "hard_time at 12.0 s: A fades {} frames from {} (limit: 72000 frames ending at 576000), B's attack at {} \
         (limit 576000 within 1 frame)",
        stop.frames, stop.at, b.attack_frame
    );
    assert_eq!(stop.frames, 72_000);
    assert_eq!(stop.at, 576_000 - 72_000);
    assert!((b.attack_frame as i64 - 576_000).abs() <= 1);

    // stop: a 2.0 s cosine and silence after it; nothing else plays.
    let x = run(&mk("{ at_s: 6.0, do: stop }"), &dir);
    let a = voice(&x, "a");
    let stop = a.stop.clone().unwrap();
    println!(
        "stop: A fades {} frames from {}, ended at {:?}; voices in the render: {}",
        stop.frames,
        stop.at,
        a.ended_frame,
        x.sidecar.voices.len()
    );
    assert_eq!(stop.frames, 96_000);
    assert_eq!(x.sidecar.voices.len(), 1);
    assert_eq!(x.sidecar.render_frames, stop.at + 96_000);

    // hold (stop after current): A plays to its cue-out, then silence.
    let x = run(&mk("{ at_s: 6.0, do: hold }"), &dir);
    let a = voice(&x, "a");
    let fa = analysis::analyze::analyze_file(
        &dir.join("tone_fade.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let want = a.start_frame + (fa.auto.cue_out - fa.auto.cue_in) as u64 * 48;
    println!(
        "hold: A played to frame {:?} (its cue-out {want}), voices: {}",
        a.ended_frame,
        x.sidecar.voices.len()
    );
    assert_eq!(x.sidecar.voices.len(), 1);
    assert!(a.ended_frame.unwrap().abs_diff(want) <= 1);
}

/// Review M1.4: a manual action pressed after the boundary into B was committed (the renderer
/// commits 10 s ahead, M1.5 about 2 s) but before B starts. Before the fix, `next` planned from B
/// (not started: it was cancelled, the item on air kept playing) and failed with "nothing is
/// queued" on a two-item queue; `stop` stopped only B, so A played on; `hold` let B play. Now the
/// committed boundary is undone and the action acts on what is sounding.
#[test]
fn m14_8c_manual_actions_after_a_committed_boundary_act_on_what_is_sounding() {
    let Some(dir) = fx(&["tone_fade.flac", "album_t2.flac", "long_tail.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: album_t2.flac }";
    let q = "queue:\n  - { item: a }\n  - { item: b }\n";
    let mk = |act: &str| format!("station: {{ {SAM} }}\nitems:\n{items}\n{q}actions:\n  - {act}\n");
    let press = 18.0f64;
    let pf = (press * 48_000.0) as u64;
    let played = |x: &station::scenario::Rendered, name: &str| -> Vec<(u64, Option<u64>)> {
        x.sidecar
            .voices
            .iter()
            .filter(|v| v.item == name && v.played.unwrap_or(0) > 0)
            .map(|v| (v.start_frame, v.ended_frame))
            .collect()
    };
    // Without an action, B starts after the press and is committed before it (10 s ahead).
    let plain = run(&format!("station: {{ {SAM} }}\nitems:\n{items}\n{q}"), &dir);
    let b0 = voice(&plain, "b").start_frame;
    println!(
        "the boundary: B starts at {b0} ({:.2} s), committed 10 s earlier; press at {press} s",
        ms(b0) / 1000.0
    );
    assert!(b0 > pf + 24_000 && b0 < pf + 480_000);

    for act in ["next", "fade_next", "cut"] {
        let x = run(&mk(&format!("{{ at_s: {press}, do: {act} }}")), &dir);
        let a = voice(&x, "a");
        let stop = a.stop.clone().unwrap();
        let end = stop.at + u64::from(stop.frames);
        let bs = played(&x, "b");
        let cancelled = x
            .sidecar
            .voices
            .iter()
            .filter(|v| v.item == "b" && v.end_reason.as_deref() == Some("cancelled"))
            .count();
        println!(
            "{act} at {press} s: A's fade {} frames from {} (ends {end}), A ended at {:?}; B played as {bs:?}, \
             {cancelled} cancelled B voice",
            stop.frames, stop.at, a.ended_frame
        );
        assert!(
            stop.at >= pf && stop.at < b0,
            "{act}: the fade starts at the press"
        );
        assert_eq!(a.ended_frame, Some(end), "{act}: A ends with its fade");
        assert_eq!(bs.len(), 1, "{act}: B plays once");
        assert!(
            bs[0].0 >= stop.at && bs[0].0 <= end,
            "{act}: B starts in A's fade"
        );
        assert_eq!(cancelled, 1, "{act}: the committed B was cancelled");
    }

    // stop: everything is silent after the 2 s fade.
    let x = run(&mk(&format!("{{ at_s: {press}, do: stop }}")), &dir);
    let a = voice(&x, "a");
    let end = a.stop.clone().map(|s| s.at + u64::from(s.frames)).unwrap();
    println!(
        "stop at {press} s: A ended at {:?} (fade end {end}); B played as {:?}; render {} frames",
        a.ended_frame,
        played(&x, "b"),
        x.sidecar.render_frames
    );
    assert_eq!(a.ended_frame, Some(end));
    assert!(played(&x, "b").is_empty());
    assert_eq!(x.sidecar.render_frames, end);

    // hold: A plays to its cue-out, B never plays.
    let x = run(&mk(&format!("{{ at_s: {press}, do: hold }}")), &dir);
    let a = voice(&x, "a");
    let fa = analysis::analyze::analyze_file(
        &dir.join("tone_fade.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let want = a.start_frame + (fa.auto.cue_out - fa.auto.cue_in) as u64 * 48;
    println!(
        "hold at {press} s: A ended at {:?} (its cue-out {want}); B played as {:?}",
        a.ended_frame,
        played(&x, "b")
    );
    assert!(a.ended_frame.unwrap().abs_diff(want) <= 1);
    assert!(played(&x, "b").is_empty());

    // stop during an overlap: A's tail under B stops with B (long_tail's 11 s tail).
    let items2 = "  a: { file: long_tail.flac }\n  b: { file: album_t2.flac }";
    let y0 = run(
        &format!("station: {{ {SAM} }}\nitems:\n{items2}\n{q}"),
        &dir,
    );
    let press2 = ms(voice(&y0, "b").start_frame) / 1000.0 + 2.0;
    let y = run(
        &format!("station: {{ {SAM} }}\nitems:\n{items2}\n{q}actions:\n  - {{ at_s: {press2:.3}, do: stop }}\n"),
        &dir,
    );
    let last_end = y
        .sidecar
        .voices
        .iter()
        .filter_map(|v| v.ended_frame)
        .max()
        .unwrap();
    let a = voice(&y, "a");
    let b = voice(&y, "b");
    println!(
        "stop at {press2:.3} s, 2 s into the overlap: A ended at {:?}, B at {:?} (A's own tail ran to {:?})",
        a.ended_frame,
        b.ended_frame,
        voice(&y0, "a").ended_frame
    );
    assert_eq!(a.ended_frame, b.ended_frame, "the tail stops with B");
    assert_eq!(y.sidecar.render_frames, last_end);
    assert!(a.ended_frame.unwrap() < voice(&y0, "a").ended_frame.unwrap());
}

// ---- 9: override precedence --------------------------------------------------------------------

#[test]
fn m14_9_a_marker_override_moves_the_segue_exactly_and_deleting_it_restores_the_auto_value() {
    let Some(dir) = fx(&["tone_fade.flac", "album_t2.flac"]) else {
        return;
    };
    let fa = analysis::analyze::analyze_file(
        &dir.join("tone_fade.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let mk = |ovr: &str| {
        format!(
            "station: {{ {SAM} }}\nitems:\n  a: {{ file: tone_fade.flac{ovr} }}\n  b: {{ file: album_t2.flac }}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
        )
    };
    let with = run(&mk(", overrides: { segue: 20000 }"), &dir);
    let without = run(&mk(""), &dir);
    let (a1, a0) = (voice(&with, "a"), voice(&without, "a"));
    let want1 = a1.start_frame as i64 + (20_000 - fa.auto.cue_in) * 48;
    let want0 = a0.start_frame as i64 + (fa.auto.segue - fa.auto.cue_in) * 48;
    let (b1, b0) = (voice(&with, "b"), voice(&without, "b"));
    println!(
        "segue override 20000 ms: segue frame {} (wanted {want1}), B's attack {}; without it: segue frame {} \
         (auto {} ms, wanted {want0}), B's attack {} (limit: exact)",
        a1.segue_frame, b1.attack_frame, a0.segue_frame, fa.auto.segue, b0.attack_frame
    );
    assert_eq!(a1.segue_frame as i64, want1);
    assert_eq!(a0.segue_frame as i64, want0);
    assert!((b1.attack_frame as i64 - want1).abs() <= 1);
    assert!((b0.attack_frame as i64 - want0).abs() <= 1);
    assert_ne!(want1, want0);
}

// ---- 10: long intro ----------------------------------------------------------------------------

#[test]
fn m14_10_long_intro_play_and_skip_to_attack() {
    let Some(dir) = fx(&["tone_fade.flac", "the_summoning.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: the_summoning.flac }";
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let x = run(&yaml, &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let cue_in = b.file_start_ms;
    let fb = analysis::analyze::analyze_file(
        &dir.join("the_summoning.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let attack = fb.auto.attack;
    let want_attack = a.segue_frame as i64 + (attack - cue_in - 2000) * 48;
    // B alone at its start: the intro sounds from its first frame, never faded in.
    let y = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
    let ly = left(&y.samples);
    let src = decode_left(&dir.join("the_summoning.flac"));
    let g = 10f32.powf(b.gain_db as f32 / 20.0);
    let from = (cue_in * 48) as usize;
    let mut exact = 0.0f32;
    for i in 144..96_000 {
        exact = exact.max((ly[b.start_frame as usize + i] - src[from + i] * g).abs());
    }
    let first = (b.start_frame as usize..ly.len())
        .find(|&i| ly[i].abs() > 1e-3)
        .unwrap();
    println!(
        "tone_fade -> the_summoning (play, lead_max 2 s): B starts at {} = segue {} - {} frames (limit 96000 +-1); \
         attack at {} (wanted segue + {} = {want_attack}, limit 1 frame); intro first sounds {} frames after its \
         start; solo render equals source x gain from frame 144: largest difference {exact} (limit 0)",
        b.start_frame,
        a.segue_frame,
        a.segue_frame as i64 - b.start_frame as i64,
        b.attack_frame,
        (attack - cue_in - 2000) as f64 / 1000.0,
        first as u64 - b.start_frame
    );
    assert!((a.segue_frame as i64 - b.start_frame as i64 - 96_000).abs() <= 1);
    assert!((b.attack_frame as i64 - want_attack).abs() <= 1);
    assert!(
        first as u64 - b.start_frame < 2400,
        "the intro is audible from the start"
    );
    assert_eq!(exact, 0.0);

    // skip_to_attack: the attack lands on the segue frame.
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n  a: {{ file: tone_fade.flac }}\n  b: {{ file: the_summoning.flac, transition: {{ patch: {{ long_intro_policy: skip_to_attack }} }} }}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
    );
    let x = run(&yaml, &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    println!(
        "skip_to_attack: B starts from {} ms (attack {} - 20), attack at frame {} on the segue frame {} (limit 1)",
        b.file_start_ms, fb.auto.attack, b.attack_frame, a.segue_frame
    );
    assert_eq!(b.file_start_ms, fb.auto.attack - 20);
    assert!((b.attack_frame as i64 - a.segue_frame as i64).abs() <= 1);
}

// ---- 11: album run versus shuffle --------------------------------------------------------------

fn album_items() -> &'static str {
    "  a: { file: slowly_growing_deaf.flac, overrides: { cue_out: 352000 } }\n  b: { file: album_t2.flac }"
}

#[test]
fn m14_11_album_run_versus_shuffle() {
    let Some(dir) = fx(&["slowly_growing_deaf.flac", "album_t2.flac"]) else {
        return;
    };
    let items = album_items();
    let q_shuffle = "queue:\n  - { item: a }\n  - { item: b }\n";
    let q_run = "queue:\n  - { item: a, run: album }\n  - { item: b, run: album }\n";
    let q_normal =
        "queue:\n  - { item: a, run: album, continuity: normal }\n  - { item: b, run: album }\n";
    let mk = |q: &str| format!("station: {{ {SAM} }}\nitems:\n{items}\n{q}");

    // Shuffle: A ends at its cue-out override and the interlude never plays.
    let sh = run(&mk(q_shuffle), &dir);
    let a = voice(&sh, "a");
    let fa = analysis::analyze::analyze_file(
        &dir.join("slowly_growing_deaf.flac"),
        &analysis::markers::ProfileParams::music(),
    )
    .unwrap();
    let want_end = a.start_frame + ((352_000 - fa.auto.cue_in) * 48) as u64;
    let b = voice(&sh, "b");
    let y = run(&solo_yaml(items, "b", b.start_frame, SAM), &dir);
    let (lx, ly) = (left(&sh.samples), left(&y.samples));
    let after: f32 = (a.ended_frame.unwrap() as usize..lx.len())
        .map(|i| (lx[i] - ly.get(i).copied().unwrap_or(0.0)).abs())
        .fold(0.0, f32::max);
    println!(
        "shuffle: A ended at {:?} (cue-out override 352.0 s -> frame {want_end}), kind {}; A's audio after \
         its end differs from silence by {after} (limit 1e-6); render {:.1} s (the interlude would run to 416 s)",
        a.ended_frame,
        sh.sidecar.boundaries[0].kind,
        ms(sh.sidecar.render_frames) / 1000.0
    );
    assert!(a.ended_frame.unwrap().abs_diff(want_end) <= 1);
    assert!(after < 1e-6);
    assert_eq!(sh.sidecar.boundaries[0].kind, "segue");

    // Album run: sample-identical to the two decoded files, joined, at the run gain.
    let al = run(&mk(q_run), &dir);
    let (a, b) = (voice(&al, "a"), voice(&al, "b"));
    let src_a = decode_left(&dir.join("slowly_growing_deaf.flac"));
    let src_b = decode_left(&dir.join("album_t2.flac"));
    let from_a = (a.file_start_ms * 48) as usize;
    assert_eq!(a.gain_db, b.gain_db, "one gain for the run");
    let g = 10f32.powf(a.gain_db as f32 / 20.0);
    let mut expect: Vec<f32> = src_a[from_a..].iter().map(|v| v * g).collect();
    let a_len = expect.len();
    expect.extend(src_b.iter().map(|v| v * g));
    let l = left(&al.samples);
    let start = a.start_frame as usize;
    // Skip the 3 ms declick at the run's first frame; everything else is exact.
    let mut worst = 0.0f32;
    let mut first_bad = None;
    for i in 144..expect.len() {
        let d = (l[start + i] - expect[i]).abs();
        if d > worst {
            worst = d;
        }
        if d != 0.0 && first_bad.is_none() {
            first_bad = Some(i);
        }
    }
    let run_l = {
        let ia = Item::analyze(
            &dir.join("slowly_growing_deaf.flac"),
            &ItemBuild {
                name: "a".into(),
                category: "music".into(),
                overrides: analysis::effective::Overrides {
                    cue_out: Some(352_000),
                    ..Default::default()
                },
                params: serde_json::Value::Null,
                ..Default::default()
            },
        )
        .unwrap();
        let ib = Item::analyze(
            &dir.join("album_t2.flac"),
            &ItemBuild {
                name: "b".into(),
                category: "music".into(),
                params: serde_json::Value::Null,
                ..Default::default()
            },
        )
        .unwrap();
        let da = (ia.markers.cue_out - ia.markers.cue_in) as f64 / 1000.0;
        let db_ = (ib.markers.cue_out - ib.markers.cue_in) as f64 / 1000.0;
        let rg = run_gain(&[(&ia, da), (&ib, db_)]);
        gain_db(&ia, -18.0, 12.0, 6.0, Some(rg))
    };
    println!(
        "album run: A from {} ms plays {a_len} frames (decoder total {:?}), B starts at {} = A start + total \
         (gap or overlap {} frames, limit 0); kind {}; run gain {:+.6} dB (formula {run_l:+.6}); render \
         equals the two decoded files joined x gain over {} frames from frame 144: largest difference {worst} \
         (limit 0); first differing frame {first_bad:?}; interlude included",
        a.file_start_ms,
        a.total_frames,
        b.start_frame,
        b.start_frame as i64 - (a.start_frame + a.total_frames.unwrap()) as i64,
        al.sidecar.boundaries[0].kind,
        a.gain_db,
        expect.len()
    );
    assert_eq!(al.sidecar.boundaries[0].kind, "continuity");
    assert_eq!(a.total_frames, Some(a_len as u64));
    assert_eq!(b.start_frame, a.start_frame + a_len as u64);
    assert!((a.gain_db - run_l).abs() < 1e-9);
    assert_eq!(worst, 0.0);
    assert!(!b.declick_in);
    assert!(a.stop.is_none());
    assert_eq!(
        al.sidecar.render_frames,
        a.start_frame + expect.len() as u64
    );

    // continuity: normal, the run uses the override.
    let no = run(&mk(q_normal), &dir);
    let a = voice(&no, "a");
    println!(
        "continuity: normal: boundary kind {}, A ended at {:?} (override -> frame {want_end})",
        no.sidecar.boundaries[0].kind, a.ended_frame
    );
    assert_eq!(no.sidecar.boundaries[0].kind, "segue");
    assert!(a.ended_frame.unwrap().abs_diff(want_end) <= 1);
}

fn ffmpeg(args: &[&str]) {
    let s = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .expect("ffmpeg");
    assert!(s.success(), "ffmpeg {args:?}");
}

#[test]
fn m14_11b_a_44k_copy_of_the_pair_is_gapless_within_one_frame() {
    let Some(dir) = fx(&["slowly_growing_deaf.flac", "album_t2.flac"]) else {
        return;
    };
    let sub = dir.join(format!("sr44-{}", std::process::id()));
    std::fs::create_dir_all(&sub).unwrap();
    for f in ["slowly_growing_deaf.flac", "album_t2.flac"] {
        ffmpeg(&[
            "-i",
            dir.join(f).to_str().unwrap(),
            "-ar",
            "44100",
            sub.join(f).to_str().unwrap(),
        ]);
    }
    let sub_name = sub.file_name().unwrap().to_string_lossy().to_string();
    let yaml = format!(
        "station: {{ {SAM} }}\nitems:\n  a: {{ file: {sub_name}/slowly_growing_deaf.flac }}\n  b: {{ file: {sub_name}/album_t2.flac }}\nqueue:\n  - {{ item: a, run: album }}\n  - {{ item: b, run: album }}\n"
    );
    let r = run(&yaml, &dir);
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    let mut n44 = 0u64;
    let info = analysis::decode::decode_file(&sub.join("slowly_growing_deaf.flac"), |s, ch, _| {
        n44 += (s.len() / ch) as u64;
    })
    .unwrap();
    let start_ms = a.file_start_ms;
    let want = ((n44 as f64 - start_ms as f64 * 44.1) * 48_000.0 / 44_100.0).round() as i64;
    // The reference: each 44.1 kHz file decoded by the engine to 48 kHz, joined, x gain.
    let dec = |p: &std::path::Path, start_s: f64| {
        let mut d = engine::decode::FileDecoder::open(p, 48_000, start_s).unwrap();
        let mut out = Vec::new();
        while d.decode_next(&mut out).unwrap() {}
        out.chunks_exact(2).map(|f| f[0]).collect::<Vec<f32>>()
    };
    let ra = dec(
        &sub.join("slowly_growing_deaf.flac"),
        start_ms as f64 / 1000.0,
    );
    let rb = dec(&sub.join("album_t2.flac"), 0.0);
    let g = 10f32.powf(a.gain_db as f32 / 20.0);
    let l = left(&r.samples);
    let s0 = a.start_frame as usize;
    let mut worst = 0.0f32;
    for (i, v) in ra.iter().chain(rb.iter()).enumerate().skip(144) {
        worst = worst.max((l[s0 + i] - v * g).abs());
    }
    println!(
        "44.1 kHz pair ({} source frames of A at {} Hz): A plays {:?} frames at 48 kHz (source length x 48000/44100 = \
         {want}, difference {} frames, limit 1); B starts at A start + total: gap or overlap {} frames (limit 0); \
         render equals the two engine-decoded files joined x gain: largest difference {worst} (limit 0)",
        info.frames,
        info.sample_rate,
        a.total_frames,
        a.total_frames.unwrap() as i64 - want,
        b.start_frame as i64 - (a.start_frame + a.total_frames.unwrap()) as i64
    );
    assert!((a.total_frames.unwrap() as i64 - want).abs() <= 1);
    assert_eq!(b.start_frame, a.start_frame + a.total_frames.unwrap());
    assert_eq!(worst, 0.0);
    let _ = std::fs::remove_dir_all(&sub);
}

// ---- 12: gapless limits ------------------------------------------------------------------------

#[test]
fn m14_12_gapless_limits_flac_exact_and_mp3_without_a_header_flagged() {
    let Some(dir) = fx(&["cold_start.flac", "album_t2.flac"]) else {
        return;
    };
    // A FLAC pair is exact: 13 s at 48 kHz = 624,000 frames each.
    let yaml = "station: { preset: sam }\nitems:\n  a: { file: cold_start.flac }\n  b: { file: album_t2.flac }\nqueue:\n  - { item: a, run: r }\n  - { item: b, run: r }\n";
    let r = run(yaml, &dir);
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    println!(
        "FLAC pair: A {:?} frames (source 624000 - cue-in {} ms), B at A start + total: {} frames apart (limit 0), flags none",
        a.total_frames,
        a.file_start_ms,
        b.start_frame as i64 - (a.start_frame + a.total_frames.unwrap()) as i64
    );
    assert_eq!(a.total_frames, Some(624_000 - a.file_start_ms as u64 * 48));
    assert_eq!(b.start_frame, a.start_frame + a.total_frames.unwrap());

    // An MP3 pair without Xing or LAME headers: decoder padding, flagged.
    let sub = dir.join(format!("mp3-{}", std::process::id()));
    std::fs::create_dir_all(&sub).unwrap();
    for (src, dst) in [("cold_start.flac", "m1.mp3"), ("album_t2.flac", "m2.mp3")] {
        ffmpeg(&[
            "-i",
            dir.join(src).to_str().unwrap(),
            "-c:a",
            "libmp3lame",
            "-b:a",
            "128k",
            "-vn",
            "-write_xing",
            "0",
            "-id3v2_version",
            "0",
            sub.join(dst).to_str().unwrap(),
        ]);
    }
    let name = sub.file_name().unwrap().to_string_lossy().to_string();
    let item1 = Item::analyze(
        &sub.join("m1.mp3"),
        &ItemBuild {
            name: "a".into(),
            category: "music".into(),
            params: serde_json::Value::Null,
            ..Default::default()
        },
    )
    .unwrap();
    let flagged = item1
        .flags
        .contains(&analysis::markers::Flag::GaplessUnknown);
    let yaml = format!(
        "station: {{ preset: sam }}\nitems:\n  a: {{ file: {name}/m1.mp3 }}\n  b: {{ file: {name}/m2.mp3 }}\nqueue:\n  - {{ item: a, run: r }}\n  - {{ item: b, run: r }}\n"
    );
    let r = run(&yaml, &dir);
    let (a, b) = (voice(&r, "a"), voice(&r, "b"));
    let total = a.total_frames.unwrap() as i64;
    let true_len = 624_000 - a.file_start_ms * 48;
    println!(
        "MP3 pair without gapless headers: flag gapless_unknown {flagged}; A decodes to {total} frames (source \
         {true_len}, decoder padding {} frames, limit 2400); B at A start + decoded total: {} frames apart (limit 0)",
        total - true_len,
        b.start_frame as i64 - (a.start_frame + a.total_frames.unwrap()) as i64
    );
    assert!(flagged);
    assert!((total - true_len).abs() <= 2400);
    assert_eq!(b.start_frame, a.start_frame + a.total_frames.unwrap());
    let _ = std::fs::remove_dir_all(&sub);
}

// ---- the named fixture quote_unquote: the render half of M1.3 acceptance 9 ---------------------

/// `quote_unquote.flac` (the Mr. Bungle analog: 32 s of quiet, a glass burst at 32.0 s, the band from
/// 34.0 s): with `ovr_cue_in = 33,980 ms` nothing before the cue-in is ever played, and without the
/// override the auto cue-in lets the glass burst sound. Measured on the solo render of the item,
/// which must equal the file times its gain from the cue-in on, sample for sample.
#[test]
fn m14_named_quote_unquote_plays_nothing_before_a_manual_cue_in() {
    let Some(dir) = fx(&["quote_unquote.flac", "tone_fade.flac"]) else {
        return;
    };
    let src = decode_left(&dir.join("quote_unquote.flac"));
    let mk = |ovr: &str| {
        format!("  a: {{ file: tone_fade.flac }}\n  q: {{ file: quote_unquote.flac{ovr} }}")
    };
    for (label, ovr) in [
        (
            "with ovr_cue_in 33,980 ms",
            ", overrides: { cue_in: 33980 }",
        ),
        ("without the override", ""),
    ] {
        let items = mk(ovr);
        let two = format!(
            "station: {{ {SAM} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: q }}\n"
        );
        let r = run(&two, &dir);
        let q = voice(&r, "q");
        let solo = run(&solo_yaml(&items, "q", q.start_frame, SAM), &dir);
        let l = left(&solo.samples);
        let g = 10f32.powf(q.gain_db as f32 / 20.0);
        let from = (q.file_start_ms * 48) as usize;
        let start = q.start_frame as usize;
        // Everything the voice plays is the file from `from` on, exactly (after the 3 ms declick);
        // nothing sounds before its start.
        let mut worst = 0.0f32;
        for i in 144..(60 * 48_000).min(src.len() - from) {
            worst = worst.max((l[start + i] - src[from + i] * g).abs());
        }
        let before = l[..start].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        // The glass burst is 32.0 to 32.4 s of the file; is it in the render?
        let glass_from = 32_000 * 48;
        let glass_present = q.file_start_ms < 32_000 && {
            let idx = start + glass_from - from;
            let level = rms_db(&l[idx..idx + 9_600]);
            level > -70.0
        };
        println!(
            "quote_unquote {label}: plays from {} ms; solo render equals file x gain from that point: largest difference {worst} \
             (limit 0); audio before its start {before} (limit 0); the glass burst at 32.0 s is {} the render",
            q.file_start_ms,
            if glass_present { "IN" } else { "not in" }
        );
        assert_eq!(worst, 0.0);
        assert_eq!(before, 0.0);
        if ovr.is_empty() {
            assert!(glass_present, "the auto cue-in lets the glass burst sound");
        } else {
            assert_eq!(q.file_start_ms, 33_980);
            assert!(!glass_present);
        }
    }
}

// ---- M6 acceptance 5: the segue timing with the `radio` chain active --------------------------------------

/// The M1.4 attack-alignment scenario rendered twice, with the chain bypassed and with `radio` active: the conductor's
/// frames (sidecar) are the same, and the audio's step (found independently, as in `m14_1`) moves by no more than the
/// 1 ms of that test's own limit, because the chain adds no delay (and the 5 ms of the limiters is accounted for in
/// both).
#[test]
fn m6_5_segue_timing_with_radio_active() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let items = "  a: { file: tone_fade.flac }\n  b: { file: pickup.flac }";
    let render = |chain: &str| {
        let yaml = format!(
            "station: {{ {SAM}{chain} }}\nitems:\n{items}\nqueue:\n  - {{ item: a }}\n  - {{ item: b }}\n"
        );
        run(&yaml, &dir)
    };
    let (plain, radio) = (render(""), render(", chain: radio"));
    let (pa, pb) = (voice(&plain, "a"), voice(&plain, "b"));
    let (ra, rb) = (voice(&radio, "a"), voice(&radio, "b"));
    let same = (pa.segue_frame, pb.start_frame, pb.attack_frame)
        == (ra.segue_frame, rb.start_frame, rb.attack_frame);
    // The audio's step: the first 96-sample window whose 500 Hz amplitude passes the midpoint between the levels
    // before and after the segue.
    let step = |r: &station::scenario::Rendered, a: &station::sidecar::VoiceRecord| -> usize {
        let l = left(&r.samples);
        let seg = a.segue_frame as usize;
        let amp = |s: usize| tone_amp(&l, 500.0, s, 96);
        let low = (0..20).map(|k| amp(seg - 30_000 + k * 96)).sum::<f64>() / 20.0;
        let high = (0..20).map(|k| amp(seg + 4_800 + k * 96)).sum::<f64>() / 20.0;
        let mid = (low + high) / 2.0;
        (seg - 6000..seg + 6000).find(|&s| amp(s) >= mid).unwrap() + 48
    };
    let (s_plain, s_radio) = (step(&plain, pa), step(&radio, ra));
    let diff = s_radio as i64 - s_plain as i64;
    // The exact check: the same render without B (the chain is causal, so until B sounds the two renders are identical,
    // sample for sample); the first frame where they differ is where B is first audible.
    let solo = |chain: &str| {
        let yaml = format!("station: {{ {SAM}{chain} }}\nitems:\n  a: {{ file: tone_fade.flac }}\nqueue:\n  - {{ item: a }}\n");
        run(&yaml, &dir)
    };
    let first_diff =
        |full: &station::scenario::Rendered, only_a: &station::scenario::Rendered| -> usize {
            let (x, y) = (left(&full.samples), left(&only_a.samples));
            (0..x.len().min(y.len()))
                .find(|&i| (x[i] - y[i]).abs() > 1e-9)
                .unwrap()
        };
    let (d_plain, d_radio) = (
        first_diff(&plain, &solo("")),
        first_diff(&radio, &solo(", chain: radio")),
    );
    println!(
        "attack alignment with the chain bypassed and with radio active: conductor frames identical {same} (segue {} start {} attack {}); \
         B is first audible at frame {d_plain} bypassed and {d_radio} with radio (the file's own first sound is {} frames after its start frame {}; difference between the two renders {} frames, limit 1); \
         the audio's amplitude step moves {diff} frames ({:.3} ms) because the AGC reshapes the edge, not because anything is delayed",
        ra.segue_frame, rb.start_frame, rb.attack_frame, d_plain as i64 - rb.start_frame as i64, rb.start_frame,
        d_radio as i64 - d_plain as i64,
        diff as f64 * 1000.0 / 48_000.0
    );
    assert!((d_radio as i64 - d_plain as i64).abs() <= 1);
    assert!(same);
}
