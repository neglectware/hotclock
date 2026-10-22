//! What the station and the library service write about an item that fails (architecture 1.5):
//! quarantine with a reason, release, the playback error counter, and re-analysis that keeps the old
//! analysis when it fails. Every write bumps `meta.library_revision`, so stations reload (5.2).
//!
//! A station never writes `library.db` (one writer per database; its systemd unit mounts the file
//! read-only). What it decides about an item goes into its own `state.db` (`library_outbox`), and the
//! library service applies those rows here ([`apply_station_outbox`], called from its loop every 2 s).

use crate::util::now_iso;
use crate::{Library, LibraryError, Result};
use analysis::analyze::analyze_file;
use analysis::markers::{flags_to_json, ProfileParams};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// The reasons an item is quarantined (5.2).
pub const REASONS: [&str; 6] = [
    "decode",
    "missing",
    "truncated",
    "silent",
    "hash_mismatch",
    "manual",
];

/// Quarantine an item: never picked, dropped from queues at the next preflight. Returns false when it
/// was quarantined already (nothing changes then, and no revision is bumped).
pub fn quarantine(
    conn: &Connection,
    item_id: i64,
    reason: &str,
    errors_to_add: i64,
) -> Result<bool> {
    if !REASONS.contains(&reason) {
        return Err(LibraryError::Invalid(format!(
            "unknown quarantine reason `{reason}`"
        )));
    }
    let n = conn.execute(
        "UPDATE items SET quarantined = 1, quarantine_reason = ?2, quarantined_at = ?3,
            playback_errors = playback_errors + ?4, updated_at = ?3
         WHERE id = ?1 AND quarantined = 0",
        params![item_id, reason, now_iso(), errors_to_add],
    )?;
    if n > 0 {
        Library::bump_revision(conn)?;
    }
    Ok(n > 0)
}

/// Release from quarantine ("Release from quarantine" in the item editor): also resets
/// `playback_errors`. Returns false when the item was not quarantined.
pub fn release(conn: &Connection, item_id: i64) -> Result<bool> {
    let n = conn.execute(
        "UPDATE items SET quarantined = 0, quarantine_reason = NULL, quarantined_at = NULL,
            playback_errors = 0, updated_at = ?2
         WHERE id = ?1 AND quarantined = 1",
        params![item_id, now_iso()],
    )?;
    if n > 0 {
        Library::bump_revision(conn)?;
    }
    Ok(n > 0)
}

/// `playback_errors += n`; returns the new count.
pub fn add_playback_errors(conn: &Connection, item_id: i64, n: i64) -> Result<i64> {
    conn.execute(
        "UPDATE items SET playback_errors = playback_errors + ?2 WHERE id = ?1",
        params![item_id, n],
    )?;
    Library::bump_revision(conn)?;
    Ok(conn
        .query_row(
            "SELECT playback_errors FROM items WHERE id = ?1",
            params![item_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Apply what one station has queued for `library.db` (its `state.db` table `library_outbox`, read
/// read-only): quarantines, releases and playback errors, each row once. The place reached is kept in
/// `meta` per station and per `state.db` instance, in the same transaction as the writes. A quarantine or
/// an error count for a file that has been replaced since (another hash) is skipped: the station's
/// judgement was about the old file. Returns how many rows were applied.
pub fn apply_station_outbox(conn: &Connection, station: &str, state_db: &Path) -> Result<usize> {
    if !state_db.exists() {
        return Ok(0);
    }
    let st = Connection::open_with_flags(
        state_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    st.busy_timeout(std::time::Duration::from_secs(2))?;
    let has: Option<String> = st
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='library_outbox'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if has.is_none() {
        return Ok(0);
    }
    let instance: String = st
        .query_row("SELECT value FROM meta WHERE key='instance'", [], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or_default();
    let key = format!("outbox:{station}");
    let place: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?;
    let after: i64 = place
        .as_deref()
        .and_then(|v| v.rsplit_once(':'))
        .filter(|(inst, _)| *inst == instance)
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0);
    type Row = (i64, String, i64, Option<String>, i64, String);
    let rows: Vec<Row> = {
        let mut q = st.prepare(
            "SELECT id, op, item_id, reason, n, sha256 FROM library_outbox WHERE id > ?1 ORDER BY id",
        )?;
        let r = q.query_map(params![after], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        r.collect::<std::result::Result<_, _>>()?
    };
    let Some(last) = rows.last().map(|r| r.0) else {
        return Ok(0);
    };
    let tx = conn.unchecked_transaction()?;
    for (_, op, item_id, reason, n, sha) in &rows {
        let current: Option<String> = tx
            .query_row(
                "SELECT sha256 FROM items WHERE id = ?1",
                params![item_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(current) = current else { continue };
        let same_file = sha.is_empty() || *sha == current;
        match op.as_str() {
            "quarantine" if same_file => {
                let reason = reason.as_deref().unwrap_or("manual");
                let reason = if REASONS.contains(&reason) {
                    reason
                } else {
                    "manual"
                };
                quarantine(&tx, *item_id, reason, 0)?;
            }
            "add_errors" if same_file => {
                add_playback_errors(&tx, *item_id, *n)?;
            }
            "release" => {
                release(&tx, *item_id)?;
            }
            _ => {}
        }
    }
    tx.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, format!("{instance}:{last}")],
    )?;
    tx.commit()?;
    Ok(rows.len())
}

/// [`apply_station_outbox`] for every station whose state lives beside this library
/// (`<data dir>/stations/<name>/state.db`). Errors of one station are logged and do not stop the others.
pub fn apply_station_outboxes(lib: &Library) -> usize {
    let Some(dir) = lib.paths.db.parent().map(|d| d.join("stations")) else {
        return 0;
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let db = e.path().join("state.db");
        match apply_station_outbox(&lib.conn, &name, &db) {
            Ok(k) => n += k,
            Err(err) => {
                tracing::warn!(station = %name, "cannot apply the station's library writes: {err}")
            }
        }
    }
    n
}

/// What a quarantined item looks like to the Library screen.
#[derive(Debug, Clone, PartialEq)]
pub struct QuarantineInfo {
    pub item_id: i64,
    pub title: String,
    pub reason: Option<String>,
    pub at: Option<String>,
    pub playback_errors: i64,
}

pub fn quarantined_items(conn: &Connection) -> Result<Vec<QuarantineInfo>> {
    let mut st = conn.prepare(
        "SELECT id, title, quarantine_reason, quarantined_at, playback_errors FROM items
         WHERE quarantined = 1 AND deleted_at IS NULL ORDER BY quarantined_at, id",
    )?;
    let rows = st.query_map([], |r| {
        Ok(QuarantineInfo {
            item_id: r.get(0)?,
            title: r.get(1)?,
            reason: r.get(2)?,
            at: r.get(3)?,
            playback_errors: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The outcome of a re-analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum Reanalysis {
    /// New analysis stored, effective markers recomputed.
    Updated,
    /// It failed: the previous analysis and markers are kept, the item stays playable, and
    /// `items.analysis_failed` is set for review (1.5).
    KeptOld { error: String },
}

impl Library {
    /// The items (not deleted) whose stored analysis was made by an older version of the analysis
    /// (`analysis::analyze::ALGO_VERSION`), oldest id first: what `reanalyze --stale` analyzes again.
    pub fn stale_analysis_ids(&self) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare(
            "SELECT i.id FROM items i JOIN analysis a ON a.item_id = i.id
             WHERE i.deleted_at IS NULL AND a.algo_version < ?1 ORDER BY i.id",
        )?;
        let rows = st.query_map(params![analysis::analyze::ALGO_VERSION], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn quarantine_item(&self, item_id: i64, reason: &str) -> Result<bool> {
        quarantine(&self.conn, item_id, reason, 0)
    }

    pub fn release_item(&self, item_id: i64) -> Result<bool> {
        release(&self.conn, item_id)
    }

    /// Analyze the stored file again and replace the item's analysis (after an algorithm change, or
    /// on request). A failure never damages the item: the old analysis and markers stay, the item
    /// stays playable, `analysis_failed` is set. `inject_failure` is a test hook that fails the
    /// analysis with the given message (an item "whose re-analysis is forced to fail").
    pub fn reanalyze_item(&self, item_id: i64, inject_failure: Option<&str>) -> Result<Reanalysis> {
        let (store_path, profile_name): (String, String) = self
            .conn
            .query_row(
                "SELECT i.store_path, COALESCE(o.segue_profile, c.segue_profile)
                 FROM items i JOIN categories c ON c.id = i.category_id
                 LEFT JOIN item_overrides o ON o.item_id = i.id
                 WHERE i.id = ?1",
                params![item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(LibraryError::NoItem(item_id))?;
        let mut profile =
            ProfileParams::builtin(&profile_name).unwrap_or_else(ProfileParams::music);
        profile.name = profile_name;
        let result = match inject_failure {
            Some(m) => Err(m.to_string()),
            None => analyze_file(&self.paths.media.join(&store_path), &profile)
                .map_err(|e| e.to_string()),
        };
        match result {
            Err(error) => {
                self.conn.execute(
                    "UPDATE items SET analysis_failed = 1, analysis_error = ?2, updated_at = ?3 WHERE id = ?1",
                    params![item_id, error, now_iso()],
                )?;
                Library::bump_revision(&self.conn)?;
                Ok(Reanalysis::KeptOld { error })
            }
            Ok(a) => {
                let tx = self.conn.unchecked_transaction()?;
                let wave = zstd::encode_all(&a.wave[..], 3).unwrap_or_default();
                tx.execute(
                    "UPDATE analysis SET algo_version=?2, l_int=?3, lra=?4, true_peak=?5, sample_peak=?6,
                        auto_cue_in=?7, auto_attack=?8, auto_segue=?9, auto_cue_out=?10, flags=?11,
                        env_e10=?12, env_e50=?13, env_k200=?14, env_k10=?15, wave=?16,
                        ramp_start=?17, ramp_end=?18, analyzed_at=?19
                     WHERE item_id = ?1",
                    params![
                        item_id,
                        analysis::analyze::ALGO_VERSION,
                        a.l_int,
                        a.lra,
                        a.true_peak,
                        a.sample_peak,
                        a.auto.cue_in,
                        a.auto.attack,
                        a.auto.segue,
                        a.auto.cue_out,
                        flags_to_json(&a.auto.flags),
                        a.stored.e10,
                        a.stored.e50,
                        a.stored.k200,
                        a.stored.k10,
                        wave,
                        a.ramp_start,
                        a.ramp_end,
                        now_iso()
                    ],
                )?;
                tx.execute(
                    "UPDATE items SET analysis_failed = 0, analysis_error = NULL, updated_at = ?2 WHERE id = ?1",
                    params![item_id, now_iso()],
                )?;
                crate::items::materialize(&tx, item_id)?;
                tx.commit()?;
                Ok(Reanalysis::Updated)
            }
        }
    }
}
