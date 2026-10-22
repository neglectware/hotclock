//! The station's in-memory view of the library: the tracks its playlists and blocks reference
//! (`library::tracks::TrackRow`, the same rows `state.db`'s `track_cache` holds) and the `Item` the
//! conductor plays, built from a row on first use. The scheduler reads only [`TrackMeta`].

use crate::item::Item;
use library::tracks::TrackRow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What the scheduler needs to know about a track.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackMeta {
    pub item_id: i64,
    pub title: String,
    pub artist: String,
    pub category: String,
    pub category_id: i64,
    pub kind: String,
    pub album_key: String,
    pub disc_no: Option<i64>,
    pub track_no: Option<i64>,
    pub track_total: Option<i64>,
    /// `segue - cue_in`, ms: what the design calls the effective length (4.3).
    pub effective_ms: i64,
    /// `cue_out - cue_in`, ms: how long the item plays when nothing follows it (back-timing, 4.9).
    pub play_ms: i64,
    pub file_ms: i64,
    pub l_used: f64,
    pub true_peak: f64,
    /// Enabled, not quarantined and not deleted.
    pub playable: bool,
}

impl TrackMeta {
    pub fn is_music(&self) -> bool {
        self.kind == "music"
    }

    /// Casefolded title and artist: the key of the "same song" separation rule (4.3).
    pub fn title_key(&self) -> String {
        format!("{}|{}", norm(&self.title), norm(&self.artist))
    }
}

pub fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub struct TrackEntry {
    pub row: TrackRow,
    pub meta: TrackMeta,
    media: PathBuf,
    item: Mutex<Option<Result<Arc<Item>, String>>>,
}

impl TrackEntry {
    fn new(row: TrackRow, media: &Path) -> Self {
        let meta = TrackMeta {
            item_id: row.item_id,
            title: row.title.clone(),
            artist: row.artist.clone(),
            category: row.category.clone(),
            category_id: row.category_id,
            kind: row.kind.clone(),
            album_key: row.album_key.clone(),
            disc_no: row.disc_no,
            track_no: row.track_no,
            track_total: row.track_total,
            effective_ms: row.effective_ms(),
            play_ms: (row.cue_out - row.cue_in).max(1),
            file_ms: row.file_ms,
            l_used: row.l_used,
            true_peak: row.true_peak,
            playable: row.enabled && !row.quarantined && !row.deleted,
        };
        Self {
            row,
            meta,
            media: media.to_path_buf(),
            item: Mutex::new(None),
        }
    }

    /// The `Item` the conductor plays, built once from the row.
    pub fn item(&self) -> Result<Arc<Item>, String> {
        let mut g = self.item.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(|| Item::from_row(&self.row, self.row.path(&self.media)).map(Arc::new))
            .clone()
    }
}

#[derive(Debug, Clone)]
struct Forced {
    sha: String,
    reason: String,
    /// A row from the library has shown the item quarantined: from then on a row that does not is a
    /// release.
    seen_in_library: bool,
}

#[derive(Default)]
pub struct Catalog {
    media: PathBuf,
    tracks: HashMap<i64, Arc<TrackEntry>>,
    /// Items the station quarantined itself (1.5) and the library may not show yet: `(sha256 of the
    /// file it judged, reason)`. A row from the library for that file is treated as quarantined; a row
    /// with another hash (the file was replaced) is not.
    forced: HashMap<i64, Forced>,
    /// Forced quarantines that ended since the station last asked (released in the library, or the
    /// file was replaced): `[(item id, why)]`.
    ended: Vec<(i64, &'static str)>,
    /// Forced quarantines the library has just been seen to hold (to be remembered in `state.db`).
    seen: Vec<i64>,
    /// Some item became playable or unplayable (or appeared) since the station last asked: the soft plan
    /// must be dealt again (a quarantine ended, an item was disabled).
    playable_changed: bool,
    /// Bumped whenever a row changed, appeared or went away.
    pub generation: u64,
}

impl Catalog {
    pub fn new(media: &Path) -> Self {
        Self {
            media: media.to_path_buf(),
            tracks: HashMap::new(),
            forced: HashMap::new(),
            ended: Vec::new(),
            seen: Vec::new(),
            playable_changed: false,
            generation: 0,
        }
    }

    pub fn media(&self) -> &Path {
        &self.media
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn get(&self, id: i64) -> Option<Arc<TrackEntry>> {
        self.tracks.get(&id).cloned()
    }

    pub fn meta(&self, id: i64) -> Option<&TrackMeta> {
        self.tracks.get(&id).map(|t| &t.meta)
    }

    pub fn ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.tracks.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Insert or replace rows. Returns the ids whose row changed (or is new).
    pub fn apply(&mut self, rows: Vec<TrackRow>) -> Vec<i64> {
        let mut changed = Vec::new();
        for mut r in rows {
            if let Some(f) = self.forced.get_mut(&r.item_id) {
                if f.sha != r.sha256 {
                    // The file was replaced: the judgement was about the old one.
                    self.forced.remove(&r.item_id);
                    self.ended.push((r.item_id, "replaced"));
                } else if r.quarantined {
                    if !f.seen_in_library {
                        f.seen_in_library = true;
                        self.seen.push(r.item_id);
                    }
                } else if f.seen_in_library {
                    self.forced.remove(&r.item_id);
                    self.ended.push((r.item_id, "released"));
                } else {
                    r.quarantined = true;
                    r.quarantine_reason = Some(f.reason.clone());
                }
            }
            let same = self
                .tracks
                .get(&r.item_id)
                .is_some_and(|old| old.row == r && old.row.k10 == r.k10 && old.row.k200 == r.k200);
            if !same {
                changed.push(r.item_id);
                let now_playable = r.enabled && !r.quarantined && !r.deleted;
                if self
                    .tracks
                    .get(&r.item_id)
                    .is_none_or(|old| old.meta.playable != now_playable)
                {
                    self.playable_changed = true;
                }
                self.tracks
                    .insert(r.item_id, Arc::new(TrackEntry::new(r, &self.media)));
            }
        }
        if !changed.is_empty() {
            self.generation += 1;
        }
        changed
    }

    /// Quarantine an item on the station's own authority (1.5). Returns false when the catalog does not
    /// hold it. The item is not playable from now on, whatever the library says about that file until
    /// [`Catalog::unforce`] (a release) or a row with a different hash (the file was replaced).
    pub fn force_quarantine(&mut self, id: i64, reason: &str) -> bool {
        let Some(e) = self.tracks.get(&id) else {
            return false;
        };
        self.forced.insert(
            id,
            Forced {
                sha: e.row.sha256.clone(),
                reason: reason.to_string(),
                seen_in_library: false,
            },
        );
        let mut row = e.row.clone();
        row.quarantined = true;
        row.quarantine_reason = Some(reason.to_string());
        self.tracks
            .insert(id, Arc::new(TrackEntry::new(row, &self.media)));
        self.generation += 1;
        true
    }

    /// Restore forced quarantines read from `state.db` at start: `(item id, sha256, reason, seen in the
    /// library)`. Call before the first [`Catalog::apply`]. "Seen" matters: a quarantine the library had
    /// taken over and has since released (while the station was down) must end, not be enforced again as if
    /// the library had not caught up yet.
    pub fn restore_forced(&mut self, rows: &[(i64, String, String, bool)]) {
        for (id, sha, reason, seen) in rows {
            self.forced.insert(
                *id,
                Forced {
                    sha: sha.clone(),
                    reason: reason.clone(),
                    seen_in_library: *seen,
                },
            );
        }
    }

    /// Forced quarantines the library has been seen to hold since the last call.
    pub fn take_seen(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.seen)
    }

    /// Did any item change between playable and not since the last call?
    pub fn take_playable_changed(&mut self) -> bool {
        std::mem::take(&mut self.playable_changed)
    }

    /// Forced quarantines that ended through a library row since the last call.
    pub fn take_ended(&mut self) -> Vec<(i64, &'static str)> {
        std::mem::take(&mut self.ended)
    }

    /// End a forced quarantine by hand (the operator's "release", before the library shows it): the
    /// item is playable again. Returns true when the item was forced.
    pub fn unforce(&mut self, id: i64) -> bool {
        if self.forced.remove(&id).is_none() {
            return false;
        }
        if let Some(e) = self.tracks.get(&id) {
            let mut row = e.row.clone();
            row.quarantined = false;
            row.quarantine_reason = None;
            self.tracks
                .insert(id, Arc::new(TrackEntry::new(row, &self.media)));
            self.generation += 1;
        }
        true
    }

    /// Is the item quarantined on the station's authority?
    pub fn forced_reason(&self, id: i64) -> Option<&str> {
        self.forced.get(&id).map(|f| f.reason.as_str())
    }

    pub fn forced_hash(&self, id: i64) -> Option<&str> {
        self.forced.get(&id).map(|f| f.sha.as_str())
    }

    /// Drop every track not in `keep`. Returns how many went.
    pub fn retain_ids(&mut self, keep: &dyn Fn(i64) -> bool) -> usize {
        let before = self.tracks.len();
        self.tracks.retain(|id, _| keep(*id));
        let gone = before - self.tracks.len();
        if gone > 0 {
            self.generation += 1;
        }
        gone
    }

    /// The playable items of a category.
    pub fn category_items(&self, category_id: i64) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .tracks
            .values()
            .filter(|t| t.meta.category_id == category_id && t.meta.playable)
            .map(|t| t.meta.item_id)
            .collect();
        v.sort_unstable();
        v
    }

    pub fn category_id_by_name(&self, name: &str) -> Option<i64> {
        let n = norm(name);
        self.tracks
            .values()
            .find(|t| norm(&t.meta.category) == n)
            .map(|t| t.meta.category_id)
    }

    /// The playable tracks of an album in disc and track order. A track without a number cannot be
    /// ordered and follows the numbered ones, by id (3.7: it is never adjacent to anything).
    pub fn album_tracks(&self, album_key: &str) -> Vec<Arc<TrackEntry>> {
        if album_key.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<Arc<TrackEntry>> = self
            .tracks
            .values()
            .filter(|t| t.meta.album_key == album_key && t.meta.playable)
            .cloned()
            .collect();
        v.sort_by_key(|t| {
            (
                t.meta.track_no.is_none(),
                t.meta.disc_no.unwrap_or(1),
                t.meta.track_no.unwrap_or(0),
                t.meta.item_id,
            )
        });
        v
    }

    /// The last track number of `disc` of `album_key`: the `TRACKTOTAL` tag of any of its tracks,
    /// else the highest number the library holds (3.7).
    pub fn last_track_of_disc(&self, album_key: &str, disc: i64) -> Option<i64> {
        let mut total = None;
        let mut highest = None;
        for t in self.tracks.values() {
            if t.meta.album_key != album_key || t.meta.disc_no.unwrap_or(1) != disc {
                continue;
            }
            if let Some(tt) = t.meta.track_total {
                total = total.max(Some(tt));
            }
            if let Some(n) = t.meta.track_no {
                highest = highest.max(Some(n));
            }
        }
        total.or(highest)
    }

    /// Are `x` then `y` adjacent tracks of one album (3.7)? Both music, the same `album_key`, and
    /// either the next track on the same disc, or track 1 of the next disc after the last track of the
    /// disc. A missing track number is never adjacent; a missing disc tag is disc 1.
    pub fn adjacent(&self, x: &TrackMeta, y: &TrackMeta) -> bool {
        if !x.is_music() || !y.is_music() || x.album_key.is_empty() || x.album_key != y.album_key {
            return false;
        }
        let (Some(tx), Some(ty)) = (x.track_no, y.track_no) else {
            return false;
        };
        let (dx, dy) = (x.disc_no.unwrap_or(1), y.disc_no.unwrap_or(1));
        if dx == dy && ty == tx + 1 {
            return true;
        }
        dy == dx + 1 && ty == 1 && self.last_track_of_disc(&x.album_key, dx) == Some(tx)
    }

    /// Mean `effective_ms` of the given items (4.3's `m`); `default_ms` when the list is empty.
    pub fn mean_effective_ms(&self, ids: &[i64], default_ms: f64) -> f64 {
        let v: Vec<f64> = ids
            .iter()
            .filter_map(|i| self.meta(*i))
            .map(|m| m.effective_ms as f64)
            .collect();
        if v.is_empty() {
            default_ms
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    }

    /// Distinct album keys with their track counts (for the albums list and tests).
    pub fn albums(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for t in self.tracks.values() {
            if !t.meta.album_key.is_empty() {
                *m.entry(t.meta.album_key.clone()).or_insert(0) += 1;
            }
        }
        m
    }
}

/// Synthetic rows for tests that need no audio (scheduler, deck and simulation tests).
pub mod testing {
    use super::*;

    /// A playable row: `file_ms` long, markers `cue_in` 0 and `segue` at `file_ms - tail_ms`.
    pub fn row(
        id: i64,
        title: &str,
        artist: &str,
        category: &str,
        kind: &str,
        file_ms: i64,
    ) -> TrackRow {
        let category_id = match category {
            "music" => 1,
            "liner" => 2,
            "id" => 3,
            other => 100 + other.len() as i64,
        };
        TrackRow {
            item_id: id,
            revision: 1,
            store_path: format!("{id}.flac"),
            file_bytes: 1000,
            file_ms,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            category: category.into(),
            category_id,
            kind: kind.into(),
            album_key: String::new(),
            disc_no: None,
            track_no: None,
            track_total: None,
            art_sha: None,
            cue_in: 0,
            attack: 20,
            intro: None,
            segue: file_ms,
            fade_out: None,
            cue_out: file_ms,
            flags: vec![],
            l_used: -14.0,
            true_peak: -3.0,
            gain_offset_db: 0.0,
            gain_trim_db: 0.0,
            manual_cue_in: false,
            manual_cue_out: false,
            manual_segue: false,
            category_preset: None,
            category_patch: "{}".into(),
            item_transition: "{}".into(),
            ramp_start: String::new(),
            ramp_end: String::new(),
            k10: vec![],
            k200: vec![],
            enabled: true,
            quarantined: false,
            deleted: false,
            silence_ok: false,
            sha256: format!("test-{id}"),
            quarantine_reason: None,
            analysis_failed: false,
            longest_quiet_ms: 0,
            longest_quiet_file_ms: 0,
            quiet_profile: String::new(),
        }
    }

    /// An album track.
    #[allow(clippy::too_many_arguments)]
    pub fn album_row(
        id: i64,
        title: &str,
        album: &str,
        disc: Option<i64>,
        track: Option<i64>,
        total: Option<i64>,
        file_ms: i64,
    ) -> TrackRow {
        let mut r = row(id, title, "Artist", "music", "music", file_ms);
        r.album_key = format!("artist|{album}");
        r.disc_no = disc;
        r.track_no = track;
        r.track_total = total;
        r
    }

    pub fn catalog(rows: Vec<TrackRow>) -> Catalog {
        let mut c = Catalog::new(Path::new("/nonexistent"));
        c.apply(rows);
        c
    }
}

#[cfg(test)]
mod forced_tests {
    use super::*;
    use testing::row;

    fn cat_with(id: i64) -> Catalog {
        let mut c = Catalog::new(Path::new("/media"));
        c.apply(vec![row(id, "Song", "Artist", "music", "music", 240_000)]);
        c
    }

    #[test]
    fn a_forced_quarantine_holds_until_the_library_shows_it_and_then_follows_the_library() {
        let mut c = cat_with(1);
        assert!(c.meta(1).unwrap().playable);
        assert!(c.force_quarantine(1, "silent"));
        assert!(!c.meta(1).unwrap().playable);
        assert_eq!(c.forced_reason(1), Some("silent"));
        // The library has not caught up: its row says playable, the station still refuses it, and the
        // unchanged row is not reported as a change.
        let lib_row = row(1, "Song", "Artist", "music", "music", 240_000);
        let gen = c.generation;
        assert!(c.apply(vec![lib_row.clone()]).is_empty());
        assert!(!c.meta(1).unwrap().playable);
        assert_eq!(c.generation, gen);
        // The library shows it quarantined: from now on the library decides.
        let mut q = lib_row.clone();
        q.quarantined = true;
        q.quarantine_reason = Some("silent".into());
        c.apply(vec![q]);
        assert!(!c.meta(1).unwrap().playable);
        assert!(c.take_ended().is_empty());
        // Released in the library: playable again, and the station learns the quarantine ended.
        c.apply(vec![lib_row.clone()]);
        assert!(c.meta(1).unwrap().playable);
        assert_eq!(c.take_ended(), vec![(1, "released")]);
        assert_eq!(c.forced_reason(1), None);
    }

    #[test]
    fn a_replaced_file_ends_the_forced_quarantine() {
        let mut c = cat_with(2);
        c.force_quarantine(2, "truncated");
        let mut new_file = row(2, "Song", "Artist", "music", "music", 240_000);
        new_file.sha256 = "another-hash".into();
        c.apply(vec![new_file]);
        assert!(c.meta(2).unwrap().playable);
        assert_eq!(c.take_ended(), vec![(2, "replaced")]);
    }

    #[test]
    fn restored_quarantines_apply_to_the_first_rows() {
        let mut c = Catalog::new(Path::new("/media"));
        c.restore_forced(&[(3, "test-3".into(), "decode".into(), false)]);
        c.apply(vec![row(3, "Song", "Artist", "music", "music", 1000)]);
        assert!(!c.meta(3).unwrap().playable);
        assert!(c.unforce(3));
        assert!(c.meta(3).unwrap().playable);
    }

    #[test]
    fn a_release_made_while_the_station_was_down_ends_a_restored_quarantine() {
        // The library had taken the quarantine over (seen), then released it while the station was down.
        let mut c = Catalog::new(Path::new("/media"));
        c.restore_forced(&[(4, "test-4".into(), "silent".into(), true)]);
        c.apply(vec![row(4, "Song", "Artist", "music", "music", 1000)]);
        assert!(c.meta(4).unwrap().playable);
        assert_eq!(c.take_ended(), vec![(4, "released")]);
        // And the moment the library is first seen holding one is reported, once.
        let mut c = cat_with(5);
        c.force_quarantine(5, "missing");
        let mut q = row(5, "Song", "Artist", "music", "music", 240_000);
        q.quarantined = true;
        c.apply(vec![q.clone()]);
        c.apply(vec![{
            let mut q2 = q.clone();
            q2.revision = 2;
            q2
        }]);
        assert_eq!(c.take_seen(), vec![5]);
    }
}
