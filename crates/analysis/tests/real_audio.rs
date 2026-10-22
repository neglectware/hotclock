//! Census over real audio, only when `HOTCLOCK_TESTDATA` points at a directory holding the
//! read-only copies `office/`, `preshow/` and `elevator/` (kept outside the repository, never
//! committed). Skipped, with a message, when it is unset. The three sets are three moods and are
//! never mixed: each is inspected on its own.

use analysis::census::{collect_files, inspect, summarize, Verdict};
use std::path::PathBuf;

#[test]
fn census_over_real_test_sets() {
    let Ok(root) = std::env::var("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA is not set");
        return;
    };
    for set in ["office", "preshow", "elevator"] {
        let dir = PathBuf::from(&root).join(set);
        if !dir.is_dir() {
            eprintln!("SKIPPED {set}: {} does not exist", dir.display());
            continue;
        }
        let files = collect_files(&dir).unwrap();
        let reports: Vec<_> = files.iter().map(|f| inspect(f)).collect();
        let s = summarize(&reports);
        println!("{set}: {s:?}");
        for r in reports.iter().filter(|r| r.verdict != Verdict::Ok) {
            println!(
                "  {}: {} ({:?})",
                r.path.display(),
                r.verdict.label(),
                r.detail
            );
        }
        assert!(!files.is_empty(), "{set}: no audio files");
        assert_eq!(s.undecodable, 0, "{set}: every file must decode");
    }
}
