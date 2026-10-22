//! M1.4a acceptance tests (milestones.md, "M1.4a Transition model and presets"): the presets
//! `basic`, `plexamp`, `sam`, `fm` and `none`, their resolution, the voice-content rule and the
//! preview, as offline renders asserted numerically. Each test prints its measured numbers.

mod support;

use analysis::ramp::{lookup, parse_ramp};
use station::scenario::{preview_pair, render, Scenario};
use support::*;

fn yaml(preset: &str, items: &str, queue: &str) -> String {
    format!("station: {{ preset: {preset} }}\nitems:\n{items}\nqueue:\n{queue}")
}

const TF_PICKUP: &str = "  a: { file: tone_fade.flac }\n  b: { file: pickup.flac }";
const Q2: &str = "  - { item: a }\n  - { item: b }\n";

fn analysis_of(dir: &std::path::Path, file: &str) -> analysis::analyze::FileAnalysis {
    analysis::analyze::analyze_file(&dir.join(file), &analysis::markers::ProfileParams::music())
        .unwrap()
}

// ---- 1: sam unchanged through the new resolution path ------------------------------------------

#[test]
fn m14a_1_sam_selected_at_any_layer_renders_identically() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let base = run(&yaml("sam", TF_PICKUP, Q2), &dir);
    // The station says plexamp; the item overrides, the category and the queue entry each name sam.
    let by_item = run(
        &yaml(
            "plexamp",
            "  a: { file: tone_fade.flac, transition: { preset: sam } }\n  b: { file: pickup.flac, transition: { preset: sam } }",
            Q2,
        ),
        &dir,
    );
    let by_category = run(
        &yaml(
            "plexamp",
            "  a: { file: tone_fade.flac, category_preset: sam }\n  b: { file: pickup.flac, category_preset: sam }",
            Q2,
        ),
        &dir,
    );
    println!(
        "sam through the station, the item, the category: renders identical (frames {}, {}, {})",
        base.samples.len() / 2,
        by_item.samples.len() / 2,
        by_category.samples.len() / 2,
    );
    assert_eq!(base.samples, by_item.samples);
    assert_eq!(base.samples, by_category.samples);
}

// ---- 2: basic -----------------------------------------------------------------------------------

#[test]
fn m14a_2_basic_overlaps_five_seconds_with_straight_line_fades_and_needs_no_analysis() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let x = run(&yaml("basic", TF_PICKUP, Q2), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let bd = &x.sidecar.boundaries[0];
    let l = left(&x.samples);
    let src_a = decode_left(&dir.join("tone_fade.flac"));
    let src_b = decode_left(&dir.join("pickup.flac"));
    let ga = 10f32.powf(a.gain_db as f32 / 20.0);
    let gb = 10f32.powf(b.gain_db as f32 / 20.0);
    let (sa, sb) = (a.start_frame as usize, b.start_frame as usize);
    let len = 240_000.0f64;
    // A: 1 kHz, B: 500 Hz; 480-sample windows hold whole cycles of both. The gain of a voice is its
    // rendered amplitude over the amplitude of its own file (x its normalization gain) in the same
    // window.
    let ratio = |hz: f64, src: &[f32], g: f32, vstart: usize, abs: usize| {
        let mine = tone_amp(&l, hz, abs, 480);
        let own: Vec<f32> = (0..480).map(|j| src[abs - vstart + j] * g).collect();
        mine / tone_amp(&own, hz, 0, 480)
    };
    let mut worst_a = 0.0f64;
    let mut worst_b = 0.0f64;
    let s_b = bd.s_b as usize;
    for x_ in [0.05, 0.2, 0.35, 0.5, 0.65] {
        let k = (x_ * len) as usize;
        let got = ratio(1000.0, &src_a, ga, sa, s_b + k);
        let want = 1.0 - (k as f64 + 240.0 + 1.0) / (len + 1.0);
        worst_a = worst_a.max((db(got) - db(want)).abs());
    }
    for x_ in [0.1, 0.3, 0.5, 0.7, 0.9] {
        let k = (x_ * len) as usize;
        let got = ratio(500.0, &src_b, gb, sb, s_b + k);
        let want = (k as f64 + 240.0) / len;
        worst_b = worst_b.max((db(got) - db(want)).abs());
    }
    println!(
        "basic, tone_fade -> pickup: overlap {} frames (limit 240000 +-1); A's gain within {worst_a:.4} dB of the line 1 to 0 and \
         B's within {worst_b:.4} dB of 0 to 1 at 5 checkpoints each (limit 0.1); A stop: {:?}; B fade in: {:?}",
        bd.overlap_frames, a.stop, b.envelope
    );
    assert!((bd.overlap_frames - 240_000).abs() <= 1);
    assert!(worst_a < 0.1 && worst_b < 0.1);
    assert_eq!(b.envelope.len(), 1);
    assert_eq!(b.envelope[0].curve, "linear");

    // The same with the analysis changed: silence thresholds moved so every auto cue differs.
    let altered = run(
        &yaml(
            "basic",
            "  a: { file: tone_fade.flac, params: { silence_offset_db: 10, silence_min_db: -20, silence_max_db: -20 } }\n  b: { file: pickup.flac, params: { silence_offset_db: 10, silence_min_db: -20, silence_max_db: -20, segue_lu: -30 } }",
            Q2,
        ),
        &dir,
    );
    let same = altered.samples == x.samples;
    println!(
        "basic with every auto cue moved: markers differ (A cue_out {} vs {} ms) and the render is {} ({} frames)",
        altered.sidecar.voices[0].cue_out_frame,
        x.sidecar.voices[0].cue_out_frame,
        if same { "bit-identical" } else { "DIFFERENT" },
        altered.samples.len() / 2
    );
    assert_ne!(
        altered.sidecar.voices[0].cue_out_frame,
        x.sidecar.voices[0].cue_out_frame
    );
    assert!(same, "basic ignores the analysis rows (bounds = file)");
}

// ---- 3: plexamp --------------------------------------------------------------------------------

/// The overlap the formula gives: `min(15 s, t_A + t_B)` from the two files' stored ramps at
/// `T = -16 - g`, `g = -16 - L` (the track's own gain against Plex's -16 LUFS reference).
fn plex_overlap(
    a: &analysis::analyze::FileAnalysis,
    b: &analysis::analyze::FileAnalysis,
) -> (f64, f64, f64) {
    let ta = lookup(&parse_ramp(&a.ramp_end), -16.0 - (-16.0 - a.l_int));
    let tb = lookup(&parse_ramp(&b.ramp_start), -16.0 - (-16.0 - b.l_int));
    (ta, tb, (ta + tb).min(15.0))
}

#[test]
fn m14a_3_plexamp_sums_two_untouched_voices_and_overlaps_by_the_ramp_formula() {
    let Some(dir) = fx(&[
        "tone_fade.flac",
        "pickup.flac",
        "long_tail.flac",
        "the_summoning.flac",
        "album_t2.flac",
        "cold_start.flac",
    ]) else {
        return;
    };
    // (a) No gain envelope on either voice: the render is the sum of the two solo renders.
    let x = run(&yaml("plexamp", TF_PICKUP, Q2), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let y = run(
        &solo_yaml(TF_PICKUP, "b", b.start_frame, "preset: plexamp"),
        &dir,
    );
    let z = run(
        &solo_yaml(TF_PICKUP, "a", a.start_frame, "preset: plexamp"),
        &dir,
    );
    let (lx, ly, lz) = (left(&x.samples), left(&y.samples), left(&z.samples));
    let sum_err = (0..lx.len())
        .map(|i| {
            (lx[i] - ly.get(i).copied().unwrap_or(0.0) - lz.get(i).copied().unwrap_or(0.0)).abs()
        })
        .fold(0.0f32, f32::max);
    let bd = &x.sidecar.boundaries[0];
    let ov = bd.overlap_frames as usize;
    println!(
        "plexamp, tone_fade -> pickup: overlap {} frames ({:.3} s); render minus (A solo + B solo) is at most {sum_err} \
         (limit 1e-6: no envelope on either voice); A's stop {:?}, B's envelope {:?}; limiter active {} frames",
        bd.overlap_frames,
        ms(ov as u64) / 1000.0,
        a.stop,
        b.envelope,
        x.sidecar.limiter_over_1db_frames
    );
    assert!(sum_err < 1e-6);
    assert!(a.stop.is_none() && b.envelope.is_empty());

    // (b) The overlap equals the formula, from the stored ramps, to a frame; B starts from its file
    // start at A's file end minus the overlap.
    let (fa, fb) = (
        analysis_of(&dir, "tone_fade.flac"),
        analysis_of(&dir, "pickup.flac"),
    );
    let (ta, tb, ovl) = plex_overlap(&fa, &fb);
    let want = (ovl * 48_000.0).round() as i64;
    let a_end = a.start_frame + a.total_frames.unwrap();
    println!(
        "  formula: t_A {ta:.3} s, t_B {tb:.3} s, overlap min(15, {:.3}) = {ovl:.3} s = {want} frames; rendered {} frames \
         (difference {}, limit 1); B starts at {} = A's file end {a_end} - overlap; B's file start {} ms (limit 0)",
        ta + tb,
        bd.overlap_frames,
        bd.overlap_frames - want,
        b.start_frame,
        b.file_start_ms
    );
    assert!((bd.overlap_frames - want).abs() <= 1);
    assert_eq!(b.start_frame as i64, a_end as i64 - bd.overlap_frames);
    assert_eq!(b.file_start_ms, 0);

    // (c) The cap: a pair whose t_A + t_B exceeds 15 s overlaps for exactly 15.0 s.
    let items = "  a: { file: long_tail.flac }\n  b: { file: the_summoning.flac }";
    let x = run(&yaml("plexamp", items, Q2), &dir);
    let bd = &x.sidecar.boundaries[0];
    let (fa, fb) = (
        analysis_of(&dir, "long_tail.flac"),
        analysis_of(&dir, "the_summoning.flac"),
    );
    let (ta, tb, _) = plex_overlap(&fa, &fb);
    println!(
        "  cap: long_tail -> the_summoning: t_A {ta:.3} s + t_B {tb:.3} s = {:.3} s > 15; overlap {} frames = {:.4} s (limit 15.0 exactly, {}), capped flag {:?}",
        ta + tb,
        bd.overlap_frames,
        bd.overlap_frames as f64 / 48_000.0,
        720_000,
        bd.ramp
    );
    assert!(ta + tb > 15.0, "the pair is long enough to hit the cap");
    assert!((bd.overlap_frames - 720_000).abs() <= 1);

    // (d) Empty ramps: a butt joint.
    let items = "  a: { file: tone_fade.flac, empty_ramps: true }\n  b: { file: pickup.flac, empty_ramps: true }";
    let x = run(&yaml("plexamp", items, Q2), &dir);
    let bd = &x.sidecar.boundaries[0];
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    println!(
        "  empty ramps: overlap {} frames (limit 0); B starts at {} = A's end {}",
        bd.overlap_frames,
        b.start_frame,
        a.start_frame + a.total_frames.unwrap()
    );
    assert_eq!(bd.overlap_frames, 0);
    assert_eq!(b.start_frame, a.start_frame + a.total_frames.unwrap());

    // (e) Bounds are the file: moving the auto cues changes nothing; a manual cue-in on B starts B
    // there and uses the ramp computed inside its window.
    let moved = "  a: { file: tone_fade.flac, params: { silence_offset_db: 10, silence_min_db: -20, silence_max_db: -20 } }\n  b: { file: pickup.flac, params: { silence_offset_db: 10, silence_min_db: -20, silence_max_db: -20 } }";
    let m = run(&yaml("plexamp", moved, Q2), &dir);
    let plain = run(&yaml("plexamp", TF_PICKUP, Q2), &dir);
    println!(
        "  auto cues moved: render {}",
        if m.samples == plain.samples {
            "bit-identical"
        } else {
            "DIFFERENT"
        }
    );
    assert_eq!(m.samples, plain.samples);

    let items =
        "  a: { file: tone_fade.flac }\n  b: { file: pickup.flac, overrides: { cue_in: 1500 } }";
    let x = run(&yaml("plexamp", items, Q2), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let bd = &x.sidecar.boundaries[0];
    let fb = analysis_of(&dir, "pickup.flac");
    let env = &fb.env;
    let lw =
        analysis::analyze::window_loudness_exact(&dir.join("pickup.flac"), 1500, 16_800).unwrap();
    let (rs, _) = analysis::ramp::window_ramps(env, lw, 1500, 16_800);
    let fa = analysis_of(&dir, "tone_fade.flac");
    let ta = lookup(&parse_ramp(&fa.ramp_end), -16.0 - (-16.0 - fa.l_int));
    let tb = lookup(&rs, -16.0 - (-16.0 - lw));
    let want = ((ta + tb).min(15.0) * 48_000.0).round() as i64;
    println!(
        "  manual cue-in 1500 ms on B: B plays from {} ms (limit 1500); t_B in the window {tb:.3} s (whole-file ramp would give {:.3}); \
         overlap {} frames, formula {want} (difference {}, limit 1)",
        b.file_start_ms,
        lookup(&parse_ramp(&analysis_of(&dir, "pickup.flac").ramp_start), -16.0 - (-16.0 - analysis_of(&dir, "pickup.flac").l_int)),
        bd.overlap_frames,
        bd.overlap_frames - want
    );
    assert_eq!(b.file_start_ms, 1500);
    assert!((bd.overlap_frames - want).abs() <= 1);
    let _ = a;

    // (f) An album run in order is gapless (continuity): no overlap, no ramps read, sample-exact.
    let items = "  a: { file: cold_start.flac }\n  b: { file: album_t2.flac }";
    let q = "  - { item: a, run: r }\n  - { item: b, run: r }\n";
    let x = run(&yaml("plexamp", items, q), &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let src_a = decode_left(&dir.join("cold_start.flac"));
    let src_b = decode_left(&dir.join("album_t2.flac"));
    let g = 10f32.powf(a.gain_db as f32 / 20.0);
    let l = left(&x.samples);
    let mut worst = 0.0f32;
    for (i, v) in src_a.iter().chain(src_b.iter()).enumerate().skip(144) {
        worst = worst.max((l[a.start_frame as usize + i] - v * g).abs());
    }
    let runs = silent_runs(&x.samples, -60.0, 2_400);
    println!(
        "  album run under plexamp: kind {}, B at A start + total (difference {}, limit 0); render equals the joined files x gain: \
         {worst} (limit 0); silent runs over 50 ms below -60 dBFS: {} (limit 0)",
        x.sidecar.boundaries[0].kind,
        b.start_frame as i64 - (a.start_frame + a.total_frames.unwrap()) as i64,
        runs.iter().filter(|r| r.0 > a.start_frame as usize + 2000 && r.0 + r.1 < l.len() - 2000).count()
    );
    assert_eq!(x.sidecar.boundaries[0].kind, "continuity");
    assert_eq!(worst, 0.0);
}

// ---- 4: fm -------------------------------------------------------------------------------------

#[test]
fn m14a_4_fm_starts_a_long_intro_on_its_attack_and_cuts_the_tail() {
    let Some(dir) = fx(&["cold_end.flac", "the_summoning.flac", "tone_fade.flac"]) else {
        return;
    };
    let fb = analysis_of(&dir, "the_summoning.flac");
    for a_file in ["cold_end.flac", "tone_fade.flac"] {
        let items = format!("  a: {{ file: {a_file} }}\n  b: {{ file: the_summoning.flac }}");
        let x = run(&yaml("fm", &items, Q2), &dir);
        let (a, b) = (voice(&x, "a"), voice(&x, "b"));
        let bd = &x.sidecar.boundaries[0];
        let tail = a.ended_frame.unwrap() as i64 - b.attack_frame as i64;
        println!(
            "fm, {a_file} -> the_summoning: B starts from {} ms (attack {} - 20); B's attack at frame {} on A's exit {} \
             (difference {}, limit 1); A ends {tail} frames = {:.3} s after B's attack ({})",
            b.file_start_ms,
            fb.auto.attack,
            b.attack_frame,
            bd.e_a,
            b.attack_frame as i64 - bd.e_a,
            tail as f64 / 48_000.0,
            if a_file == "cold_end.flac" { "limit 0.5 s" } else { "tone_fade's tail is cut by the 2 s cap: the preset table says 2 s, the description 0.5 s" }
        );
        assert_eq!(b.file_start_ms, fb.auto.attack - 20);
        assert!((b.attack_frame as i64 - bd.e_a).abs() <= 1);
        if a_file == "cold_end.flac" {
            assert!(
                tail <= 24_000,
                "A's tail after B's attack is at most 0.5 s: {tail}"
            );
        } else {
            assert!(tail <= 2 * 48_000 + 1);
        }
    }
}

// ---- 5: none -----------------------------------------------------------------------------------

#[test]
fn m14a_5_none_ends_a_at_its_cue_out_and_starts_b_on_the_next_frame() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let x = run(
        &format!("station: {{ preset: none, start_s: 0 }}\nitems:\n{TF_PICKUP}\nqueue:\n{Q2}"),
        &dir,
    );
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let fa = analysis_of(&dir, "tone_fade.flac");
    let a_len = ((fa.auto.cue_out - fa.auto.cue_in) * 48) as u64;
    let played = a.played.unwrap() + b.played.unwrap();
    println!(
        "none: A plays {:?} frames (cue window {a_len}), ended at {:?}; B starts at {} (A's end {}); voices' frames {played} vs render {} \
         (limit equal); A stop {:?}; envelopes {} and {}",
        a.played,
        a.ended_frame,
        b.start_frame,
        a.start_frame + a_len,
        x.sidecar.render_frames,
        a.stop,
        a.envelope.len(),
        b.envelope.len()
    );
    assert_eq!(a.played, Some(a_len));
    assert_eq!(b.start_frame, a.start_frame + a_len);
    assert_eq!(a.ended_frame, Some(b.start_frame));
    assert_eq!(played, x.sidecar.render_frames);
    assert_eq!(a.stop.as_ref().unwrap().curve, "declick");
    assert!(a.envelope.is_empty() && b.envelope.is_empty());
    assert!(b.declick_in);
}

// ---- 6: resolution order -----------------------------------------------------------------------

#[test]
fn m14a_6_resolution_order_a_side_b_side_and_per_entry() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac", "cold_start.flac"]) else {
        return;
    };
    // Four layers, each naming different parameters, each winning for exactly its own.
    let items = "  a: { file: tone_fade.flac, category_preset: basic, category_patch: { tail_max_s: 3, gap_s: 0 }, transition: { patch: { exit: { mode: last_crossing } } } }\n  b: { file: pickup.flac, transition: { preset: fm } }\n  c: { file: cold_start.flac }";
    let q = "  - { item: a }\n  - { item: b, transition: { patch: { lead_max_s: 0.25, accept_lead_s: 1.0 } } }\n  - { item: c }\n";
    let sc = Scenario::from_yaml(&yaml("sam", items, q)).unwrap();
    let r = render(&sc, Some(&dir)).unwrap();
    let (a, b, c) = (voice(&r, "a"), voice(&r, "b"), voice(&r, "c"));
    let (b1, b2) = (&r.sidecar.boundaries[0], &r.sidecar.boundaries[1]);
    println!(
        "resolution: station sam; A's category preset basic (out fade linear 5 s, bounds file) with a category cap; A's item patch \
         (exit last_crossing); B's item preset fm; the queue entry into B patches lead_max_s 0.25 and accept_lead_s 1.0. \
         Boundary A->B: kind {}, A's stop {:?} (the category preset's 5 s linear out fade beats sam's none), B's lead {} frames \
         (limit: min(pickup 1.52 s, accept 1.0 s, lead_max 0.25 s) = 12000); boundary B->C (no entry patch): lead {} frames",
        b1.kind, a.stop, b1.lead_frames, b2.lead_frames
    );
    // A-side (A's chain) and B-side (B's chain) both act at one boundary.
    let stop = a.stop.clone().expect("A's stop");
    assert_eq!(stop.curve, "linear");
    assert_eq!(b1.lead_frames, 12_000);
    // The per-entry patch affects that boundary only: B -> C keeps sam's own lead (cold start: no pickup).
    assert_eq!(b2.lead_frames, 0);
    assert!(b.start_frame > 0 && c.start_frame > b.start_frame);

    // The layered parameters themselves.
    let book = common::transition::PresetBook::builtin();
    let cat_patch = serde_json::json!({"tail_max_s": 3, "gap_s": 0});
    let item = common::transition::PresetPatch {
        preset: None,
        patch: Some(serde_json::json!({"exit": {"mode": "last_crossing"}})),
    };
    let entry = common::transition::PresetPatch {
        preset: None,
        patch: Some(serde_json::json!({"lead_max_s": 0.25})),
    };
    let p = common::transition::resolve(
        &book,
        &common::transition::Chain {
            station_preset: "sam",
            category_preset: Some("basic"),
            category_patch: Some(&cat_patch),
            item: Some(&item),
            entry: Some(&entry),
        },
    )
    .unwrap();
    println!(
        "  A's chain: exit {:?} (item patch), align {:?} (category preset basic), bounds {:?} (basic), tail_max_s {} (category cap on sam's 12, \
         then basic names none), lead_max_s {} (entry)",
        p.exit.mode, p.align.mode, p.bounds, p.tail_max_s, p.lead_max_s
    );
    assert_eq!(p.exit.mode, common::transition::ExitMode::LastCrossing);
    assert_eq!(p.align.mode, common::transition::AlignMode::FixedOverlap);
    assert_eq!(p.bounds, common::transition::Bounds::File);
    assert_eq!(p.lead_max_s, 0.25);
    assert_eq!(p.tail_max_s, 3.0);
}

// ---- 7: voice-content rule ---------------------------------------------------------------------

#[test]
fn m14a_7_a_song_after_a_liner_never_covers_it_under_fm() {
    let Some(dir) = fx(&[
        "liner_8s.flac",
        "pickup.flac",
        "voice_short.flac",
        "tone_fade.flac",
    ]) else {
        return;
    };
    for (liner, song) in [
        ("liner_8s", "pickup"),
        ("voice_short", "pickup"),
        ("liner_8s", "tone_fade"),
    ] {
        let items =
            format!("  l: {{ file: {liner}.flac, category: liner }}\n  b: {{ file: {song}.flac }}");
        let q = "  - { item: l }\n  - { item: b }\n";
        let x = run(&yaml("fm", &items, q), &dir);
        let (l, b) = (voice(&x, "l"), voice(&x, "b"));
        let bd = &x.sidecar.boundaries[0];
        println!(
            "fm, {liner} -> {song}: the liner's exit (last crossing at -8 LU, its segue marker at frame {}) is frame {}; {song} starts at {} \
             ({} frames after the exit, limit >= 0), lead {} frames (limit 0; the song's own pickup would be {:.2} s, fm allows 3 s)",
            l.segue_frame,
            bd.e_a,
            b.start_frame,
            b.start_frame as i64 - bd.e_a,
            bd.lead_frames,
            ms(b.attack_frame - b.start_frame) / 1000.0
        );
        assert!(b.start_frame as i64 >= bd.e_a);
        assert_eq!(bd.lead_frames, 0);
    }
}

// ---- 8: preview --------------------------------------------------------------------------------

#[test]
fn m14a_8_preview_is_sample_identical_to_the_scenario_renderer() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let sc = Scenario::from_yaml(&yaml("basic", TF_PICKUP, Q2)).unwrap();
    let full = render(&sc, Some(&dir)).unwrap();
    let pre = preview_pair(&sc, Some(&dir), 2.0, 2.0).unwrap();
    let bd = &full.sidecar.boundaries[0];
    let from = bd.s_b.saturating_sub(96_000) as usize;
    let to = ((bd.f_end_a + 96_000) as usize).min(full.sidecar.render_frames as usize);
    let same = pre.samples == full.samples[from * 2..to * 2];
    println!(
        "preview (basic): {} frames from {from}; identical to the scenario renderer's slice: {same}",
        pre.samples.len() / 2
    );
    assert!(same);
    assert_eq!(pre.samples.len() / 2, to - from);
}

// ---- 9: the office scenario under every preset -------------------------------------------------

#[test]
fn m14a_9_twenty_mixed_items_under_each_preset_have_no_click_and_no_dead_air() {
    let Some(dir) = fx(&OFFICE_FILES) else {
        return;
    };
    let mut sources: std::collections::HashMap<String, (f32, f32, f32)> = Default::default();
    for p in ["plexamp", "basic", "sam", "fm", "none"] {
        let (y, _) = office_yaml(p);
        let r = run(&y, &dir);
        let l = left(&r.samples);
        // Each voice's own largest step and its own last sample (a file that ends loud ends with
        // that step at EOF: the file's own, not a click of the executor's).
        let mut own: Vec<(usize, usize, f32, f32)> = vec![];
        for v in &r.sidecar.voices {
            let g = 10f32.powf(v.gain_db as f32 / 20.0);
            let (step, last, _) = *sources.entry(v.path.clone()).or_insert_with(|| {
                let src = decode_left(std::path::Path::new(&v.path));
                (
                    max_step(&src),
                    src.last().copied().unwrap_or(0.0).abs(),
                    0.0,
                )
            });
            let eof = v.end_reason.as_deref() == Some("eof");
            own.push((
                v.started_frame.unwrap() as usize,
                v.ended_frame.unwrap() as usize,
                step * g,
                if eof { last * g } else { 0.0 },
            ));
        }
        let mut worst_excess = f32::NEG_INFINITY;
        let mut worst_at = 0usize;
        let mut click = 0.0f32;
        for i in 1..l.len() {
            let mut allowed = 0.01f32;
            for &(s, e, st, end_step) in &own {
                if s <= i + 1 && e + 1 >= i {
                    allowed += st;
                }
                if i >= e && i <= e + 1 {
                    allowed += end_step;
                }
            }
            let step = (l[i] - l[i - 1]).abs();
            click = click.max(step);
            if step - allowed > worst_excess {
                worst_excess = step - allowed;
                worst_at = i;
            }
        }
        let (bad, checked) = dead_air(&r, &dir);
        println!(
            "office scenario under {p}: {} items, {:.1} s; largest step {click:.4}; worst excess over the voices' own steps + 0.01: \
             {worst_excess:.5} at frame {worst_at} (limit <= 0); silent runs over 50 ms: {checked}, dead air {bad} (limit 0); starved {} frames",
            r.sidecar.voices.len(),
            ms(r.sidecar.render_frames) / 1000.0,
            r.sidecar.starved_frames
        );
        assert!(
            worst_excess <= 0.0,
            "{p}: a click of {worst_excess} over the allowed step at frame {worst_at}"
        );
        assert_eq!(bad, 0, "{p}: dead air");
        assert_eq!(r.sidecar.starved_frames, 0);
    }
}

// ---- the Plexamp replay -------------------------------------------------------------------------

/// The logged Plexamp transitions replayed through Hotclock's own ramps and planner, for every pair
/// whose two tracks both exist in the local test sets (few do). Informational: the target is a
/// median error under 0.25 s, asserted when at least one pair can be replayed.
#[test]
fn m14a_plexamp_replay_of_the_logged_pairs() {
    let root = std::env::var("HOTCLOCK_TESTDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Developer/hotclock-testdata")
        });
    let Some(rep) = station::replay::plexamp_replay(&root, 48_000) else {
        eprintln!("SKIPPED: no calibration data under {}", root.display());
        return;
    };
    print!("{}", rep.format());
    if let Some((median, _, _)) = rep.abs_error_stats() {
        assert!(median < 250.0, "median overlap error {median} ms");
    }
}

// ---- a custom preset with the dB-linear (`log`) fade -----------------------------------------

/// `log` is linear in dB down to -60 dB: a custom preset that is `basic` with both fades on the
/// `log` curve (2.6, 2.2). Fades to silence go through a stop, the fade in through the envelope.
#[test]
fn m14a_log_fades_are_straight_lines_in_decibels() {
    let Some(dir) = fx(&["tone_fade.flac", "pickup.flac"]) else {
        return;
    };
    let custom = r#"{"exit":{"mode":"fixed_before_end","seconds":5},"out_fade":{"curve":"log","seconds":5},"align":{"mode":"fixed_overlap","overlap_s":5},"in_fade":{"curve":"log","seconds":5,"skip_if_cold_start":false},"lead_max_s":0,"accept_lead_s":0,"bounds":"file","gap_s":0}"#;
    let y = format!(
        "station: {{ preset: mylog, presets: {{ mylog: {custom} }} }}\nitems:\n{TF_PICKUP}\nqueue:\n{Q2}"
    );
    let x = run(&y, &dir);
    let (a, b) = (voice(&x, "a"), voice(&x, "b"));
    let bd = &x.sidecar.boundaries[0];
    let l = left(&x.samples);
    let src_a = decode_left(&dir.join("tone_fade.flac"));
    let src_b = decode_left(&dir.join("pickup.flac"));
    let (ga, gb) = (
        10f32.powf(a.gain_db as f32 / 20.0),
        10f32.powf(b.gain_db as f32 / 20.0),
    );
    let s_b = bd.s_b as usize;
    let ratio = |hz: f64, src: &[f32], g: f32, vstart: usize, abs: usize| {
        let mine = tone_amp(&l, hz, abs, 480);
        let own: Vec<f32> = (0..480).map(|j| src[abs - vstart + j] * g).collect();
        mine / tone_amp(&own, hz, 0, 480)
    };
    let (mut wa, mut wb) = (0.0f64, 0.0f64);
    for x_ in [0.1, 0.3, 0.5, 0.6] {
        let k = (x_ * 240_000.0) as usize;
        let xa = (k + 240) as f64 / 240_001.0;
        wa = wa.max(
            (db(ratio(1000.0, &src_a, ga, a.start_frame as usize, s_b + k)) - (-60.0 * xa)).abs(),
        );
    }
    for x_ in [0.4, 0.6, 0.8, 0.95] {
        let k = (x_ * 240_000.0) as usize;
        let xb = (k + 240) as f64 / 240_000.0;
        wb = wb.max(
            (db(ratio(500.0, &src_b, gb, b.start_frame as usize, s_b + k)) - (-60.0 * (1.0 - xb)))
                .abs(),
        );
    }
    println!(
        "log fades: A's stop {:?}, B's envelope {:?}; A within {wa:.4} dB of a line from 0 to -60 dB and B within {wb:.4} dB of a line from -60 to 0 dB \
         at 4 checkpoints each (limit 0.1)",
        a.stop, b.envelope
    );
    assert_eq!(a.stop.as_ref().unwrap().curve, "db_linear");
    assert_eq!(b.envelope[0].curve, "db_linear");
    assert!(wa < 0.1 && wb < 0.1);
}
