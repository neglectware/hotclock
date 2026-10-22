//! M1.3 acceptance 1, 2, 6 (analysis half) and 9 (analysis half) against the synthetic fixtures.
//! Expected values are `tests/fixtures/expected.toml`, written by hand from the signal
//! definitions; loudness is checked against `ffmpeg -af ebur128`, an independent tool.

use analysis::analyze::{analyze_file, FileAnalysis};
use analysis::effective::{effective_markers, Effective, EffectiveInput, Overrides};
use analysis::markers::{FileFacts, Flag, LongIntroPolicy, ProfileParams};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn expected() -> toml::Table {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/expected.toml");
    std::fs::read_to_string(p).unwrap().parse().unwrap()
}

fn file_for(key: &str) -> String {
    if key.contains('.') {
        key.to_string()
    } else {
        format!("{key}.flac")
    }
}

/// The fixture directory, or `None` (with a message) when ffmpeg is missing.
fn fixture_dir(names: &[&str]) -> Option<PathBuf> {
    let (dir, report) = fixtures::shared_dir(names);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?): {:?}", report.skipped);
        return None;
    }
    Some(dir)
}

fn int(t: &toml::Table, k: &str) -> Option<i64> {
    t.get(k).and_then(|v| v.as_integer())
}

fn range(t: &toml::Table, k: &str) -> Option<(i64, i64)> {
    let a = t.get(k)?.as_array()?;
    Some((a.first()?.as_integer()?, a.get(1)?.as_integer()?))
}

fn names(t: &toml::Table, k: &str) -> Vec<String> {
    t.get(k)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn effective_of(a: &FileAnalysis, ovr: Overrides, policy: LongIntroPolicy) -> Effective {
    effective_markers(&EffectiveInput {
        facts: FileFacts {
            file_ms: a.file_ms,
            l_int: a.l_int,
            lra: a.lra,
        },
        env: &a.env,
        profile: &ProfileParams::music(),
        ovr,
        policy,
        base_flags: &a.base_flags,
        l_w_exact: None,
    })
}

/// Check every marker key of one expected table against an `Effective`. Returns the number of
/// values checked; prints one line per check with its limit.
fn check(label: &str, t: &toml::Table, e: &Effective) -> usize {
    let mut n = 0;
    let mut want = |name: &str,
                    got: i64,
                    exact: Option<i64>,
                    tol: i64,
                    rng: Option<(i64, i64)>,
                    max: Option<i64>| {
        if let Some(x) = exact {
            println!("{label}: {name} {got} (expected {x}, limit {tol} ms)");
            assert!(
                (got - x).abs() <= tol,
                "{label}: {name} {got} vs {x} (limit {tol})"
            );
            n += 1;
        }
        if let Some((lo, hi)) = rng {
            println!("{label}: {name} {got} (expected in [{lo}, {hi}])");
            assert!(
                got >= lo && got <= hi,
                "{label}: {name} {got} not in [{lo}, {hi}]"
            );
            n += 1;
        }
        if let Some(m) = max {
            println!("{label}: {name} {got} (expected at most {m})");
            assert!(got <= m, "{label}: {name} {got} > {m}");
            n += 1;
        }
    };
    want(
        "cue_in",
        e.cue_in,
        int(t, "cue_in_ms"),
        20,
        range(t, "cue_in_range_ms"),
        int(t, "cue_in_max_ms"),
    );
    want(
        "cue_out",
        e.cue_out,
        int(t, "cue_out_ms"),
        20,
        range(t, "cue_out_range_ms"),
        None,
    );
    let atol = int(t, "attack_tol_ms").unwrap_or(20);
    want(
        "attack",
        e.attack,
        int(t, "attack_ms"),
        atol,
        range(t, "attack_range_ms"),
        None,
    );
    want(
        "segue",
        e.segue,
        int(t, "segue_ms"),
        50,
        range(t, "segue_range_ms"),
        None,
    );
    if let Some(m) = int(t, "segue_min_ms") {
        println!("{label}: segue {} (at least {m})", e.segue);
        assert!(e.segue >= m, "{label}: segue {} < {m}", e.segue);
        n += 1;
    }
    if let Some(m) = int(t, "segue_max_ms") {
        println!("{label}: segue {} (at most {m})", e.segue);
        assert!(e.segue <= m, "{label}: segue {} > {m}", e.segue);
        n += 1;
    }
    if let Some(m) = int(t, "max_cue_out_minus_segue_ms") {
        println!(
            "{label}: cue_out - segue {} (at most {m})",
            e.cue_out - e.segue
        );
        assert!(e.cue_out - e.segue <= m);
        n += 1;
    }
    let have: BTreeSet<&str> = e.flags.iter().map(|f| f.name()).collect();
    for f in names(t, "flags") {
        println!("{label}: flag {f} present: {}", have.contains(f.as_str()));
        assert!(
            have.contains(f.as_str()),
            "{label}: missing flag {f} (have {have:?})"
        );
        n += 1;
    }
    for f in names(t, "not_flags") {
        assert!(
            !have.contains(f.as_str()),
            "{label}: unexpected flag {f} (have {have:?})"
        );
        n += 1;
    }
    n
}

/// Acceptance 1 (auto markers of every fixture with marker expectations) and the analysis half
/// of 9 (the override cases of the three named fixtures).
#[test]
fn fixture_markers_match_expected_toml() {
    let exp = expected();
    let files: Vec<String> = exp.keys().map(|k| file_for(k)).collect();
    let refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let Some(dir) = fixture_dir(&refs) else {
        return;
    };
    let mut checks = 0;
    for (key, val) in &exp {
        let t = val.as_table().unwrap();
        let path = dir.join(file_for(key));
        let has_markers = [
            "cue_in_ms",
            "cue_out_ms",
            "attack_ms",
            "segue_range_ms",
            "attack_range_ms",
            "cue_in_max_ms",
            "cue_out_range_ms",
        ]
        .iter()
        .any(|k| t.contains_key(*k));
        let sub_cases = t.values().any(|v| v.is_table());
        if !has_markers && !sub_cases {
            continue;
        }
        let a =
            analyze_file(&path, &ProfileParams::music()).unwrap_or_else(|e| panic!("{key}: {e}"));
        if has_markers {
            checks += check(key, t, &a.auto);
        }
        // Named override cases.
        for (sub, sv) in t {
            let Some(st) = sv.as_table() else { continue };
            let label = format!("{key}.{sub}");
            let ovr = match sub.as_str() {
                "with_ovr_cue_in_33980" => Overrides {
                    cue_in: Some(33_980),
                    ..Default::default()
                },
                "with_ovr_cue_out_352000" => Overrides {
                    cue_out: Some(352_000),
                    ..Default::default()
                },
                _ => Overrides::default(),
            };
            let e = effective_of(&a, ovr, LongIntroPolicy::Play);
            checks += check(&label, st, &e);
        }
    }
    println!("{checks} marker checks against expected.toml");
    assert!(checks >= 60, "only {checks} checks ran");
}

fn ffmpeg_lufs(path: &Path, dual_mono: bool) -> Option<f64> {
    let filter = if dual_mono {
        "pan=stereo|c0=c0|c1=c0,ebur128"
    } else {
        "ebur128"
    };
    let o = Command::new("ffmpeg")
        .args(["-nostats", "-hide_banner", "-i"])
        .arg(path)
        .args(["-af", filter, "-f", "null", "-"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stderr).to_string();
    // The summary block: "    I:         -11.4 LUFS".
    let i = text.rfind("Integrated loudness:")?;
    let rest = &text[i..];
    let j = rest.find("I:")?;
    rest[j + 2..].split_whitespace().next()?.parse().ok()
}

/// Acceptance 2: integrated loudness of each fixture within 0.1 LU of `ffmpeg -af ebur128`.
#[test]
fn integrated_loudness_matches_ffmpeg_ebur128() {
    let Some(dir) = fixture_dir(fixtures::ALL) else {
        return;
    };
    let mut worst = 0.0f64;
    let mut n = 0;
    for name in fixtures::ALL {
        if *name == "truncated.flac" || *name == "corrupt.mp3" || name.contains('/') {
            continue;
        }
        let path = dir.join(name);
        if !path.exists() {
            continue;
        }
        let a =
            analyze_file(&path, &ProfileParams::music()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let Some(reference) = ffmpeg_lufs(&path, a.info.channels == 1) else {
            eprintln!("SKIPPED {name}: ffmpeg ebur128 gave no summary");
            continue;
        };
        let d = (a.l_int - reference).abs();
        println!(
            "{name}: own {:.2} LUFS, ffmpeg {:.1} LUFS, difference {:.3} LU (limit 0.1)",
            a.l_int, reference, d
        );
        // ffmpeg prints one decimal, so its own rounding is up to 0.05.
        assert!(d <= 0.1, "{name}: {d}");
        assert!(
            (a.l_int - a.l_int_crosscheck).abs() < 0.02,
            "{name}: crate {} vs own gate {}",
            a.l_int,
            a.l_int_crosscheck
        );
        worst = worst.max(d);
        n += 1;
    }
    println!("{n} fixtures, worst difference {worst:.3} LU");
    assert!(n >= 15);
}

/// The integrated loudness of real music from the three test sets against ffmpeg (same rule).
#[test]
fn real_audio_loudness_matches_ffmpeg() {
    let Ok(root) = std::env::var("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA is not set");
        return;
    };
    for (set, name) in [
        ("office", "office"),
        ("preshow", "preshow"),
        ("elevator", "elevator"),
    ] {
        let files = analysis::census::collect_files(&PathBuf::from(&root).join(set)).unwrap();
        let mut worst = 0.0f64;
        for f in files.iter().take(3) {
            let a = analyze_file(f, &ProfileParams::music()).unwrap();
            let r = ffmpeg_lufs(f, a.info.channels == 1).unwrap();
            worst = worst.max((a.l_int - r).abs());
        }
        println!("{name}: 3 files, worst difference {worst:.3} LU (limit 0.1)");
        assert!(worst <= 0.1);
    }
}

/// The stored-envelope recompute of `{"segue_lu": -18}` equals a full re-analysis with that
/// parameter (acceptance 6, analysis half; the timing and the database half are in the library
/// tests).
#[test]
fn recompute_from_envelopes_equals_full_reanalysis() {
    let Some(dir) = fixture_dir(&["cold_start.flac", "long_tail.flac", "tone_fade.flac"]) else {
        return;
    };
    for name in ["cold_start.flac", "long_tail.flac", "tone_fade.flac"] {
        let path = dir.join(name);
        let base = analyze_file(&path, &ProfileParams::music()).unwrap();
        let mut p = ProfileParams::music();
        p.apply_json(&serde_json::json!({"segue_lu": -18}));
        let t0 = std::time::Instant::now();
        let recomputed = effective_markers(&EffectiveInput {
            facts: FileFacts {
                file_ms: base.file_ms,
                l_int: base.l_int,
                lra: base.lra,
            },
            env: &base.env,
            profile: &p,
            ovr: Overrides::default(),
            policy: LongIntroPolicy::Play,
            base_flags: &base.base_flags,
            l_w_exact: None,
        });
        let dt = t0.elapsed();
        let full = analyze_file(&path, &p).unwrap();
        println!(
            "{name}: segue -12 LU {} ms, recomputed at -18 LU {} ms in {:?}, full re-analysis {} ms (limit 20 ms difference)",
            base.auto.segue, recomputed.segue, dt, full.auto.segue
        );
        assert!((recomputed.segue - full.auto.segue).abs() <= 20);
        assert!(dt.as_millis() < 50);
        assert_ne!(
            base.auto.segue, recomputed.segue,
            "{name}: the parameter should move the segue"
        );
    }
}

/// `the_summoning`: long_intro, and the policy (acceptance 9, third bullet), including a manual
/// cue-in beating the policy.
#[test]
fn the_summoning_policy() {
    let Some(dir) = fixture_dir(&["the_summoning.flac"]) else {
        return;
    };
    let a = analyze_file(&dir.join("the_summoning.flac"), &ProfileParams::music()).unwrap();
    let play = effective_of(&a, Overrides::default(), LongIntroPolicy::Play);
    assert!(play.flags.contains(&Flag::LongIntro));
    assert!((play.attack - 8000).abs() <= 20, "attack {}", play.attack);
    let skip = effective_of(&a, Overrides::default(), LongIntroPolicy::SkipToAttack);
    println!(
        "the_summoning: skip_to_attack cue_in {} attack {} (cue_in should be attack - 20)",
        skip.cue_in, skip.attack
    );
    assert_eq!(skip.cue_in, skip.attack - 20);
    assert!(skip.flags.contains(&Flag::ColdStart));
    let manual = effective_of(
        &a,
        Overrides {
            cue_in: Some(1234),
            ..Default::default()
        },
        LongIntroPolicy::SkipToAttack,
    );
    assert_eq!(manual.cue_in, 1234, "a manual cue-in beats the policy");
}

/// `quote_unquote` with the manual cue-in: nothing before it is read, so a render starts at the
/// cue-in (the render half is in the library and engine tests; here: the markers are all at or
/// after it and the window loudness is the estimate's).
#[test]
fn quote_unquote_override_ignores_everything_before_the_cue() {
    let Some(dir) = fixture_dir(&["quote_unquote.flac"]) else {
        return;
    };
    let a = analyze_file(&dir.join("quote_unquote.flac"), &ProfileParams::music()).unwrap();
    let without = effective_of(&a, Overrides::default(), LongIntroPolicy::Play);
    assert!((without.attack - 32_000).abs() <= 20);
    assert!(without.flags.contains(&Flag::LongIntroReview));
    let with = effective_of(
        &a,
        Overrides {
            cue_in: Some(33_980),
            ..Default::default()
        },
        LongIntroPolicy::Play,
    );
    assert!(
        with.attack >= with.cue_in && with.attack - with.cue_in <= 20,
        "attack {} cue {}",
        with.attack,
        with.cue_in
    );
    assert!(!with.flags.contains(&Flag::LongIntroReview));
    assert!(
        with.flags.contains(&Flag::GainPending),
        "window loudness is an estimate until the job runs"
    );
    // The estimate from the stored blocks against the exact decode of the window.
    let exact = analysis::analyze::window_loudness_exact(
        &dir.join("quote_unquote.flac"),
        with.window.0,
        with.window.1,
    )
    .unwrap();
    let est =
        analysis::markers::estimate_window_loudness(&a.env, with.window.0, with.window.1).unwrap();
    println!("quote_unquote window loudness: estimate {est:.3} LUFS, exact {exact:.3} LUFS, difference {:.3} LU (limit 0.5)", (est - exact).abs());
    assert!((est - exact).abs() < 0.5);
}

/// `slowly_growing_deaf` with the cue-out override: the interlude after it changes nothing.
#[test]
fn slowly_growing_deaf_interlude_is_ignored() {
    let Some(dir) = fixture_dir(&["slowly_growing_deaf.flac"]) else {
        return;
    };
    let a = analyze_file(
        &dir.join("slowly_growing_deaf.flac"),
        &ProfileParams::music(),
    )
    .unwrap();
    let ovr = Overrides {
        cue_out: Some(352_000),
        ..Default::default()
    };
    let e1 = effective_of(&a, ovr, LongIntroPolicy::Play);
    // Wipe every envelope value after the cue-out (the interlude replaced by different audio).
    let mut env2 = a.env.clone();
    for (i, v) in env2.e10.iter_mut().enumerate() {
        if i as i64 * 10 >= 352_000 {
            *v = -20.0;
        }
    }
    for (i, v) in env2.e50.iter_mut().enumerate() {
        if i as i64 * 10 + 50 > 352_000 {
            *v = -15.0;
        }
    }
    for (i, v) in env2.k200.iter_mut().enumerate() {
        if i as i64 * 20 + 200 > 352_000 {
            *v = -5.0;
        }
    }
    for (i, v) in env2.k10.iter_mut().enumerate() {
        if i as i64 * 10 >= 352_000 {
            *v = -5.0;
        }
    }
    let e2 = effective_markers(&EffectiveInput {
        facts: FileFacts {
            file_ms: a.file_ms,
            l_int: a.l_int,
            lra: a.lra,
        },
        env: &env2,
        profile: &ProfileParams::music(),
        ovr,
        policy: LongIntroPolicy::Play,
        base_flags: &a.base_flags,
        l_w_exact: None,
    });
    println!(
        "slowly_growing_deaf: cue_out {} segue {} (interlude replaced: cue_out {} segue {})",
        e1.cue_out, e1.segue, e2.cue_out, e2.segue
    );
    assert_eq!(
        (e1.cue_in, e1.attack, e1.segue, e1.cue_out),
        (e2.cue_in, e2.attack, e2.segue, e2.cue_out)
    );
}
