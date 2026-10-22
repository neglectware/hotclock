//! M1.6, library side: quarantine with a reason, release, the error counter, and re-analysis that keeps
//! the old analysis when it fails (architecture 1.5).

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::health::{self, Reanalysis};
use library::tracks::read_track;
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use std::path::PathBuf;

fn tmpdir(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("lib16-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn item(lib: &Library, dir: &std::path::Path, hz: f64) -> i64 {
    let p = dir.join(format!("t{hz}.wav"));
    render_tone(&ToneRender {
        out: p.clone(),
        rate: 48_000,
        seconds: 12.0,
        hz,
        level_dbfs: -20.0,
        format: WavFormat::F32,
    })
    .unwrap();
    match import_file(
        lib,
        &p,
        &ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    }
}

#[test]
fn quarantine_records_the_reason_bumps_the_revision_and_release_resets_the_errors() {
    let d = tmpdir("q");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let id = item(&lib, &d, 440.0);
    let rev0 = lib.revision().unwrap();
    assert!(health::quarantine(lib.conn(), id, "silent", 1).unwrap());
    assert!(
        lib.revision().unwrap() > rev0,
        "stations reload on the bump"
    );
    // Idempotent: a second quarantine changes nothing.
    let rev1 = lib.revision().unwrap();
    assert!(!health::quarantine(lib.conn(), id, "decode", 1).unwrap());
    assert_eq!(lib.revision().unwrap(), rev1);
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    assert!(row.quarantined);
    assert_eq!(row.quarantine_reason.as_deref(), Some("silent"));
    assert!(!row.sha256.is_empty());
    let list = health::quarantined_items(lib.conn()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].playback_errors, 1);
    assert!(list[0].at.is_some());
    assert!(health::quarantine(lib.conn(), id, "bogus", 0).is_err());
    assert_eq!(health::add_playback_errors(lib.conn(), id, 2).unwrap(), 3);
    // Release.
    assert!(lib.release_item(id).unwrap());
    assert!(!lib.release_item(id).unwrap());
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    assert!(!row.quarantined);
    assert_eq!(row.quarantine_reason, None);
    assert!(health::quarantined_items(lib.conn()).unwrap().is_empty());
    let errs: i64 = lib
        .conn()
        .query_row(
            "SELECT playback_errors FROM items WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(errs, 0);
}

#[test]
fn a_failed_reanalysis_keeps_the_old_analysis_and_flags_the_item() {
    let d = tmpdir("r");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let id = item(&lib, &d, 660.0);
    let before = lib.item_effective(id).unwrap();
    let l_before: f64 = lib
        .conn()
        .query_row("SELECT l_int FROM analysis WHERE item_id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    let r = lib.reanalyze_item(id, Some("forced")).unwrap();
    assert!(matches!(r, Reanalysis::KeptOld { .. }));
    let after = lib.item_effective(id).unwrap();
    assert_eq!(
        (before.cue_in, before.segue, before.cue_out, before.revision),
        (after.cue_in, after.segue, after.cue_out, after.revision)
    );
    let (flag, err): (i64, Option<String>) = lib
        .conn()
        .query_row(
            "SELECT analysis_failed, analysis_error FROM items WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(flag, 1);
    assert_eq!(err.as_deref(), Some("forced"));
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    assert!(row.analysis_failed);
    assert!(!row.quarantined, "it stays playable");
    // A real re-analysis clears the flag and recomputes.
    assert_eq!(lib.reanalyze_item(id, None).unwrap(), Reanalysis::Updated);
    let l_after: f64 = lib
        .conn()
        .query_row("SELECT l_int FROM analysis WHERE item_id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert!((l_before - l_after).abs() < 1e-6);
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    assert!(!row.analysis_failed);
    assert!(lib.item_effective(id).unwrap().revision > before.revision);
}

/// A stereo f32 WAV of `(seconds, dBFS)` pieces of a 440 Hz tone (`None` for digital silence).
fn pieces(path: &std::path::Path, parts: &[(f64, Option<f64>)]) {
    let mut w = engine::wav::WavWriter::create(path, 48_000, 2, WavFormat::F32).unwrap();
    let mut bytes = Vec::new();
    let mut n = 0u64;
    for (secs, db) in parts {
        let a = db.map_or(0.0, |d| 10f64.powf(d / 20.0) * std::f64::consts::SQRT_2);
        for _ in 0..(secs * 48_000.0) as u64 {
            let v = (a * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / 48_000.0).sin()) as f32;
            bytes.extend_from_slice(&v.to_le_bytes());
            bytes.extend_from_slice(&v.to_le_bytes());
            n += 1;
        }
    }
    w.write_bytes(&bytes).unwrap();
    w.finish().unwrap();
}

#[test]
fn the_longest_quiet_stretch_is_measured_stored_and_backfilled() {
    let d = tmpdir("quiet");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let p = d.join("gap.wav");
    // 10 s of music level, 12 s at -65 dBFS (quiet, not silent), 10 s of music level.
    pieces(
        &p,
        &[
            (10.0, Some(-20.0)),
            (12.0, Some(-65.0)),
            (10.0, Some(-20.0)),
        ],
    );
    let id = match import_file(
        &lib,
        &p,
        &ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    };
    let e = lib.item_effective(id).unwrap();
    println!(
        "quiet: {} ms in the cue window, {} ms in the file",
        e.longest_quiet_ms, e.longest_quiet_file_ms
    );
    assert!(
        (11_900..=12_100).contains(&e.longest_quiet_ms),
        "{}",
        e.longest_quiet_ms
    );
    let row = read_track(lib.conn(), id).unwrap().unwrap();
    assert_eq!(row.longest_quiet_ms, e.longest_quiet_ms);
    let prof = analysis::quiet::QuietProfile::from_json(&row.quiet_profile).unwrap();
    // Read at a threshold below the quiet level, the stretch is not quiet.
    assert!(prof.longest_below(-70.0, false) < 1000);
    // A library from before schema 4: the columns are filled from the stored envelope when it is opened.
    lib.conn()
        .execute_batch(
            "ALTER TABLE item_effective DROP COLUMN quiet_profile;
             ALTER TABLE item_effective DROP COLUMN longest_quiet_ms;
             ALTER TABLE item_effective DROP COLUMN longest_quiet_file_ms;
             UPDATE meta SET value = '3' WHERE key = 'schema_version';",
        )
        .unwrap();
    let rev = e.revision;
    drop(lib);
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let back = read_track(lib.conn(), id).unwrap().unwrap();
    assert_eq!(back.longest_quiet_ms, e.longest_quiet_ms);
    assert_eq!(back.quiet_profile, e.quiet_profile);
    assert!(back.revision > rev, "the stations reload it");
}

/// A station's `state.db` as far as the library service reads it (the station crate writes the real one).
fn station_db(
    path: &std::path::Path,
    instance: &str,
    rows: &[(&str, i64, Option<&str>, i64, &str)],
) {
    let _ = std::fs::remove_file(path);
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE library_outbox (id INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, item_id INTEGER NOT NULL,
           reason TEXT, n INTEGER NOT NULL DEFAULT 0, sha256 TEXT NOT NULL DEFAULT '', at_ms INTEGER NOT NULL);",
    )
    .unwrap();
    c.execute("INSERT INTO meta VALUES('instance', ?1)", [instance])
        .unwrap();
    for (op, id, reason, n, sha) in rows {
        c.execute(
            "INSERT INTO library_outbox(op, item_id, reason, n, sha256, at_ms) VALUES(?1,?2,?3,?4,?5,0)",
            rusqlite::params![op, id, reason, n, sha],
        )
        .unwrap();
    }
}

#[test]
fn the_library_service_applies_a_stations_outbox_once() {
    let d = tmpdir("outbox");
    let lib = Library::open(Paths::from_data_dir(&d.join("data"))).unwrap();
    let a = item(&lib, &d, 440.0);
    let b = item(&lib, &d, 550.0);
    let sha = |id: i64| -> String {
        lib.conn()
            .query_row("SELECT sha256 FROM items WHERE id = ?1", [id], |r| r.get(0))
            .unwrap()
    };
    let (sa, sb) = (sha(a), sha(b));
    let st = d.join("data").join("stations").join("office");
    std::fs::create_dir_all(&st).unwrap();
    let db = st.join("state.db");
    station_db(
        &db,
        "one",
        &[
            ("add_errors", a, None, 1, sa.as_str()),
            ("quarantine", a, Some("truncated"), 0, sa.as_str()),
            // About a file that has been replaced since: not applied.
            ("quarantine", b, Some("silent"), 0, "another-file"),
            ("add_errors", b, None, 1, "another-file"),
        ],
    );
    assert_eq!(health::apply_station_outboxes(&lib), 4);
    let q = |id: i64| -> (i64, Option<String>, i64) {
        lib.conn()
            .query_row(
                "SELECT quarantined, quarantine_reason, playback_errors FROM items WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    };
    assert_eq!(q(a), (1, Some("truncated".into()), 1));
    assert_eq!(q(b), (0, None, 0));
    // Once only.
    assert_eq!(health::apply_station_outboxes(&lib), 0);
    assert_eq!(q(a).2, 1);
    // More rows later: only those.
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute(
        "INSERT INTO library_outbox(op, item_id, reason, n, sha256, at_ms) VALUES('release', ?1, NULL, 0, '', 0)",
        [a],
    )
    .unwrap();
    drop(c);
    assert_eq!(health::apply_station_outboxes(&lib), 1);
    assert_eq!(q(a), (0, None, 0), "released, errors reset");
    // A state.db rebuilt after corruption starts its ids again: it is a new instance, read from the start.
    station_db(
        &db,
        "two",
        &[("quarantine", b, Some("decode"), 0, sb.as_str())],
    );
    assert_eq!(health::apply_station_outboxes(&lib), 1);
    assert_eq!(q(b), (1, Some("decode".into()), 0));
}
