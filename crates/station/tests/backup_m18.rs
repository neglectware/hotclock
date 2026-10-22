//! M1.8: the nightly snapshot of `state.db` (architecture 5.5), on the virtual clock. The station takes it at
//! 02:30 local time, once a day, on the writer's own thread; the result is on `health()` and a snapshot that
//! fails its check raises `backup_check_failed`.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::alerts::code;
use station::offline::Offline;
use station::runtime::{Station, StationOptions};
use std::path::PathBuf;

/// 2027-01-15 02:00:00 UTC.
const T_0200: i64 = 1_800_000_000_000 - 6 * 3_600_000;

struct W {
    dir: PathBuf,
    lib: Library,
}

fn world(name: &str) -> W {
    // The station asks the system for the UTC offset; pin it so the test means the same anywhere.
    std::env::set_var("TZ", "UTC");
    let dir = fixtures::testdir::process_dir().join(format!("b18-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut ids = vec![];
    for i in 0..3 {
        let p = dir.join("src").join(format!("t{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 120.0,
            hz: 300.0 + 100.0 * f64::from(i),
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        match import_file(
            &lib,
            &p,
            &ImportOptions {
                category: Some("music".into()),
                source: "test".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } => ids.push(item_id),
            o => panic!("{o:?}"),
        }
    }
    let mut pl = NewPlaylist::shuffle("Rot");
    pl.mode = Mode::Sequential;
    pl.no_repeat_minutes = 0;
    let pid = lib.create_playlist(&pl).unwrap();
    for i in ids {
        lib.add_item_to_playlist(pid, i, 1.0).unwrap();
    }
    W { dir, lib }
}

impl W {
    fn open(&self, backup: Option<PathBuf>) -> Offline {
        let mut o = StationOptions::new("office", self.lib.paths.media.clone());
        o.default_playlist = Some("Rot".into());
        o.backup_dir = backup;
        let (st, mixer) = Station::open(
            o,
            Some(self.dir.join("stations/office/state.db")),
            Some(self.lib.paths.db.clone()),
            T_0200,
        )
        .unwrap();
        Offline::new(st, mixer, T_0200)
    }
}

#[test]
fn the_snapshot_is_taken_at_0230_once_a_day_and_a_restart_does_not_repeat_it() {
    let w = world("night");
    let backup = w.dir.join("backup");
    let mut off = w.open(Some(backup.clone()));
    off.keep_samples = false;
    off.start().unwrap();
    // 02:00 to 02:29: nothing yet.
    off.run_until(29.0 * 60.0, &[]).unwrap();
    off.station.flush_writer();
    assert!(
        library::backup::list(&backup, "station-office").is_empty(),
        "no snapshot before 02:30"
    );
    // Past 02:30: one, named for the day, checked.
    off.run_until(32.0 * 60.0, &[]).unwrap();
    off.station.flush_writer();
    let l = library::backup::list(&backup, "station-office");
    assert_eq!(l.len(), 1, "{l:?}");
    assert_eq!(l[0].0, "20270115");
    let h = off.station.health();
    let snap = h.snapshot.expect("health carries the snapshot");
    assert!(snap.ok(), "{snap:?}");
    assert!(
        (snap.taken_ms - (T_0200 + 30 * 60_000)).abs() < 60_000,
        "taken at {} ms after 02:00",
        snap.taken_ms - T_0200
    );
    assert!(!off.station.alerts().is_active(code::BACKUP_CHECK_FAILED));
    // The file is a whole database with the plays in it.
    let c = rusqlite::Connection::open(&l[0].1).unwrap();
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM as_run", [], |r| r.get(0))
        .unwrap();
    assert!(n >= 1, "{n} as-run rows in the snapshot");
    // The same day again: not twice. (The 20-minute run keeps the one file.)
    off.run_until(60.0 * 60.0, &[]).unwrap();
    off.station.flush_writer();
    assert_eq!(library::backup::list(&backup, "station-office").len(), 1);
    drop(c);
    drop(off);
    // A restart later the same night does not take another: the sidecar says tonight's is done.
    let mut again = w.open(Some(backup.clone()));
    again.keep_samples = false;
    again.start().unwrap();
    let before = std::fs::metadata(&l[0].1).unwrap().modified().unwrap();
    again.run_until(20.0, &[]).unwrap();
    again.station.flush_writer();
    assert_eq!(
        std::fs::metadata(&l[0].1).unwrap().modified().unwrap(),
        before,
        "a restart the same night does not rewrite the snapshot"
    );
    assert!(
        again.station.health().snapshot.is_some(),
        "status read back at start"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

#[test]
fn a_snapshot_that_cannot_be_written_raises_backup_check_failed() {
    let w = world("fail");
    // A file where the backup directory should be: the snapshot cannot be made.
    let blocked = w.dir.join("backup");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let mut off = w.open(Some(blocked.clone()));
    off.keep_samples = false;
    off.start().unwrap();
    off.run_until(32.0 * 60.0, &[]).unwrap();
    off.station.flush_writer();
    off.run_until(32.0 * 60.0 + 2.0, &[]).unwrap();
    let snap = off
        .station
        .health()
        .snapshot
        .expect("the failure is reported");
    assert!(!snap.ok(), "{snap:?}");
    assert!(off.station.alerts().is_active(code::BACKUP_CHECK_FAILED));
    let _ = std::fs::remove_dir_all(&w.dir);
}
