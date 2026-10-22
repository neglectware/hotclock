//! `state.db`, the station's own database (architecture 5.3): one writer thread owns the connection, so
//! playout never waits on SQLite; everything reaches it through a channel. The checkpoint is rewritten
//! every 2 s and at transitions; the queue, decks, counters, run and history are written whenever the
//! scheduler changes; as-run rows are written as items start and end; the track cache and the
//! programming cache hold what the station needs to play without `library.db`.
//!
//! The tables named in 5.3 (`queue`, `decks`, `insert_counters`) are written as a readable projection of
//! the scheduler state; the state itself is one JSON document (`sched_state`) which is what a restart
//! loads, so the two cannot disagree about what was meant.

use crate::programming::Programming;
use crate::sched::SchedState;
use library::tracks::TrackRow;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;

pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS checkpoint (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  at_wall TEXT NOT NULL, at_wall_ms INTEGER NOT NULL,
  item_id INTEGER, queue_entry_id INTEGER, position_ms INTEGER,
  state TEXT NOT NULL, mode TEXT NOT NULL, active_playlist_id INTEGER, loudness_gain_db REAL
);
CREATE TABLE IF NOT EXISTS queue (
  id INTEGER PRIMARY KEY, pos INTEGER NOT NULL, item_id INTEGER NOT NULL, origin TEXT NOT NULL,
  block_id INTEGER, pinned INTEGER NOT NULL DEFAULT 0, committed INTEGER NOT NULL DEFAULT 0,
  transition TEXT, run_id INTEGER, continuity_from_prev INTEGER NOT NULL DEFAULT 0,
  planned_start TEXT, planned_segue TEXT, on_air INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS decks (
  key TEXT PRIMARY KEY, generation INTEGER NOT NULL, order_json TEXT NOT NULL, cursor INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS insert_counters (block_id INTEGER PRIMARY KEY, count INTEGER NOT NULL, last_fired TEXT);
CREATE TABLE IF NOT EXISTS sched_state (id INTEGER PRIMARY KEY CHECK (id = 1), json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS as_run (
  id INTEGER PRIMARY KEY,
  item_id INTEGER, title TEXT NOT NULL, artist TEXT NOT NULL, category TEXT NOT NULL,
  origin TEXT NOT NULL, block_id INTEGER, run_id INTEGER, playlist_id INTEGER,
  started_at TEXT NOT NULL, started_ms INTEGER NOT NULL, ended_at TEXT, ended_ms INTEGER, played_ms INTEGER,
  end_reason TEXT, planned_at TEXT, planned_ms INTEGER, deviation_ms INTEGER,
  flags TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS as_run_started ON as_run(started_at);
CREATE INDEX IF NOT EXISTS as_run_item ON as_run(item_id, started_at);
CREATE TABLE IF NOT EXISTS program_runs (
  id INTEGER PRIMARY KEY, playlist_id INTEGER NOT NULL, parent_run_id INTEGER, nominal_t0 TEXT,
  t0 TEXT NOT NULL, t0_source TEXT NOT NULL, cursor_entry_id INTEGER, state TEXT NOT NULL,
  started_at TEXT NOT NULL, ended_at TEXT
);
CREATE TABLE IF NOT EXISTS block_log (
  run_id INTEGER, block_id INTEGER NOT NULL, target TEXT NOT NULL, planned_at TEXT NOT NULL,
  fired_at TEXT, how TEXT NOT NULL, deviation_ms INTEGER, detail TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS block_once ON block_log(IFNULL(run_id, 0), block_id, target);
CREATE TABLE IF NOT EXISTS track_cache (
  item_id INTEGER PRIMARY KEY, revision INTEGER NOT NULL,
  store_path TEXT NOT NULL, file_bytes INTEGER NOT NULL,
  title TEXT NOT NULL, artist TEXT NOT NULL, category TEXT NOT NULL, kind TEXT NOT NULL,
  album_key TEXT NOT NULL, disc_no INTEGER, track_no INTEGER, track_total INTEGER, art_sha TEXT,
  markers TEXT NOT NULL,
  gain_inputs TEXT NOT NULL,
  row_json TEXT NOT NULL,
  k10 BLOB NOT NULL,
  k200 BLOB
);
CREATE TABLE IF NOT EXISTS programming_cache (key TEXT PRIMARY KEY, revision INTEGER NOT NULL, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS alerts (
  id INTEGER PRIMARY KEY, code TEXT NOT NULL, severity TEXT NOT NULL,
  message TEXT NOT NULL, raised_at TEXT NOT NULL, cleared_at TEXT,
  alert_key TEXT NOT NULL DEFAULT '', item_id INTEGER, raised_ms INTEGER, cleared_ms INTEGER
);
CREATE TABLE IF NOT EXISTS notify_spool (
  id INTEGER PRIMARY KEY, code TEXT NOT NULL, severity TEXT NOT NULL, message TEXT NOT NULL,
  raised_at TEXT NOT NULL, cleared_at TEXT, delivered_at TEXT
);
-- What the station itself quarantined (1.5): enforced from here until library.db shows it (pushed = 1: the
-- library has been seen holding it, so a later row without it is a release), and dropped when the item's file
-- is replaced (a new hash) or the library shows it released.
CREATE TABLE IF NOT EXISTS quarantine (
  item_id INTEGER PRIMARY KEY, reason TEXT NOT NULL, at_ms INTEGER NOT NULL, sha256 TEXT NOT NULL DEFAULT '',
  pushed INTEGER NOT NULL DEFAULT 0
);
-- What the library service must write to library.db on the station's behalf (1.5): quarantines, releases and
-- playback errors. The station never opens library.db for writing (one writer per database, and the unit
-- mounts it read-only); the library service reads this table (read-only) and applies each row once, keeping
-- its place per station and per `meta.instance` (a state.db rebuilt after corruption starts a new sequence).
CREATE TABLE IF NOT EXISTS library_outbox (
  id INTEGER PRIMARY KEY AUTOINCREMENT, op TEXT NOT NULL, item_id INTEGER NOT NULL,
  reason TEXT, n INTEGER NOT NULL DEFAULT 0, sha256 TEXT NOT NULL DEFAULT '', at_ms INTEGER NOT NULL
);
-- Per item and kind, how many times it failed on air (`silent` quarantines at 2, `decode` at 3), for one file (sha256).
CREATE TABLE IF NOT EXISTS item_errors (
  item_id INTEGER NOT NULL, kind TEXT NOT NULL, sha256 TEXT NOT NULL DEFAULT '', count INTEGER NOT NULL,
  last_ms INTEGER NOT NULL, PRIMARY KEY (item_id, kind)
);
";

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for unix milliseconds.
pub fn iso_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let frac = ms.rem_euclid(1000);
    let base = library::util::iso_utc(secs);
    format!("{}.{frac:03}Z", &base[..base.len() - 1])
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub at_wall_ms: i64,
    pub item_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    /// File position of the item on air, ms.
    pub position_ms: Option<i64>,
    /// `playing`, `held`, `stopped`, `paused`, `safe`.
    pub state: String,
    /// `auto` or `assist`.
    pub mode: String,
    pub active_playlist_id: Option<i64>,
    pub loudness_gain_db: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsRunRow {
    pub id: i64,
    pub item_id: Option<i64>,
    pub title: String,
    pub artist: String,
    pub category: String,
    pub origin: String,
    pub block_id: Option<i64>,
    pub run_id: Option<i64>,
    pub playlist_id: Option<i64>,
    pub started_ms: i64,
    pub planned_ms: Option<i64>,
    pub flags: Vec<String>,
    pub ended_ms: Option<i64>,
    pub played_ms: Option<i64>,
    pub end_reason: Option<String>,
}

pub enum Msg {
    Sched(Box<SchedState>),
    Checkpoint(Checkpoint),
    AsRunStart(AsRunRow),
    AsRunEnd {
        id: i64,
        ended_ms: i64,
        played_ms: i64,
        reason: String,
        /// A flag to add to the row (`clock_jump_ms=<Δ>`: the wall clock jumped while the row was open).
        flag: Option<String>,
    },
    /// A setting the operator changed at run time, kept in `meta` (`output_trim_db`).
    Meta {
        key: String,
        value: String,
    },
    /// An interrupted row of an earlier run of the process: closed as `crash` at start.
    CloseOpenAsRun {
        ended_ms: i64,
    },
    Tracks {
        upsert: Vec<TrackRow>,
        delete: Vec<i64>,
    },
    Programming {
        key: String,
        revision: i64,
        body: String,
    },
    /// An alert was raised, escalated or cleared (12.1: also spooled for the notifier).
    Alert {
        change: crate::alerts::AlertChange,
        alert: crate::alerts::Alert,
        at_ms: i64,
    },
    /// The station quarantined an item (1.5): kept here until the library shows it.
    Quarantine {
        item_id: i64,
        reason: String,
        sha256: String,
        at_ms: i64,
    },
    /// A quarantine no longer applies (released, or the file was replaced).
    QuarantineDrop {
        item_id: i64,
    },
    /// The library has been seen holding the station's quarantine (`pushed = 1`): from then on a row that
    /// shows the item playable is a release, also across a restart.
    QuarantineSeen {
        item_id: i64,
    },
    ItemErrors {
        item_id: i64,
        kind: String,
        sha256: String,
        count: i64,
        at_ms: i64,
    },
    ItemErrorsClear {
        item_id: i64,
    },
    /// A write the library service makes to `library.db` for the station (the item's quarantine, its error
    /// count): queued in `library_outbox`, buffered like every other write.
    Library(LibOp),
    /// Test hook: make writes fail as if the database were read-only or full.
    SetFault(FaultMode),
    /// Wait until everything before this message is on disk.
    Barrier(Sender<()>),
    /// Write the nightly snapshot of this database (5.5) on the writer's own thread: `VACUUM INTO`
    /// `<dir>/<name>-<date>.db`, checked, the result in [`WriterStatus::snapshot`]. Done at once, never
    /// buffered behind a failing database.
    Snapshot {
        dir: PathBuf,
        name: String,
        date: String,
        now_ms: i64,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LibOp {
    /// Quarantine the item, for the file with this hash (a file replaced since is not touched).
    Quarantine {
        item_id: i64,
        reason: String,
        sha256: String,
    },
    Release {
        item_id: i64,
    },
    /// `playback_errors += n`, for the file with this hash.
    AddErrors {
        item_id: i64,
        n: i64,
        sha256: String,
    },
}

/// A test hook (`StateWriter::set_fault`): what `state.db` writes do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultMode {
    None,
    /// The connection is read-only (`PRAGMA query_only`): the real `SQLITE_READONLY`.
    ReadOnly,
    /// Writes fail with `SQLITE_FULL`, as on a full disk.
    Full,
}

/// One quarantine the station holds (what a start reads back).
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantineRow {
    pub item_id: i64,
    pub reason: String,
    pub at_ms: i64,
    pub sha256: String,
    pub pushed: bool,
}

/// What the writer thread reports to the station (atomics: the station reads them every step).
#[derive(Default)]
pub struct WriterStatus {
    /// The last write attempt failed and messages are buffered.
    pub failing: std::sync::atomic::AtomicBool,
    /// 0 none, 1 unavailable (read-only, busy, I/O), 2 disk full, 3 corrupt.
    pub failure_kind: std::sync::atomic::AtomicU8,
    /// Messages waiting in memory (as-run rows, tracks and library writes).
    pub backlog: std::sync::atomic::AtomicUsize,
    /// As-run rows dropped because the buffer was full (10,000).
    pub dropped: std::sync::atomic::AtomicU64,
    /// Messages written after having been buffered (tests).
    pub recovered: std::sync::atomic::AtomicU64,
    /// The last nightly snapshot and its check (5.5), for `/health`.
    pub snapshot: std::sync::Mutex<Option<library::backup::SnapshotInfo>>,
}

pub const FAIL_NONE: u8 = 0;
pub const FAIL_UNAVAILABLE: u8 = 1;
pub const FAIL_FULL: u8 = 2;
pub const FAIL_CORRUPT: u8 = 3;

/// How many messages the writer keeps in memory while the database is unavailable (1.5: 10,000 as-run rows).
pub const BACKLOG_MAX: usize = 10_000;

/// What a start reads back.
#[derive(Debug, Default)]
pub struct Loaded {
    pub checkpoint: Option<Checkpoint>,
    pub sched: Option<SchedState>,
    pub tracks: Vec<TrackRow>,
    pub programming: Option<Programming>,
    pub programming_revision: Option<i64>,
    pub next_as_run_id: i64,
    pub next_alert_id: i64,
    /// The output trim saved by the operator (`meta.output_trim_db`), dB; it wins over the toml's.
    pub output_trim_db: Option<f64>,
    pub quarantine: Vec<QuarantineRow>,
    /// `(item_id, kind, sha256, count)`.
    pub item_errors: Vec<(i64, String, String, i64)>,
    /// The corrupt `state.db` was moved here and a fresh one started (1.5 `state_db_corrupt`).
    pub repaired_from: Option<PathBuf>,
    /// The station's default transition preset as set at runtime (`meta.transition_preset`), if any.
    pub station_preset: Option<String>,
    /// The config's `transition_preset` at the time that override was made: the override only applies while
    /// the config still says this (an edit of the file while the station was stopped wins over it).
    pub station_preset_base: Option<String>,
}

/// The `meta` key of the runtime station default transition preset.
pub const META_TRANSITION_PRESET: &str = "transition_preset";
pub const META_TRANSITION_PRESET_BASE: &str = "transition_preset_base";
/// Bring a `state.db` written by an older build up to [`SCHEMA_VERSION`]. Schema 2 added
/// `track_cache.k200` (the `fm` exit reads it); the column is added only when it is missing, so this is safe to
/// run on every open, and the rows already cached keep everything else. They carry no `K200` until the
/// library sync next reads them (the catalog compares it, so every such row is reported as changed once and
/// rewritten); a station that cannot reach `library.db` meanwhile plays as it did before (`fm` falls back to
/// the cue-out). The queue, decks and checkpoint are untouched, so the station resumes where it was.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let has_k200 = conn
        .prepare("SELECT 1 FROM pragma_table_info('track_cache') WHERE name = 'k200'")?
        .exists([])?;
    if !has_k200 {
        conn.execute_batch("ALTER TABLE track_cache ADD COLUMN k200 BLOB")?;
    }
    conn.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'schema_version' AND CAST(value AS INTEGER) < ?1",
        params![SCHEMA_VERSION],
    )?;
    Ok(())
}

fn open_conn(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    // Which state.db this is (the library service's place in `library_outbox` belongs to one file).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES('instance', ?1)",
        params![format!("{stamp:x}-{}", std::process::id())],
    )?;
    Ok(conn)
}

/// Read everything a start needs. Creates the database if there is none.
///
/// **Repair path** (1.7): a `state.db` SQLite reports as corrupt or as not a database is moved aside
/// (`state.db.corrupt-<unix ms>`, with its `-wal` and `-shm`) and the station starts from a fresh
/// one: it plays from the library, and only the saved queue, decks and counters are lost. Any other
/// error (a lock, a permission) is returned, and nothing is moved.
pub fn load(path: &Path) -> Result<Loaded, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    match load_from(path) {
        Err(e) if is_corrupt(&e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let mut aside = path.as_os_str().to_owned();
            aside.push(format!(".corrupt-{stamp}"));
            let aside = PathBuf::from(aside);
            tracing::error!(
                "{} is corrupt ({e}); moved to {} and starting from a fresh state.db",
                path.display(),
                aside.display()
            );
            std::fs::rename(path, &aside).map_err(|e| format!("{}: {e}", path.display()))?;
            for ext in ["-wal", "-shm"] {
                let mut side = path.as_os_str().to_owned();
                side.push(ext);
                let mut to = aside.as_os_str().to_owned();
                to.push(ext);
                let _ = std::fs::rename(PathBuf::from(side), PathBuf::from(to));
            }
            let mut l = load_from(path).map_err(|e| format!("{}: {e}", path.display()))?;
            l.repaired_from = Some(aside);
            Ok(l)
        }
        r => r.map_err(|e| format!("{}: {e}", path.display())),
    }
}

fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

fn load_from(path: &Path) -> rusqlite::Result<Loaded> {
    let conn = open_conn(path)?;
    let mut out = Loaded::default();
    #[allow(clippy::field_reassign_with_default)]
    {
        out.checkpoint = conn
        .query_row(
            "SELECT at_wall_ms, item_id, queue_entry_id, position_ms, state, mode, active_playlist_id, loudness_gain_db
             FROM checkpoint WHERE id = 1",
            [],
            |r| {
                Ok(Checkpoint {
                    at_wall_ms: r.get(0)?,
                    item_id: r.get(1)?,
                    queue_entry_id: r.get(2)?,
                    position_ms: r.get(3)?,
                    state: r.get(4)?,
                    mode: r.get(5)?,
                    active_playlist_id: r.get(6)?,
                    loudness_gain_db: r.get(7)?,
                })
            },
        )
        .optional()
        ?;
    }
    let json: Option<String> = conn
        .query_row("SELECT json FROM sched_state WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    out.sched = json.and_then(|j| match serde_json::from_str(&j) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(
                "the saved scheduler state cannot be read ({e}); starting a fresh queue"
            );
            None
        }
    });
    let mut st = conn.prepare("SELECT row_json, k10, k200 FROM track_cache ORDER BY item_id")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    for r in rows {
        let (j, k10, k200) = r?;
        if let Ok(mut t) = serde_json::from_str::<TrackRow>(&j) {
            t.k10 = k10;
            t.k200 = k200.unwrap_or_default();
            out.tracks.push(t);
        }
    }
    let prog: Option<(i64, String)> = conn
        .query_row(
            "SELECT revision, body FROM programming_cache WHERE key = 'programming'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((rev, body)) = prog {
        out.programming = serde_json::from_str(&body).ok();
        out.programming_revision = Some(rev);
    }
    out.next_as_run_id =
        conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM as_run", [], |r| {
            r.get(0)
        })?;
    out.station_preset = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_TRANSITION_PRESET],
            |r| r.get(0),
        )
        .optional()?;
    out.station_preset_base = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![META_TRANSITION_PRESET_BASE],
            |r| r.get(0),
        )
        .optional()?;
    out.next_alert_id = conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM alerts", [], |r| {
        r.get(0)
    })?;
    out.output_trim_db = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'output_trim_db'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|v| v.parse().ok());
    let mut st = conn.prepare(
        "SELECT item_id, reason, at_ms, sha256, pushed FROM quarantine ORDER BY item_id",
    )?;
    out.quarantine = st
        .query_map([], |r| {
            Ok(QuarantineRow {
                item_id: r.get(0)?,
                reason: r.get(1)?,
                at_ms: r.get(2)?,
                sha256: r.get(3)?,
                pushed: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    let mut st = conn
        .prepare("SELECT item_id, kind, sha256, count FROM item_errors ORDER BY item_id, kind")?;
    out.item_errors = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// Take the station's single-writer lock (`state.db.lock`, an advisory lock the kernel releases when
/// the process ends, however it ends). A second process for the same station would interleave its
/// queue, checkpoint and as-run writes with the first one's: it is refused at start instead.
pub fn lock_station(path: &Path) -> Result<std::fs::File, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut lp = path.as_os_str().to_owned();
    lp.push(".lock");
    let lp = PathBuf::from(lp);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lp)
        .map_err(|e| format!("{}: {e}", lp.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "another process is running this station ({} is locked)",
            lp.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", lp.display())),
    }
}

/// The saved scheduler state, read without touching the schema (a running station's `state.db` can be
/// read from outside; tests do).
pub fn read_sched_readonly(path: &Path) -> Option<SchedState> {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let json: String = conn
        .query_row("SELECT json FROM sched_state WHERE id = 1", [], |r| {
            r.get(0)
        })
        .ok()?;
    serde_json::from_str(&json).ok()
}

/// The checkpoint, read the same way.
pub fn read_checkpoint_readonly(path: &Path) -> Option<Checkpoint> {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT at_wall_ms, item_id, queue_entry_id, position_ms, state, mode, active_playlist_id, loudness_gain_db
         FROM checkpoint WHERE id = 1",
        [],
        |r| {
            Ok(Checkpoint {
                at_wall_ms: r.get(0)?,
                item_id: r.get(1)?,
                queue_entry_id: r.get(2)?,
                position_ms: r.get(3)?,
                state: r.get(4)?,
                mode: r.get(5)?,
                active_playlist_id: r.get(6)?,
                loudness_gain_db: r.get(7)?,
            })
        },
    )
    .ok()
}

/// The as-run rows of a state database, oldest first (tests and the history view).
pub fn read_as_run(path: &Path) -> Result<Vec<AsRunRow>, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare(
            "SELECT id, item_id, title, artist, category, origin, block_id, run_id, playlist_id, started_ms,
                    planned_ms, flags, ended_ms, played_ms, end_reason FROM as_run ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            let flags: String = r.get(11)?;
            Ok(AsRunRow {
                id: r.get(0)?,
                item_id: r.get(1)?,
                title: r.get(2)?,
                artist: r.get(3)?,
                category: r.get(4)?,
                origin: r.get(5)?,
                block_id: r.get(6)?,
                run_id: r.get(7)?,
                playlist_id: r.get(8)?,
                started_ms: r.get(9)?,
                planned_ms: r.get(10)?,
                flags: serde_json::from_str(&flags).unwrap_or_default(),
                ended_ms: r.get(12)?,
                played_ms: r.get(13)?,
                end_reason: r.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// As-run rows whose start lies in `[from_ms, to_ms]`, newest first, at most `limit` (the history endpoints).
/// The block log rows whose target is in `[from_ms, to_ms]`, oldest first (`GET /blocks/log`).
pub fn read_block_log(
    path: &Path,
    from_ms: i64,
    to_ms: i64,
    limit: usize,
) -> Result<Vec<crate::timed::LogRow>, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare(
            "SELECT run_id, block_id, how, deviation_ms, detail FROM block_log
             ORDER BY rowid DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows: Vec<crate::timed::LogRow> = st
        .query_map([(limit.max(1) * 4) as i64], |r| {
            let detail: String = r.get(4)?;
            let d: serde_json::Value = serde_json::from_str(&detail).unwrap_or_default();
            Ok(crate::timed::LogRow {
                run_id: r.get(0)?,
                block_id: r.get(1)?,
                name: d["name"].as_str().unwrap_or("").into(),
                kind: d["kind"].as_str().unwrap_or("").into(),
                target_ms: d["target_ms"].as_i64().unwrap_or(0),
                planned_ms: d["planned_ms"].as_i64().unwrap_or(0),
                fired_ms: d["fired_ms"].as_i64(),
                how: r.get(2)?,
                deviation_ms: r.get(3)?,
                detail: d["detail"].as_str().unwrap_or("").into(),
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e: rusqlite::Error| e.to_string())?;
    let mut out: Vec<_> = rows
        .into_iter()
        // A block fired by hand has its own firing time, which may be long before its target.
        .filter(|r| {
            (r.target_ms >= from_ms && r.target_ms <= to_ms)
                || r.fired_ms.is_some_and(|f| f >= from_ms && f <= to_ms)
        })
        .take(limit)
        .collect();
    out.sort_by_key(|r| (r.target_ms, r.block_id));
    Ok(out)
}

pub fn read_as_run_range(
    path: &Path,
    from_ms: i64,
    to_ms: i64,
    limit: usize,
) -> Result<Vec<AsRunRow>, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare(
            "SELECT id, item_id, title, artist, category, origin, block_id, run_id, playlist_id, started_ms,
                    planned_ms, flags, ended_ms, played_ms, end_reason FROM as_run
             WHERE started_ms >= ?1 AND started_ms <= ?2 ORDER BY started_ms DESC, id DESC LIMIT ?3",
        )
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map(rusqlite::params![from_ms, to_ms, limit as i64], |r| {
            let flags: String = r.get(11)?;
            Ok(AsRunRow {
                id: r.get(0)?,
                item_id: r.get(1)?,
                title: r.get(2)?,
                artist: r.get(3)?,
                category: r.get(4)?,
                origin: r.get(5)?,
                block_id: r.get(6)?,
                run_id: r.get(7)?,
                playlist_id: r.get(8)?,
                started_ms: r.get(9)?,
                planned_ms: r.get(10)?,
                flags: serde_json::from_str(&flags).unwrap_or_default(),
                ended_ms: r.get(12)?,
                played_ms: r.get(13)?,
                end_reason: r.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// The handle playout code holds: sends never block and never fail loudly.
///
/// **When the database is unavailable** (read-only, locked past its timeout, an I/O error, a full disk)
/// the writer buffers in memory and retries with backoff (1.5): as-run rows and tracks up to
/// [`BACKLOG_MAX`], the newest checkpoint and scheduler state, and every message goes in when the
/// database is writable again. Playout never waits on any of it. [`WriterStatus`] tells the station
/// what is happening (for the `db_unavailable`, `disk_full` and `state_db_corrupt` alerts).
pub struct StateWriter {
    tx: Sender<Msg>,
    join: Option<JoinHandle<()>>,
    pub path: PathBuf,
    pub status: std::sync::Arc<WriterStatus>,
}

fn classify(e: &rusqlite::Error) -> u8 {
    use rusqlite::ErrorCode::*;
    match e {
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            DiskFull => FAIL_FULL,
            DatabaseCorrupt | NotADatabase => FAIL_CORRUPT,
            _ => FAIL_UNAVAILABLE,
        },
        _ => FAIL_UNAVAILABLE,
    }
}

struct Writer {
    conn: Connection,
    fifo: std::collections::VecDeque<Msg>,
    latest_sched: Option<Msg>,
    latest_ckpt: Option<Msg>,
    fault: FaultMode,
    status: std::sync::Arc<WriterStatus>,
    next_retry: std::time::Instant,
    backoff: std::time::Duration,
    was_failing: bool,
    buffered_since_failure: u64,
}

impl Writer {
    fn pending(&self) -> usize {
        self.fifo.len()
            + usize::from(self.latest_sched.is_some())
            + usize::from(self.latest_ckpt.is_some())
    }

    fn publish(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.status.backlog.store(self.pending(), Relaxed);
    }

    fn set_fault(&mut self, mode: FaultMode) {
        self.fault = mode;
        let _ = self
            .conn
            .pragma_update(None, "query_only", mode == FaultMode::ReadOnly);
    }

    fn snapshot(&self, dir: &Path, name: &str, date: &str, now_ms: i64) {
        let Some(db) = self.conn.path().map(PathBuf::from) else {
            return;
        };
        let info = match library::backup::snapshot(&db, dir, name, date, now_ms) {
            Ok(i) => i,
            Err(e) => library::backup::SnapshotInfo {
                taken_ms: now_ms,
                date: date.into(),
                check: format!("snapshot failed: {e}"),
                file: String::new(),
                bytes: 0,
            },
        };
        if info.ok() {
            tracing::info!("snapshot {}: {} bytes, integrity ok", info.file, info.bytes);
        } else {
            tracing::error!("snapshot of {name} failed its check: {}", info.check);
        }
        if let Ok(mut g) = self.status.snapshot.lock() {
            *g = Some(info);
        }
    }

    fn write_one(&self, m: &Msg) -> rusqlite::Result<()> {
        if self.fault == FaultMode::Full {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("database or disk is full (injected)".into()),
            ));
        }
        write(&self.conn, m)
    }

    /// Write what is buffered, oldest first. Stops at the first failure and schedules a retry.
    fn flush(&mut self, force: bool) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.pending() == 0 {
            return;
        }
        if !force && std::time::Instant::now() < self.next_retry {
            return;
        }
        let mut written = 0u64;
        loop {
            let res = if let Some(m) = self.fifo.front() {
                self.write_one(m).map(|()| 0)
            } else if let Some(m) = self.latest_sched.as_ref() {
                self.write_one(m).map(|()| 1)
            } else if let Some(m) = self.latest_ckpt.as_ref() {
                self.write_one(m).map(|()| 2)
            } else {
                break;
            };
            match res {
                Ok(which) => {
                    written += 1;
                    match which {
                        0 => {
                            self.fifo.pop_front();
                        }
                        1 => self.latest_sched = None,
                        _ => self.latest_ckpt = None,
                    }
                }
                Err(e) => {
                    let kind = classify(&e);
                    if !self.was_failing {
                        tracing::error!(
                            "state.db write failed ({e}); buffering until it works again"
                        );
                    }
                    self.was_failing = true;
                    self.status.failing.store(true, Relaxed);
                    self.status.failure_kind.store(kind, Relaxed);
                    self.next_retry = std::time::Instant::now() + self.backoff;
                    self.backoff = (self.backoff * 2).min(std::time::Duration::from_secs(2));
                    self.publish();
                    return;
                }
            }
        }
        if self.was_failing {
            tracing::info!("state.db is writable again; {written} buffered writes done");
            self.status.recovered.fetch_add(written, Relaxed);
            self.was_failing = false;
        }
        self.status.failing.store(false, Relaxed);
        self.status.failure_kind.store(FAIL_NONE, Relaxed);
        self.backoff = std::time::Duration::from_millis(200);
        self.publish();
    }
}

impl StateWriter {
    pub fn start(path: &Path) -> Result<StateWriter, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let conn = open_conn(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (tx, rx) = channel::<Msg>();
        let status = std::sync::Arc::new(WriterStatus::default());
        let mut w = Writer {
            conn,
            fifo: Default::default(),
            latest_sched: None,
            latest_ckpt: None,
            fault: FaultMode::None,
            status: status.clone(),
            next_retry: std::time::Instant::now(),
            backoff: std::time::Duration::from_millis(200),
            was_failing: false,
            buffered_since_failure: 0,
        };
        let join = std::thread::Builder::new()
            .name("db-writer".into())
            .spawn(move || {
                use std::sync::mpsc::RecvTimeoutError;
                loop {
                    let waiting = w.pending() > 0;
                    let got = if waiting {
                        rx.recv_timeout(std::time::Duration::from_millis(100))
                    } else {
                        rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match got {
                        Ok(Msg::Stop) => break,
                        Ok(Msg::Barrier(done)) => {
                            w.flush(true);
                            let _ = done.send(());
                        }
                        Ok(Msg::SetFault(mode)) => {
                            w.set_fault(mode);
                            w.flush(true);
                        }
                        Ok(Msg::Snapshot {
                            dir,
                            name,
                            date,
                            now_ms,
                        }) => w.snapshot(&dir, &name, &date, now_ms),
                        Ok(m @ Msg::Sched(_)) => w.latest_sched = Some(m),
                        Ok(m @ Msg::Checkpoint(_)) => w.latest_ckpt = Some(m),
                        Ok(m) => {
                            if w.fifo.len() >= BACKLOG_MAX {
                                // Oldest first: an as-run row of an hour ago matters less than now. The
                                // messages the station needs back after a restart (its quarantines and error
                                // counts, the library outbox, the track and programming caches) are kept as long
                                // as there is a log row or an alert to drop instead.
                                let expendable = w.fifo.iter().position(|m| {
                                    matches!(
                                        m,
                                        Msg::AsRunStart(_)
                                            | Msg::AsRunEnd { .. }
                                            | Msg::Alert { .. }
                                    )
                                });
                                match expendable {
                                    Some(i) => {
                                        w.fifo.remove(i);
                                    }
                                    None => {
                                        w.fifo.pop_front();
                                    }
                                }
                                w.status
                                    .dropped
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            w.fifo.push_back(m);
                            w.buffered_since_failure += 1;
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    w.flush(false);
                    w.publish();
                }
                // Last try at what is buffered.
                w.flush(true);
            })
            .map_err(|e| e.to_string())?;
        Ok(StateWriter {
            tx,
            join: Some(join),
            path: path.to_path_buf(),
            status,
        })
    }

    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
    }

    /// A test hook: make `state.db` writes fail as if the file were read-only or the disk full.
    pub fn set_fault(&self, mode: FaultMode) {
        self.send(Msg::SetFault(mode));
    }

    /// Block until everything sent so far has been tried (and, when the database works, written).
    /// It does not wait for a database that is failing: whatever cannot be written stays buffered.
    pub fn flush(&self) {
        let (tx, rx) = channel();
        if self.tx.send(Msg::Barrier(tx)).is_ok() {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
        }
    }
}

impl Drop for StateWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn write(conn: &Connection, m: &Msg) -> rusqlite::Result<()> {
    match m {
        Msg::Sched(s) => {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO sched_state(id, json) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET json = ?1",
                params![serde_json::to_string(&**s).unwrap_or_default()],
            )?;
            tx.execute("DELETE FROM queue", [])?;
            let rows = s.on_air.iter().map(|e| (e, true)).chain(s.queue.iter().map(|e| (e, false)));
            for (pos, (e, on_air)) in rows.enumerate() {
                tx.execute(
                    "INSERT INTO queue(id, pos, item_id, origin, block_id, pinned, committed, transition, run_id,
                        continuity_from_prev, planned_start, planned_segue, on_air)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![
                        e.id,
                        pos as i64,
                        e.item_id,
                        e.origin.as_str(),
                        e.block_id,
                        e.pinned,
                        e.committed,
                        e.transition,
                        e.run_id,
                        e.continuity_from_prev,
                        e.planned_start_ms.map(iso_ms),
                        e.planned_segue_ms.map(iso_ms),
                        on_air
                    ],
                )?;
            }
            // The block log (5.3): one row per block and target; a row is written once.
            {
                let mut ins = tx.prepare_cached(
                    "INSERT OR IGNORE INTO block_log(run_id, block_id, target, planned_at, fired_at, how, deviation_ms, detail)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                )?;
                for r in &s.log.rows {
                    let detail = serde_json::json!({
                        "name": r.name, "kind": r.kind, "target_ms": r.target_ms,
                        "planned_ms": r.planned_ms, "fired_ms": r.fired_ms, "detail": r.detail,
                    });
                    ins.execute(params![
                        r.run_id,
                        r.block_id,
                        iso_ms(r.target_ms),
                        iso_ms(r.planned_ms),
                        r.fired_ms.map(iso_ms),
                        r.how,
                        r.deviation_ms,
                        detail.to_string()
                    ])?;
                }
            }
            tx.execute("DELETE FROM decks", [])?;
            for (k, d) in &s.decks {
                let cur = d.passes.last();
                tx.execute(
                    "INSERT INTO decks(key, generation, order_json, cursor) VALUES(?1,?2,?3,?4)",
                    params![
                        k,
                        cur.map_or(0, |p| p.gen as i64),
                        serde_json::to_string(&cur.map(|p| &p.order)).unwrap_or_default(),
                        cur.map_or(0, |p| p.cursor as i64)
                    ],
                )?;
            }
            tx.execute("DELETE FROM insert_counters", [])?;
            for (b, c) in &s.counters {
                tx.execute(
                    "INSERT INTO insert_counters(block_id, count, last_fired) VALUES(?1,?2,?3)",
                    params![b, c.count, c.last_fired_ms.map(iso_ms)],
                )?;
            }
            tx.commit()
        }
        Msg::Checkpoint(c) => conn
            .execute(
                "INSERT INTO checkpoint(id, at_wall, at_wall_ms, item_id, queue_entry_id, position_ms, state, mode,
                    active_playlist_id, loudness_gain_db)
                 VALUES(1,?1,?2,?3,?4,?5,?6,?7,?8,?9)
                 ON CONFLICT(id) DO UPDATE SET at_wall=?1, at_wall_ms=?2, item_id=?3, queue_entry_id=?4,
                    position_ms=?5, state=?6, mode=?7, active_playlist_id=?8, loudness_gain_db=?9",
                params![
                    iso_ms(c.at_wall_ms),
                    c.at_wall_ms,
                    c.item_id,
                    c.queue_entry_id,
                    c.position_ms,
                    c.state,
                    c.mode,
                    c.active_playlist_id,
                    c.loudness_gain_db
                ],
            )
            .map(|_| ()),
        Msg::AsRunStart(r) => conn
            .execute(
                "INSERT OR REPLACE INTO as_run(id, item_id, title, artist, category, origin, block_id, run_id,
                    playlist_id, started_at, started_ms, planned_at, planned_ms, flags)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
                params![
                    r.id,
                    r.item_id,
                    r.title,
                    r.artist,
                    r.category,
                    r.origin,
                    r.block_id,
                    r.run_id,
                    r.playlist_id,
                    iso_ms(r.started_ms),
                    r.started_ms,
                    r.planned_ms.map(iso_ms),
                    r.planned_ms,
                    serde_json::to_string(&r.flags).unwrap_or_else(|_| "[]".into())
                ],
            )
            .map(|_| ()),
        Msg::AsRunEnd {
            id,
            ended_ms,
            played_ms,
            reason,
            flag,
        } => {
            conn.execute(
                "UPDATE as_run SET ended_at = ?2, ended_ms = ?5, played_ms = ?3, end_reason = ?4 WHERE id = ?1",
                params![id, iso_ms(*ended_ms), played_ms, reason, ended_ms],
            )?;
            if let Some(flag) = flag {
                let old: String = conn.query_row(
                    "SELECT flags FROM as_run WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )?;
                let mut v: Vec<String> = serde_json::from_str(&old).unwrap_or_default();
                v.push(flag.clone());
                conn.execute(
                    "UPDATE as_run SET flags = ?2 WHERE id = ?1",
                    params![id, serde_json::to_string(&v).unwrap_or_else(|_| "[]".into())],
                )?;
            }
            Ok(())
        }
        Msg::Meta { key, value } => conn
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ()),
        Msg::CloseOpenAsRun { ended_ms } => conn
            .execute(
                "UPDATE as_run SET ended_at = ?1, ended_ms = ?2, end_reason = 'crash' WHERE ended_at IS NULL",
                params![iso_ms(*ended_ms), ended_ms],
            )
            .map(|_| ()),
        Msg::Tracks { upsert, delete } => {
            let tx = conn.unchecked_transaction()?;
            for id in delete {
                tx.execute("DELETE FROM track_cache WHERE item_id = ?1", params![id])?;
            }
            for t in upsert {
                let markers = serde_json::json!({
                    "cue_in": t.cue_in, "attack": t.attack, "intro": t.intro, "segue": t.segue,
                    "fade_out": t.fade_out, "cue_out": t.cue_out, "flags": t.flags,
                })
                .to_string();
                let gain = serde_json::json!({
                    "l_int": t.l_used, "true_peak": t.true_peak, "gain_offset_db": t.gain_offset_db,
                    "gain_trim_db": t.gain_trim_db,
                })
                .to_string();
                tx.execute(
                    "INSERT OR REPLACE INTO track_cache(item_id, revision, store_path, file_bytes, title, artist,
                        category, kind, album_key, disc_no, track_no, track_total, art_sha, markers, gain_inputs,
                        row_json, k10, k200)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    params![
                        t.item_id,
                        t.revision,
                        t.store_path,
                        t.file_bytes,
                        t.title,
                        t.artist,
                        t.category,
                        t.kind,
                        t.album_key,
                        t.disc_no,
                        t.track_no,
                        t.track_total,
                        t.art_sha,
                        markers,
                        gain,
                        serde_json::to_string(&t).unwrap_or_default(),
                        t.k10,
                        t.k200
                    ],
                )?;
            }
            tx.commit()
        }
        Msg::Programming {
            key,
            revision,
            body,
        } => conn
            .execute(
                "INSERT INTO programming_cache(key, revision, body) VALUES(?1,?2,?3)
                 ON CONFLICT(key) DO UPDATE SET revision = ?2, body = ?3",
                params![key, revision, body],
            )
            .map(|_| ()),
        Msg::Alert {
            change,
            alert,
            at_ms,
        } => {
            use crate::alerts::AlertChange::*;
            match change {
                Raised => {
                    let tx = conn.unchecked_transaction()?;
                    tx.execute(
                        "INSERT OR REPLACE INTO alerts(id, code, severity, message, raised_at, alert_key, item_id, raised_ms)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![
                            alert.id,
                            alert.code,
                            alert.severity.name(),
                            alert.message,
                            iso_ms(alert.raised_ms),
                            alert.key,
                            alert.item_id,
                            alert.raised_ms
                        ],
                    )?;
                    tx.execute(
                        "INSERT OR REPLACE INTO notify_spool(id, code, severity, message, raised_at)
                         VALUES(?1,?2,?3,?4,?5)",
                        params![
                            alert.id,
                            alert.code,
                            alert.severity.name(),
                            alert.message,
                            iso_ms(alert.raised_ms)
                        ],
                    )?;
                    tx.commit()
                }
                Escalated => {
                    let tx = conn.unchecked_transaction()?;
                    tx.execute(
                        "UPDATE alerts SET severity = ?2 WHERE id = ?1",
                        params![alert.id, alert.severity.name()],
                    )?;
                    tx.execute(
                        "UPDATE notify_spool SET severity = ?2 WHERE id = ?1",
                        params![alert.id, alert.severity.name()],
                    )?;
                    tx.commit()
                }
                Cleared => {
                    let tx = conn.unchecked_transaction()?;
                    tx.execute(
                        "UPDATE alerts SET cleared_at = ?2, cleared_ms = ?3 WHERE id = ?1",
                        params![alert.id, iso_ms(*at_ms), at_ms],
                    )?;
                    tx.execute(
                        "UPDATE notify_spool SET cleared_at = ?2 WHERE id = ?1",
                        params![alert.id, iso_ms(*at_ms)],
                    )?;
                    tx.commit()
                }
            }
        }
        Msg::Quarantine {
            item_id,
            reason,
            sha256,
            at_ms,
        } => conn
            .execute(
                "INSERT INTO quarantine(item_id, reason, at_ms, sha256, pushed) VALUES(?1,?2,?3,?4,0)
                 ON CONFLICT(item_id) DO UPDATE SET reason=?2, at_ms=?3, sha256=?4, pushed=0",
                params![item_id, reason, at_ms, sha256],
            )
            .map(|_| ()),
        Msg::QuarantineDrop { item_id } => conn
            .execute("DELETE FROM quarantine WHERE item_id = ?1", params![item_id])
            .map(|_| ()),
        Msg::QuarantineSeen { item_id } => conn
            .execute(
                "UPDATE quarantine SET pushed = 1 WHERE item_id = ?1",
                params![item_id],
            )
            .map(|_| ()),
        Msg::ItemErrors {
            item_id,
            kind,
            sha256,
            count,
            at_ms,
        } => conn
            .execute(
                "INSERT INTO item_errors(item_id, kind, sha256, count, last_ms) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(item_id, kind) DO UPDATE SET sha256=?3, count=?4, last_ms=?5",
                params![item_id, kind, sha256, count, at_ms],
            )
            .map(|_| ()),
        Msg::ItemErrorsClear { item_id } => conn
            .execute("DELETE FROM item_errors WHERE item_id = ?1", params![item_id])
            .map(|_| ()),
        Msg::Library(op) => {
            let (kind, item_id, reason, n, sha) = match op {
                LibOp::Quarantine {
                    item_id,
                    reason,
                    sha256,
                } => ("quarantine", *item_id, Some(reason.as_str()), 0, sha256.as_str()),
                LibOp::Release { item_id } => ("release", *item_id, None, 0, ""),
                LibOp::AddErrors {
                    item_id,
                    n,
                    sha256,
                } => ("add_errors", *item_id, None, *n, sha256.as_str()),
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            conn.execute(
                "INSERT INTO library_outbox(op, item_id, reason, n, sha256, at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
                params![kind, item_id, reason, n, sha, now],
            )?;
            Ok(())
        }
        Msg::SetFault(_) | Msg::Barrier(_) | Msg::Snapshot { .. } | Msg::Stop => Ok(()),
    }
}

/// One row of `alerts`: `(id, code, severity, message, raised_ms, cleared_ms)`.
pub type AlertRow = (i64, String, String, String, i64, Option<i64>);

/// The alerts a state database holds, oldest first.
pub fn read_alerts(path: &Path) -> Result<Vec<AlertRow>, String> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mut st = conn
        .prepare("SELECT id, code, severity, message, COALESCE(raised_ms, 0), cleared_ms FROM alerts ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::testing::row;
    use crate::sched::{SchedConfig, Scheduler};
    use std::sync::Arc;

    fn tmp(name: &str) -> PathBuf {
        let d =
            fixtures::testdir::process_dir().join(format!("state-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("state.db")
    }

    #[test]
    fn iso_with_milliseconds() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(1_790_000_000_123), "2026-09-21T14:13:20.123Z");
    }

    #[test]
    fn everything_written_is_read_back() {
        let path = tmp("roundtrip");
        let w = StateWriter::start(&path).unwrap();
        let mut sched = Scheduler::new(SchedConfig::default(), Arc::new(Programming::default()), 5);
        sched.st.next_entry_id = 42;
        w.send(Msg::Sched(Box::new(sched.snapshot())));
        let mut r = row(7, "Song", "Artist", "music", "music", 240_000);
        r.k10 = vec![1, 2, 3, 4];
        r.k200 = vec![5, 6, 7];
        w.send(Msg::Tracks {
            upsert: vec![r.clone()],
            delete: vec![],
        });
        w.send(Msg::Checkpoint(Checkpoint {
            at_wall_ms: 1_790_000_000_000,
            item_id: Some(7),
            queue_entry_id: Some(3),
            position_ms: Some(12_345),
            state: "playing".into(),
            mode: "auto".into(),
            active_playlist_id: Some(1),
            loudness_gain_db: None,
        }));
        w.send(Msg::AsRunStart(AsRunRow {
            id: 1,
            item_id: Some(7),
            title: "Song".into(),
            artist: "Artist".into(),
            category: "music".into(),
            origin: "rotation".into(),
            block_id: None,
            run_id: None,
            playlist_id: Some(1),
            started_ms: 1_790_000_000_000,
            planned_ms: None,
            flags: vec!["resumed".into()],
            ended_ms: None,
            played_ms: None,
            end_reason: None,
        }));
        w.flush();
        let l = load(&path).unwrap();
        assert_eq!(l.sched.unwrap().next_entry_id, 42);
        assert_eq!(l.tracks, vec![r]);
        assert_eq!(l.tracks[0].k10, vec![1, 2, 3, 4]);
        assert_eq!(l.tracks[0].k200, vec![5, 6, 7]);
        assert_eq!(l.checkpoint.unwrap().position_ms, Some(12_345));
        assert_eq!(l.next_as_run_id, 2);
        w.send(Msg::CloseOpenAsRun { ended_ms: 5 });
        w.flush();
        let rows = read_as_run(&path).unwrap();
        assert_eq!(rows[0].end_reason.as_deref(), Some("crash"));
        assert_eq!(rows[0].flags, vec!["resumed".to_string()]);
        // An end that carries a flag appends it to the row's flags.
        w.send(Msg::AsRunEnd {
            id: rows[0].id,
            ended_ms: 99,
            played_ms: 7,
            reason: "end".into(),
            flag: Some("clock_jump_ms=600000".into()),
        });
        w.flush();
        let rows = read_as_run(&path).unwrap();
        assert_eq!(
            rows[0].flags,
            vec!["resumed".to_string(), "clock_jump_ms=600000".to_string()]
        );
    }

    #[test]
    fn the_output_trim_is_kept_and_read_back() {
        let path = tmp("trim");
        let w = StateWriter::start(&path).unwrap();
        assert_eq!(load(&path).unwrap().output_trim_db, None);
        w.send(Msg::Meta {
            key: "output_trim_db".into(),
            value: "-6.5".into(),
        });
        w.send(Msg::Meta {
            key: "output_trim_db".into(),
            value: "-3".into(),
        });
        w.flush();
        assert_eq!(load(&path).unwrap().output_trim_db, Some(-3.0));
    }

    /// A `state.db` written before schema 2 (no `track_cache.k200`) opens, keeps its queue, checkpoint and
    /// cached rows, gains the column, and takes new rows with `K200`.
    #[test]
    fn a_schema_1_state_db_is_upgraded_and_resumes() {
        let path = tmp("upgrade");
        let w = StateWriter::start(&path).unwrap();
        let mut sched = Scheduler::new(SchedConfig::default(), Arc::new(Programming::default()), 5);
        sched.st.next_entry_id = 77;
        w.send(Msg::Sched(Box::new(sched.snapshot())));
        let mut r = row(9, "Song", "Artist", "music", "music", 240_000);
        r.k10 = vec![9, 9];
        w.send(Msg::Tracks {
            upsert: vec![r.clone()],
            delete: vec![],
        });
        w.send(Msg::Checkpoint(Checkpoint {
            at_wall_ms: 1_790_000_000_000,
            item_id: Some(9),
            queue_entry_id: Some(4),
            position_ms: Some(31_000),
            state: "playing".into(),
            mode: "auto".into(),
            active_playlist_id: Some(1),
            loudness_gain_db: None,
        }));
        w.flush();
        drop(w);
        // Make it a schema-1 file: no k200 column, version 1.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "ALTER TABLE track_cache DROP COLUMN k200;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        }
        let l = load(&path).expect("a schema-1 file opens");
        assert_eq!(l.sched.unwrap().next_entry_id, 77);
        let cp = l.checkpoint.unwrap();
        assert_eq!((cp.item_id, cp.position_ms), (Some(9), Some(31_000)));
        assert_eq!(l.tracks.len(), 1);
        assert_eq!(l.tracks[0], r);
        assert_eq!(l.tracks[0].k10, vec![9, 9]);
        assert!(l.tracks[0].k200.is_empty());
        let c = Connection::open(&path).unwrap();
        let ver: String = c
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ver, SCHEMA_VERSION.to_string());
        drop(c);
        // New rows carry K200 and a second open is a no-op.
        let w = StateWriter::start(&path).unwrap();
        let mut r2 = r.clone();
        r2.k200 = vec![1, 2, 3];
        w.send(Msg::Tracks {
            upsert: vec![r2],
            delete: vec![],
        });
        w.flush();
        drop(w);
        let l = load(&path).unwrap();
        assert_eq!(l.tracks[0].k200, vec![1, 2, 3]);
        assert_eq!(l.sched.unwrap().next_entry_id, 77);
    }

    #[test]
    fn a_corrupt_state_db_is_moved_aside_and_a_fresh_one_starts() {
        let path = tmp("corrupt");
        std::fs::write(&path, vec![0x5Au8; 64 * 1024]).unwrap();
        let l = load(&path).expect("the repair path starts a fresh state.db");
        assert!(l.sched.is_none() && l.checkpoint.is_none());
        let dir = path.parent().unwrap();
        let aside: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("state.db.corrupt-")
            })
            .collect();
        println!(
            "moved aside: {:?}",
            aside.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
        assert_eq!(aside.len(), 1);
        // The fresh database works.
        let w = StateWriter::start(&path).unwrap();
        w.send(Msg::CloseOpenAsRun { ended_ms: 1 });
        w.flush();
    }

    #[test]
    fn a_second_process_for_the_same_station_is_refused() {
        let path = tmp("lock");
        let first = lock_station(&path).unwrap();
        let err = lock_station(&path).unwrap_err();
        println!("second lock: {err}");
        assert!(err.contains("another process"));
        drop(first);
        lock_station(&path).expect("free again once the first is gone");
    }

    #[test]
    fn writes_are_buffered_while_the_database_is_unavailable_and_land_in_order_afterwards() {
        use std::sync::atomic::Ordering::Relaxed;
        let path = tmp("buffer");
        let w = StateWriter::start(&path).unwrap();
        let row = |id: i64| AsRunRow {
            id,
            item_id: Some(id),
            title: format!("t{id}"),
            artist: String::new(),
            category: "music".into(),
            origin: "rotation".into(),
            block_id: None,
            run_id: None,
            playlist_id: None,
            started_ms: 1_790_000_000_000 + id,
            planned_ms: None,
            flags: vec![],
            ended_ms: None,
            played_ms: None,
            end_reason: None,
        };
        w.send(Msg::AsRunStart(row(1)));
        w.flush();
        for mode in [FaultMode::ReadOnly, FaultMode::Full] {
            w.set_fault(mode);
            for i in 2..=6 {
                w.send(Msg::AsRunStart(row(i)));
            }
            w.flush();
            assert!(w.status.failing.load(Relaxed), "{mode:?}");
            assert!(w.status.backlog.load(Relaxed) >= 5);
            let kind = w.status.failure_kind.load(Relaxed);
            assert_eq!(
                kind,
                if mode == FaultMode::Full {
                    FAIL_FULL
                } else {
                    FAIL_UNAVAILABLE
                }
            );
            assert_eq!(read_as_run(&path).unwrap().len(), 1, "nothing got in");
            w.set_fault(FaultMode::None);
            let t = std::time::Instant::now();
            while w.status.failing.load(Relaxed) || w.status.backlog.load(Relaxed) > 0 {
                w.flush();
                assert!(t.elapsed().as_secs() < 10);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let ids: Vec<i64> = read_as_run(&path).unwrap().iter().map(|r| r.id).collect();
            assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
            // Reset for the next mode.
            let conn = Connection::open(&path).unwrap();
            conn.execute("DELETE FROM as_run WHERE id > 1", []).unwrap();
        }
    }
}
