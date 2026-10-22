//! The `truncated_fade` flag in the library: set at import for a file that stops in the middle of its fade,
//! not for one that fades to silence, and added to an item imported before the check existed by analyzing
//! it again from its stored file, without importing it again and without touching its overrides.

use fixtures::files::write_wav24;
use fixtures::signals::{pink, ramp_db, silence};
use library::health::Reanalysis;
use library::items::OverridePatch;
use library::tracks::read_track;
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use rusqlite::params;
use std::path::{Path, PathBuf};

const RATE: u32 = 44_100;

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("libtf-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A quiet track (about -31 dBFS RMS, like a home render) with a 24 s fade-out from 185 s. `cut` stops the file
/// at -60 dB below that (the render cut the fade short); otherwise the fade reaches -100 dB and 1 s of digital
/// silence follows.
fn quiet_track(path: &Path, cut: bool) {
    let mut body = pink(RATE, -31.0, 185.0, 7);
    let mut fade = pink(RATE, -31.0, if cut { 24.0 } else { 30.0 }, 8);
    let to = if cut { -29.0 } else { -70.0 };
    ramp_db(&mut fade, 0.0, to);
    body.append(&mut fade);
    if !cut {
        body.append(&mut silence(RATE, 1.0));
    }
    write_wav24(path, &body, RATE, true).unwrap();
}

fn import(lib: &Library, p: &Path) -> i64 {
    match import_file(
        lib,
        p,
        &ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    }
}

fn flags(lib: &Library, id: i64) -> Vec<String> {
    read_track(lib.conn(), id).unwrap().unwrap().flags
}

#[test]
fn a_cut_fade_is_flagged_at_import_and_a_complete_one_is_not() {
    let d = tmpdir("import");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let (cut, full) = (d.join("cut.wav"), d.join("full.wav"));
    quiet_track(&cut, true);
    quiet_track(&full, false);
    let a = import(&lib, &cut);
    let b = import(&lib, &full);
    println!("cut: {:?}\nfull: {:?}", flags(&lib, a), flags(&lib, b));
    assert!(flags(&lib, a).contains(&"truncated_fade".to_string()));
    assert!(!flags(&lib, b).contains(&"truncated_fade".to_string()));
    assert!(lib.stale_analysis_ids().unwrap().is_empty());
}

#[test]
fn analyzing_an_old_item_again_adds_the_flag_and_keeps_the_overrides() {
    let d = tmpdir("again");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let p = d.join("cut.wav");
    quiet_track(&p, true);
    let id = import(&lib, &p);
    // A manual fade-out and a gain trim, as the operator may have set before.
    lib.set_overrides(
        id,
        &OverridePatch {
            fade_out: Some(Some(200_000)),
            gain_trim_db: Some(Some(-1.5)),
            ..OverridePatch::default()
        },
    )
    .unwrap();
    // Make it look like an item imported by the analysis of version 1: no flag, the old version number.
    lib.conn()
        .execute(
            "UPDATE analysis SET algo_version = 1, flags = '[]' WHERE item_id = ?1",
            params![id],
        )
        .unwrap();
    lib.recompute_effective(id).unwrap();
    assert!(!flags(&lib, id).contains(&"truncated_fade".to_string()));
    assert_eq!(lib.stale_analysis_ids().unwrap(), vec![id]);
    let imports_before: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();

    assert_eq!(lib.reanalyze_item(id, None).unwrap(), Reanalysis::Updated);
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    println!("after: {:?} fade_out {:?}", row.flags, row.fade_out);
    assert!(row.flags.contains(&"truncated_fade".to_string()));
    assert_eq!(row.fade_out, Some(200_000), "the manual fade-out stays");
    assert_eq!(row.gain_trim_db, -1.5);
    assert!(lib.stale_analysis_ids().unwrap().is_empty());
    let imports_after: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(imports_before, imports_after, "nothing is imported again");
}

/// The fade-out given as milliseconds before the end (`fade_out_before_end_ms` of the API): converted against the
/// cue-out, 0 or nothing clears it, and a fade that would start too early is moved, with a note.
#[test]
fn a_fade_out_given_before_the_end() {
    let d = tmpdir("fromend");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let p = d.join("cut.wav");
    quiet_track(&p, true);
    let id = import(&lib, &p);
    let before = |ms: Option<i64>| OverridePatch {
        fade_out_before_end: Some(ms),
        ..OverridePatch::default()
    };
    let (row, notes) = lib.set_overrides_noted(id, &before(Some(5_000))).unwrap();
    println!(
        "cue-out {} segue {} attack {}: 5 s before the end is {:?}",
        row.cue_out, row.segue, row.attack, row.fade_out
    );
    assert_eq!(row.fade_out, Some(row.cue_out - 5_000));
    assert!(notes.is_empty());
    let cue_out = row.cue_out;

    // Moved together with the cue-out in the same change: against the new cue-out.
    let (row, _) = lib
        .set_overrides_noted(
            id,
            &OverridePatch {
                cue_out: Some(Some(cue_out - 1_000)),
                fade_out_before_end: Some(Some(5_000)),
                ..OverridePatch::default()
            },
        )
        .unwrap();
    assert_eq!(row.cue_out, cue_out - 1_000);
    assert_eq!(row.fade_out, Some(cue_out - 6_000));
    lib.set_overrides(
        id,
        &OverridePatch {
            cue_out: Some(None),
            ..OverridePatch::default()
        },
    )
    .unwrap();

    // 0 before the end, and nothing: no fade-out.
    let (row, _) = lib.set_overrides_noted(id, &before(Some(0))).unwrap();
    assert_eq!(row.fade_out, None);
    lib.set_overrides_noted(id, &before(Some(5_000))).unwrap();
    let (row, _) = lib.set_overrides_noted(id, &before(None)).unwrap();
    assert_eq!(row.fade_out, None);

    // 150 s before the end would start before the segue minus 30 s: moved there, with a note, and stored so.
    let (row, notes) = lib.set_overrides_noted(id, &before(Some(150_000))).unwrap();
    println!("150 s before the end: {:?}, {notes:?}", row.fade_out);
    assert_eq!(row.fade_out, Some(row.attack.max(row.segue - 30_000)));
    assert_eq!(notes.len(), 1);
    let stored: Option<i64> = lib
        .conn()
        .query_row(
            "SELECT ovr_fade_out FROM item_overrides WHERE item_id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, row.fade_out);

    // From the start, as before; and not both at once.
    let (row, notes) = lib
        .set_overrides_noted(
            id,
            &OverridePatch {
                fade_out: Some(Some(cue_out - 8_000)),
                ..OverridePatch::default()
            },
        )
        .unwrap();
    assert_eq!(row.fade_out, Some(cue_out - 8_000));
    assert!(notes.is_empty());
    assert!(lib
        .set_overrides_noted(
            id,
            &OverridePatch {
                fade_out: Some(Some(1)),
                fade_out_before_end: Some(Some(1)),
                ..OverridePatch::default()
            },
        )
        .is_err());
}
