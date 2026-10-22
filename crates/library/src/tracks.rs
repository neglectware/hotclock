//! Everything a station needs to play an item without asking the library again (architecture
//! 5.3, `track_cache`): the row the station copies into `state.db` and builds its in-memory
//! `Item` from. Read here so the join over `items`, `item_effective`, `analysis`, `categories` and
//! `item_overrides` exists once.

use crate::Result;
use analysis::markers::{flags_from_json, Flag};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackRow {
    pub item_id: i64,
    /// `item_effective.revision`.
    pub revision: i64,
    /// Path of the stored file, relative to the media directory.
    pub store_path: String,
    pub file_bytes: i64,
    pub file_ms: i64,
    pub title: String,
    pub artist: String,
    /// The album title as tagged (`album_key` is its normalized form).
    #[serde(default)]
    pub album: String,
    pub category: String,
    pub category_id: i64,
    pub kind: String,
    pub album_key: String,
    pub disc_no: Option<i64>,
    pub track_no: Option<i64>,
    pub track_total: Option<i64>,
    pub art_sha: Option<String>,
    // The effective markers, ms from the start of the file.
    pub cue_in: i64,
    pub attack: i64,
    pub intro: Option<i64>,
    pub segue: i64,
    pub fade_out: Option<i64>,
    pub cue_out: i64,
    pub flags: Vec<String>,
    // Loudness inputs (3.6).
    pub l_used: f64,
    pub true_peak: f64,
    pub gain_offset_db: f64,
    pub gain_trim_db: f64,
    // Manual overrides in force, which decide the play window and the ramps (2.6).
    pub manual_cue_in: bool,
    pub manual_cue_out: bool,
    pub manual_segue: bool,
    // Transition layers (2.6): the category's preset and patch, then the item's own below.
    pub category_preset: Option<String>,
    pub category_patch: String,
    /// The item's own `params.transition` (`{preset?, patch?}`), JSON.
    pub item_transition: String,
    /// Plex-style ramps over the whole file, as stored.
    pub ramp_start: String,
    pub ramp_end: String,
    /// The zstd `K10` blocks (window ramps inside a cue window).
    #[serde(skip)]
    pub k10: Vec<u8>,
    /// The zstd `K200` blocks (the `fm` preset's `last_crossing` exit), stored as the library holds them:
    /// `i16` in 0.1 dB steps, zstd-packed. Empty in a track cache written before schema 2.
    #[serde(skip)]
    pub k200: Vec<u8>,
    pub enabled: bool,
    pub quarantined: bool,
    pub deleted: bool,
    /// A deliberate quiet piece: exempt from the program-silence detector (1.5).
    #[serde(default)]
    pub silence_ok: bool,
    /// Hash of the stored file: a quarantine ends when the file is replaced (1.5).
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub quarantine_reason: Option<String>,
    /// A re-analysis failed; the previous analysis is kept and the item stays playable.
    #[serde(default)]
    pub analysis_failed: bool,
    /// The longest stretch below -50 dBFS between cue-in and cue-out, ms (1.5): for display.
    #[serde(default)]
    pub longest_quiet_ms: i64,
    /// The same over the whole file (a `bounds = file` preset plays the quiet before the cue-in too).
    #[serde(default)]
    pub longest_quiet_file_ms: i64,
    /// The same over a ladder of thresholds (`analysis::quiet::QuietProfile`, JSON; empty before schema 4):
    /// the station's silence detector reads it at its threshold minus the item's gain.
    #[serde(default)]
    pub quiet_profile: String,
}

impl TrackRow {
    pub fn has_flag(&self, f: Flag) -> bool {
        self.flags.iter().any(|n| n == f.name())
    }

    /// Absolute path of the stored file.
    pub fn path(&self, media: &Path) -> std::path::PathBuf {
        media.join(&self.store_path)
    }

    /// Length of the item as it plays between its markers (`cue_in` to `segue`), ms.
    pub fn effective_ms(&self) -> i64 {
        (self.segue - self.cue_in).max(1)
    }
}

const SELECT: &str =
    "SELECT i.id, e.revision, i.store_path, i.file_bytes, i.file_ms, i.title, i.artist,
        c.name, c.kind, i.album_key, i.disc_no, i.track_no, i.track_total, i.art_sha,
        e.cue_in, e.attack, e.intro, e.segue, e.fade_out, e.cue_out, e.flags,
        e.l_int, e.true_peak, e.gain_offset_db, c.transition_preset, c.transition_patch,
        a.ramp_start, a.ramp_end, a.env_k10,
        o.ovr_cue_in, o.ovr_cue_out, o.ovr_segue, o.ovr_gain_trim_db, o.params,
        i.enabled, i.quarantined, i.deleted_at, i.category_id,
        i.silence_ok, i.sha256, i.quarantine_reason, i.analysis_failed,
        e.longest_quiet_ms, e.quiet_profile, e.longest_quiet_file_ms, i.album, a.env_k200
 FROM items i
 JOIN item_effective e ON e.item_id = i.id
 JOIN analysis a ON a.item_id = i.id
 JOIN categories c ON c.id = i.category_id
 LEFT JOIN item_overrides o ON o.item_id = i.id";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<TrackRow> {
    let flags_text: String = r.get(20)?;
    let flag_set = flags_from_json(&flags_text);
    let segue: i64 = r.get(17)?;
    let ovr_cue_in: Option<i64> = r.get(29)?;
    let ovr_cue_out: Option<i64> = r.get(30)?;
    let ovr_segue: Option<i64> = r.get(31)?;
    let params_text: Option<String> = r.get(33)?;
    let item_transition = params_text
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("transition").cloned())
        .map_or("{}".to_string(), |v| v.to_string());
    Ok(TrackRow {
        item_id: r.get(0)?,
        revision: r.get(1)?,
        store_path: r.get(2)?,
        file_bytes: r.get(3)?,
        file_ms: r.get(4)?,
        title: r.get(5)?,
        artist: r.get(6)?,
        album: r.get(45)?,
        category: r.get(7)?,
        category_id: r.get(37)?,
        kind: r.get(8)?,
        album_key: r.get(9)?,
        disc_no: r.get(10)?,
        track_no: r.get(11)?,
        track_total: r.get(12)?,
        art_sha: r.get(13)?,
        cue_in: r.get(14)?,
        attack: r.get(15)?,
        intro: r.get(16)?,
        segue,
        fade_out: r.get(18)?,
        cue_out: r.get(19)?,
        flags: flag_set.iter().map(|f| f.name().to_string()).collect(),
        l_used: r.get(21)?,
        true_peak: r.get(22)?,
        gain_offset_db: r.get(23)?,
        category_preset: r.get(24)?,
        category_patch: r.get(25)?,
        ramp_start: r.get(26)?,
        ramp_end: r.get(27)?,
        k10: r.get(28)?,
        k200: r.get(46)?,
        gain_trim_db: r.get::<_, Option<f64>>(32)?.unwrap_or(0.0),
        manual_cue_in: ovr_cue_in.is_some() && !flag_set.contains(&Flag::CueOverrideInvalid),
        manual_cue_out: ovr_cue_out.is_some() && !flag_set.contains(&Flag::CueOverrideInvalid),
        manual_segue: ovr_segue == Some(segue),
        item_transition,
        enabled: r.get::<_, i64>(34)? != 0,
        quarantined: r.get::<_, i64>(35)? != 0,
        deleted: r.get::<_, Option<String>>(36)?.is_some(),
        silence_ok: r.get::<_, i64>(38)? != 0,
        sha256: r.get(39)?,
        quarantine_reason: r.get(40)?,
        analysis_failed: r.get::<_, i64>(41)? != 0,
        longest_quiet_ms: r.get(42)?,
        quiet_profile: r.get(43)?,
        longest_quiet_file_ms: r.get(44)?,
    })
}

/// One item's row, or `None` when it does not exist or has no analysis yet.
pub fn read_track(conn: &Connection, item_id: i64) -> Result<Option<TrackRow>> {
    let sql = format!("{SELECT} WHERE i.id = ?1");
    // Cached: a station's library sync reads every referenced track this way on each change.
    let mut st = conn.prepare_cached(&sql)?;
    Ok(st.query_row(params![item_id], from_row).optional()?)
}

/// Every item the library holds (not deleted) with an analysis and effective row.
pub fn read_all_tracks(conn: &Connection) -> Result<Vec<TrackRow>> {
    let sql = format!("{SELECT} WHERE i.deleted_at IS NULL ORDER BY i.id");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The items of one category (not deleted).
pub fn read_category_track_ids(conn: &Connection, category_id: i64) -> Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT id FROM items WHERE category_id = ?1 AND deleted_at IS NULL ORDER BY id",
    )?;
    let rows = st.query_map(params![category_id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The items of one album (not deleted), in disc and track order.
pub fn read_album_track_ids(conn: &Connection, album_key: &str) -> Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT id FROM items WHERE album_key = ?1 AND deleted_at IS NULL
         ORDER BY track_no IS NULL, COALESCE(disc_no, 1), track_no, id",
    )?;
    let rows = st.query_map(params![album_key], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
