//! Nightly snapshots of a live SQLite database (architecture 5.5), safe to hand to restic.
//!
//! Copying a WAL database's files while it is being written can capture the main file and the log at
//! different moments. `VACUUM INTO` writes one self-contained, consistent file from a read transaction,
//! so nothing stops for it. Each snapshot is written under a temporary name, checked with
//! `PRAGMA integrity_check`, and only then renamed to `<name>-YYYYMMDD.db`; the newest [`KEEP`] are kept.
//! The result goes to `<name>.snapshot.json` beside them, which `/health` reads (`db.last_snapshot` and
//! `db.snapshot_check`).

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Snapshots kept per database.
pub const KEEP: usize = 7;

/// The nightly time, minutes after local midnight (02:30).
pub const AT_MINUTE: i64 = 2 * 60 + 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// Unix ms when the snapshot finished.
    pub taken_ms: i64,
    /// `YYYYMMDD` in its name.
    pub date: String,
    /// `ok`, or what `integrity_check` (or the write) said.
    pub check: String,
    pub file: String,
    pub bytes: u64,
}

impl SnapshotInfo {
    pub fn ok(&self) -> bool {
        self.check == "ok"
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Where the last result is kept.
pub fn status_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.snapshot.json"))
}

/// The last result written for `name`, if any.
pub fn read_status(dir: &Path, name: &str) -> Option<SnapshotInfo> {
    let t = std::fs::read_to_string(status_path(dir, name)).ok()?;
    serde_json::from_str(&t).ok()
}

/// Write one snapshot of `db` as `<dir>/<name>-<date>.db`, verify it, prune to [`KEEP`], record the result.
/// A snapshot that fails its check is left as `<name>-<date>.db.bad` (never as a `.db` that a restore
/// could pick) and reported in the result; an `Err` is only for what stopped the attempt altogether.
pub fn snapshot(
    db: &Path,
    dir: &Path,
    name: &str,
    date: &str,
    now_ms: i64,
) -> Result<SnapshotInfo, String> {
    if !valid_name(name) || date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("bad snapshot name `{name}` or date `{date}`"));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".{name}-{date}.db.tmp"));
    let fin = dir.join(format!("{name}-{date}.db"));
    let bad = dir.join(format!("{name}-{date}.db.bad"));
    let _ = std::fs::remove_file(&tmp);
    let src = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("{}: {e}", db.display()))?;
    src.busy_timeout(std::time::Duration::from_secs(30))
        .map_err(|e| e.to_string())?;
    let tmp_s = tmp
        .to_str()
        .ok_or_else(|| "snapshot path is not UTF-8".to_string())?;
    let made = src.execute("VACUUM INTO ?1", [tmp_s]);
    drop(src);
    let check = match made {
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            format!("snapshot failed: {e}")
        }
        Ok(_) => integrity(&tmp),
    };
    let ok = check == "ok";
    let target = if ok { &fin } else { &bad };
    if tmp.exists() {
        if let Err(e) = std::fs::rename(&tmp, target) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: {e}", target.display()));
        }
    }
    if ok {
        let _ = std::fs::remove_file(&bad);
        prune(dir, name, KEEP);
    }
    let bytes = std::fs::metadata(target).map_or(0, |m| m.len());
    let info = SnapshotInfo {
        taken_ms: now_ms,
        date: date.into(),
        check,
        file: target
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
        bytes,
    };
    // Written under a temporary name and renamed, so a reader never sees half of it.
    let sp = status_path(dir, name);
    let stmp = dir.join(format!(".{name}.snapshot.json.tmp"));
    if let Ok(j) = serde_json::to_string(&info) {
        if std::fs::write(&stmp, j).is_ok() {
            let _ = std::fs::rename(&stmp, &sp);
        }
    }
    Ok(info)
}

/// `PRAGMA integrity_check` on a finished snapshot: `ok`, or the first lines of what it found.
fn integrity(path: &Path) -> String {
    let conn = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => return format!("cannot open the snapshot: {e}"),
    };
    let mut q = match conn.prepare("PRAGMA integrity_check") {
        Ok(q) => q,
        Err(e) => return format!("integrity_check: {e}"),
    };
    let rows: Result<Vec<String>, _> = match q.query_map([], |r| r.get::<_, String>(0)) {
        Ok(it) => it.collect(),
        Err(e) => return format!("integrity_check: {e}"),
    };
    match rows {
        Ok(v) if v.len() == 1 && v[0] == "ok" => "ok".into(),
        Ok(v) => v.into_iter().take(3).collect::<Vec<_>>().join("; "),
        Err(e) => format!("integrity_check: {e}"),
    }
}

/// The snapshot files of `name` in `dir`, newest date first.
pub fn list(dir: &Path, name: &str) -> Vec<(String, PathBuf)> {
    let prefix = format!("{name}-");
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let f = e.file_name().to_string_lossy().into_owned();
            let d = f.strip_prefix(&prefix)?.strip_suffix(".db")?;
            (d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
                .then(|| (d.to_string(), e.path()))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

fn prune(dir: &Path, name: &str, keep: usize) {
    for (_, p) in list(dir, name).into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

/// Restore a snapshot over the live database file: the snapshot is checked first, the old file and its
/// `-wal` and `-shm` are moved aside to `<live>.replaced-<stamp>`, and the snapshot is copied in.
/// The caller has stopped whatever owns the database.
pub fn restore(snapshot: &Path, live: &Path, stamp: &str) -> Result<(), String> {
    let check = integrity(snapshot);
    if check != "ok" {
        return Err(format!(
            "{} does not pass its integrity check: {check}",
            snapshot.display()
        ));
    }
    if let Some(dir) = live.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut p = live.as_os_str().to_owned();
        p.push(suffix);
        let p = PathBuf::from(p);
        if p.exists() {
            let mut to = live.as_os_str().to_owned();
            to.push(format!("{suffix}.replaced-{stamp}"));
            std::fs::rename(&p, PathBuf::from(to)).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let mut part = live.as_os_str().to_owned();
    part.push(".restoring");
    let part = PathBuf::from(part);
    std::fs::copy(snapshot, &part).map_err(|e| format!("{}: {e}", snapshot.display()))?;
    std::fs::rename(&part, live).map_err(|e| format!("{}: {e}", live.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hotclock-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_db(p: &Path, n: i64) {
        let c = Connection::open(p).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch("CREATE TABLE IF NOT EXISTS t(x INTEGER)")
            .unwrap();
        for i in 0..n {
            c.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }
    }

    #[test]
    fn a_snapshot_of_a_live_wal_database_is_one_consistent_file() {
        let d = scratch("live");
        let db = d.join("live.db");
        // Held open, with rows still only in the log.
        let c = Connection::open(&db).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch("CREATE TABLE t(x INTEGER)").unwrap();
        for i in 0..100 {
            c.execute("INSERT INTO t VALUES (?1)", [i]).unwrap();
        }
        let info = snapshot(&db, &d.join("backup"), "library", "20260930", 5).unwrap();
        assert!(info.ok(), "{info:?}");
        let f = d.join("backup").join("library-20260930.db");
        assert!(f.exists());
        assert!(!d.join("backup").join("library-20260930.db-wal").exists());
        let s = Connection::open(&f).unwrap();
        let n: i64 = s
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 100);
        assert_eq!(read_status(&d.join("backup"), "library").unwrap(), info);
        drop((s, c));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_the_newest_seven_are_kept() {
        let d = scratch("keep");
        let db = d.join("a.db");
        make_db(&db, 3);
        for day in 1..=10 {
            snapshot(
                &db,
                &d.join("b"),
                "station-office",
                &format!("202609{day:02}"),
                0,
            )
            .unwrap();
        }
        let l = list(&d.join("b"), "station-office");
        assert_eq!(l.len(), KEEP);
        assert_eq!(l[0].0, "20260910");
        assert_eq!(l[6].0, "20260904");
        // Another name is not touched.
        snapshot(&db, &d.join("b"), "library", "20260910", 0).unwrap();
        assert_eq!(list(&d.join("b"), "station-office").len(), KEEP);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_corrupt_source_is_reported_and_never_becomes_a_snapshot() {
        let d = scratch("bad");
        let db = d.join("a.db");
        std::fs::write(&db, vec![0x41u8; 8192]).unwrap();
        let r = snapshot(&db, &d.join("b"), "library", "20260930", 0);
        if let Ok(i) = r {
            assert!(!i.ok(), "{i:?}");
        }
        assert!(list(&d.join("b"), "library").is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_puts_the_snapshot_back_and_moves_the_old_files_aside() {
        let d = scratch("restore");
        let live = d.join("library.db");
        make_db(&live, 5);
        let s = snapshot(&live, &d.join("b"), "library", "20260930", 0).unwrap();
        assert!(s.ok());
        make_db(&live, 50);
        std::fs::write(d.join("library.db-wal"), b"junk").unwrap();
        restore(&d.join("b").join(&s.file), &live, "t1").unwrap();
        let c = Connection::open(&live).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5);
        assert!(d.join("library.db.replaced-t1").exists());
        assert!(!d.join("library.db-wal").exists());
        drop(c);
        let _ = std::fs::remove_dir_all(&d);
    }
}
