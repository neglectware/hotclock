//! M5 acceptance 0 and 1: back-timing from a manual start over 200 trials with an 80s-library fixture,
//! the transition planner's real overlaps (`plexamp`) as the timing model.

#[path = "support_m5/mod.rs"]
mod common_m5;
use common_m5::*;
use library::tracks::TrackRow;
use serde_json::json;
use station::catalog::testing::{catalog, row};
use station::item::Item;
use station::sim::{run_timed, PlanTiming};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const T_REL: i64 = 880 * SEC;

/// Fourteen "80s" songs of 3:00 to 5:30, each with an 8 s tail after its segue.
fn rows_80s() -> Vec<TrackRow> {
    let lens = [
        182, 215, 247, 198, 276, 231, 305, 189, 262, 224, 330, 207, 243, 288,
    ];
    lens.iter()
        .enumerate()
        .map(|(i, s)| {
            let mut r = row(
                i as i64 + 1,
                &format!("80s {i}"),
                &format!("Artist {i}"),
                "music",
                "music",
                s * SEC,
            );
            r.segue = r.file_ms - 8 * SEC;
            r
        })
        .collect()
}

fn timing_for(rows: &[TrackRow]) -> PlanTiming {
    let items: HashMap<i64, Arc<Item>> = rows
        .iter()
        .map(|r| {
            (
                r.item_id,
                Arc::new(Item::from_row(r, PathBuf::from(format!("{}.flac", r.item_id))).unwrap()),
            )
        })
        .collect();
    PlanTiming::new(items, "plexamp")
}

fn show_prog(mode: &str, until: serde_json::Value) -> Arc<station::programming::Programming> {
    let play = block(
        200,
        "Pre-show",
        "play",
        "sequence",
        json!({
            "source": {"playlist_id": 1}, "until": until, "end": "natural",
            "back_time": {"mode": mode, "tolerance_s": 45}
        }),
    );
    let fade = block(
        201,
        "Fade",
        "fade_stop",
        "timed",
        json!({"when": {"kind": "relative", "offset_ms": T_REL}, "seconds": 7}),
    );
    prog(
        vec![
            shuffle(1, "80s", 1..=14),
            sequential(2, "Show", vec![play, fade]),
        ],
        vec![],
    )
}

#[test]
fn m5_0_back_timing_from_a_manual_start_lands_within_the_tolerance() {
    let rows = rows_80s();
    let cat = catalog(rows.clone());
    let timing = timing_for(&rows);
    let p = show_prog("best_fit", json!("next"));
    let trials = 200;
    let (mut within, mut deltas_agree, mut recomputed_ok) = (0, 0, 0);
    let mut worst_agree = 0i64;
    let mut fallbacks = 0;
    let mut abs_deltas: Vec<i64> = vec![];
    for n in 0..trials {
        // A different clock offset and a different deck order every time.
        let t0 = local("2027-01-18T18:45:00")
            + (n as i64 * 977) * SEC % (6 * 3600 * SEC)
            + (n as i64 * 13) % 1000;
        let mut s = sched_for(p.clone(), 2, t0, 1000 + n as u64);
        let (plays, _acts, _) = run_timed(
            &mut s,
            &cat,
            &timing,
            t0,
            t0 + T_REL + 10 * SEC,
            usize::MAX,
            |_, _, _, _| {},
        );
        let fill: Vec<_> = plays.iter().filter(|p| p.start_ms < t0 + T_REL).collect();
        let last = fill.last().expect("plays");
        let play_ms = cat.meta(last.item_id()).unwrap().play_ms;
        let actual_end = last.start_ms + play_ms;
        let bt =
            s.st.program
                .as_ref()
                .and_then(|p| p.bt.clone())
                .expect("status");
        let planned_delta = bt.planned_end_ms - (t0 + T_REL);
        let actual_delta = actual_end - (t0 + T_REL);
        abs_deltas.push(actual_delta.abs());
        if actual_delta.abs() <= 45 * SEC {
            within += 1;
        } else {
            fallbacks += 1;
        }
        let diff = (planned_delta - actual_delta).abs();
        worst_agree = worst_agree.max(diff);
        if diff <= 100 {
            deltas_agree += 1;
        }
        // Recomputed after every transition: one `planned` row per fill item dealt, each for its own slot.
        let rows_bt =
            s.st.log
                .rows
                .iter()
                .filter(|r| r.kind == "back_time" && r.how == "planned")
                .count();
        if rows_bt >= fill.len() {
            recomputed_ok += 1;
        }
    }
    let pct = 100.0 * within as f64 / trials as f64;
    println!(
        "0: {within} of {trials} trials ({pct:.1} percent) end within 45 s of T0 + 14:40; {fallbacks} fell back to the fade; the reported delta agrees with the end within 100 ms in {deltas_agree} (worst {worst_agree} ms); a recomputation is logged for every item in {recomputed_ok}"
    );
    abs_deltas.sort_unstable();
    println!(
        "0: |delta| seconds: median {} p90 {} max {}",
        abs_deltas[100] / 1000,
        abs_deltas[180] / 1000,
        abs_deltas[199] / 1000
    );
    assert!(pct >= 95.0);
    assert_eq!(deltas_agree, trials);
    assert_eq!(recomputed_ok, trials);
}

#[test]
fn m5_1_exact_mode_ends_the_closing_item_on_the_target_within_one_period() {
    let rows = rows_80s();
    let cat = catalog(rows.clone());
    let timing = timing_for(&rows);
    let p = show_prog(
        "exact",
        json!({"at": {"kind": "relative", "offset_ms": T_REL}}),
    );
    let trials = 200;
    let (mut ok, mut worst) = (0, 0i64);
    let mut cut_fades = 0;
    for n in 0..trials {
        let t0 = local("2027-01-18T18:45:00") + (n as i64 * 1303) * SEC % (6 * 3600 * SEC);
        let mut s = sched_for(p.clone(), 2, t0, 5000 + n as u64);
        let (plays, _, _) = run_timed(
            &mut s,
            &cat,
            &timing,
            t0,
            t0 + T_REL + 10 * SEC,
            usize::MAX,
            |_, _, _, _| {},
        );
        let fill: Vec<_> = plays.iter().filter(|p| p.start_ms < t0 + T_REL).collect();
        let last = fill.last().unwrap();
        let end = last.start_ms + cat.meta(last.item_id()).unwrap().play_ms;
        let err = end - (t0 + T_REL);
        worst = worst.max(err.abs());
        if err.abs() <= 21 {
            ok += 1;
        }
        if last
            .entry
            .flags
            .iter()
            .any(|f| f == "hard_time" || f == "bt_exact")
        {
            cut_fades += 1;
        }
    }
    println!("1 (exact): {ok} of {trials} closing items end within 21 ms of the target (worst {worst} ms); {cut_fades} were cut in at a hard time with the 2 s fade");
    assert_eq!(ok, trials);
}
