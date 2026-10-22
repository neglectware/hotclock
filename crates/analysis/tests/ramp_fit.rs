//! The Plexamp ramp fit (M1.3 addition). Needs `HOTCLOCK_TESTDATA`; skipped without it.
//! Prints the whole report (matched count, variant table, error distribution, pairs replay).

use analysis::ramp_fit::{format_report, run};
use std::path::PathBuf;

#[test]
fn hotclock_ramps_reproduce_plexs_crossing_times() {
    let Ok(root) = std::env::var("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA is not set");
        return;
    };
    let Some(r) = run(&PathBuf::from(root)) else {
        eprintln!("SKIPPED: no calibration data under HOTCLOCK_TESTDATA");
        return;
    };
    println!("{}", format_report(&r));
    assert!(
        r.matched >= 10,
        "only {} calibration tracks matched local files",
        r.matched
    );
    // Our own loudness is Plex's loudness (same BS.1770 measurement).
    assert!(r.max_loudness_diff < 0.05, "{}", r.max_loudness_diff);
    // Target: crossing times at T = -16 - g within 0.25 s for 95 percent of the tracks. The two
    // ramps of a track are two comparisons; the target is met when at most 5 percent of all of
    // them miss.
    let both = &r.chosen_both;
    let pct = 100.0 * both.within as f64 / both.n as f64;
    println!(
        "within 0.25 s: {}/{} = {pct:.1} percent (target 95)",
        both.within, both.n
    );
    assert!(pct >= 95.0);
    assert!(
        r.chosen_start.within == r.chosen_start.n,
        "start ramps: every track within 0.25 s"
    );
    // The pairs replay is informational here (an M1.4a test), but it must have run.
    println!("pairs replayed: {} of {}", r.pairs.len(), r.pairs_total);
}
