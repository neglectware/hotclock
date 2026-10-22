//! The library service's HTTP routes (7.3): items and overrides, imports, artwork, albums, categories, tags,
//! playlists and their entries, the station proxy, notifications and the display view.

use crate::http::{client, Handler, Reply, Request, Response};
use crate::multipart::{boundary_of, Multipart};
use crate::notify;
use crate::station_http::serve_ws;
use crate::util::art_response;
use crate::web;
use crate::ws::{self, Hub};
use library::items::OverridePatch;
use library::playlists::{NewBlock, NewPlaylist};
use library::{ImportOptions, ImportOutcome, Library, LibraryError, Paths};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct LibraryOptions {
    pub paths: Paths,
    pub product: String,
    pub stations_dir: PathBuf,
    pub secrets: PathBuf,
    /// The global configuration file this service was started with (the Settings page edits it).
    pub global_config: PathBuf,
    pub version: String,
    /// IANA zone timed blocks are written in (1.6); the validator and the weekly grid read it.
    pub time_zone: String,
}

/// The library's categories and items, for the validator's source checks (4.4).
struct DbRefs<'a>(&'a rusqlite::Connection);

impl station::programming::Refs for DbRefs<'_> {
    fn category_exists(&self, id: Option<i64>, name: Option<&str>) -> bool {
        match (id, name) {
            (Some(i), _) => self
                .0
                .query_row("SELECT 1 FROM categories WHERE id = ?1", [i], |_| Ok(()))
                .is_ok(),
            (None, Some(n)) => self
                .0
                .query_row(
                    "SELECT 1 FROM categories WHERE lower(name) = lower(?1)",
                    [n],
                    |_| Ok(()),
                )
                .is_ok(),
            _ => false,
        }
    }

    fn item_exists(&self, id: i64) -> bool {
        self.0
            .query_row(
                "SELECT 1 FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |_| Ok(()),
            )
            .is_ok()
    }

    fn category_playable(&self, id: Option<i64>, name: Option<&str>) -> Option<usize> {
        let sql = "SELECT COUNT(*) FROM items i JOIN categories c ON c.id = i.category_id
                   WHERE i.deleted_at IS NULL AND i.enabled = 1 AND i.quarantined = 0";
        match (id, name) {
            (Some(i), _) => self
                .0
                .query_row(&format!("{sql} AND c.id = ?1"), [i], |r| r.get::<_, i64>(0)),
            (None, Some(n)) => {
                self.0
                    .query_row(&format!("{sql} AND lower(c.name) = lower(?1)"), [n], |r| {
                        r.get::<_, i64>(0)
                    })
            }
            _ => return None,
        }
        .ok()
        .map(|n| n as usize)
    }

    fn category_min_ms(&self, id: Option<i64>, name: Option<&str>) -> Option<i64> {
        let sql = "SELECT MIN(e.cue_out - e.cue_in) FROM items i JOIN item_effective e ON e.item_id = i.id
                   JOIN categories c ON c.id = i.category_id
                   WHERE i.deleted_at IS NULL AND i.enabled = 1 AND i.quarantined = 0";
        match (id, name) {
            (Some(i), _) => self.0.query_row(&format!("{sql} AND c.id = ?1"), [i], |r| {
                r.get::<_, Option<i64>>(0)
            }),
            (None, Some(n)) => {
                self.0
                    .query_row(&format!("{sql} AND lower(c.name) = lower(?1)"), [n], |r| {
                        r.get::<_, Option<i64>>(0)
                    })
            }
            _ => return None,
        }
        .ok()
        .flatten()
    }

    fn playlist_min_ms(&self, playlist_id: i64) -> Option<i64> {
        self.0
            .query_row(
                "SELECT MIN(e.cue_out - e.cue_in) FROM playlist_entries p
                 JOIN items i ON i.id = p.item_id JOIN item_effective e ON e.item_id = i.id
                 WHERE p.playlist_id = ?1 AND i.deleted_at IS NULL AND i.enabled = 1 AND i.quarantined = 0",
                [playlist_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten()
    }
}

/// A block as its fields say it is (4.5): its timing must read, its source must be named. `Err` is
/// what the editor shows on the row.
fn check_block(kind: &str, trigger: &str, params: &Value) -> Result<(), String> {
    let row = library::playlists::BlockRow {
        id: 0,
        name: String::new(),
        kind: kind.into(),
        trigger: trigger.into(),
        priority: 100,
        params: params.clone(),
        enabled: true,
    };
    match station::programming::Block::from_row(&row).problem {
        Some(p) => Err(p),
        None => Ok(()),
    }
}

pub struct LibraryApi {
    pub opts: LibraryOptions,
    pub hub: Arc<Hub>,
    pub started: Instant,
    pub notifications_sent: Arc<AtomicU64>,
    slots: Arc<(Mutex<usize>, Condvar)>,
    preview_cache: Mutex<Vec<(String, Vec<u8>, Value)>>,
    pub(crate) plex_jobs: Mutex<crate::plex_http::PlexJobs>,
    /// The whole box's CPU use for the top bar of the box's screen.
    cpu: crate::sysinfo::CpuSampler,
}

type WaveRow = (Vec<u8>, Vec<u8>, i64, String, f64, i64);

/// Import workers running at once (5.4: two).
const IMPORT_WORKERS: usize = 2;
/// The size of one uploaded file (13: 1 GB per file).
const UPLOAD_LIMIT: u64 = 1 << 30;
/// The size of one upload request, all its files together. Larger than one file so that a batch of
/// files, each within [`UPLOAD_LIMIT`], is not refused for its total; the free-space check below still
/// looks at the whole request.
const UPLOAD_REQUEST_LIMIT: u64 = 16 << 30;
/// Disk space an upload must leave free (besides twice its size: the temporary file and the stored copy).
const UPLOAD_RESERVE: u64 = 2 << 30;

fn bad(msg: &str) -> Response {
    Response::error(400, "bad_request", msg)
}
fn not_found(msg: &str) -> Response {
    Response::error(404, "not_found", msg)
}
fn conflict(msg: &str) -> Response {
    Response::error(409, "conflict", msg)
}
fn id_of(s: &str) -> Option<i64> {
    s.parse().ok()
}

fn lerr(e: LibraryError) -> Response {
    match e {
        LibraryError::NoItem(i) => not_found(&format!("no item {i}")),
        LibraryError::Invalid(m) => Response::error(422, "invalid", &m),
        e @ LibraryError::MarkerOutsideWindow { .. } => {
            Response::error(422, "invalid", &e.to_string())
        }
        LibraryError::Db(rusqlite::Error::SqliteFailure(f, m))
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            conflict(&m.unwrap_or_else(|| "a constraint was violated".into()))
        }
        e => Response::error(500, "internal", &e.to_string()),
    }
}

fn dberr(e: rusqlite::Error) -> Response {
    lerr(LibraryError::Db(e))
}

fn opt_patch(b: &Value, key: &str) -> Option<Option<i64>> {
    b.get(key).map(Value::as_i64)
}

pub fn effective_json(e: &library::items::EffectiveRow) -> Value {
    json!({
        "cue_in": e.cue_in, "attack": e.attack, "intro": e.intro, "segue": e.segue, "fade_out": e.fade_out,
        "fade_out_before_end_ms": e.fade_out.map(|f| e.cue_out - f), "cue_out": e.cue_out, "flags": e.flags.iter().map(|f| f.name()).collect::<Vec<_>>(),
        "l_int_source": e.l_int_source, "profile": e.profile, "l_int": e.l_int, "true_peak": e.true_peak,
        "gain_offset_db": e.gain_offset_db, "revision": e.revision,
        "longest_quiet_ms": e.longest_quiet_ms, "longest_quiet_file_ms": e.longest_quiet_file_ms,
    })
}

const ITEM_COLS: &str = "i.id, i.title, i.artist, i.album, i.year, i.album_artist, i.disc_no, i.track_no, i.track_total,
    i.album_key, i.art_sha, c.name, c.kind, i.file_ms, i.enabled, i.quarantined, i.quarantine_reason,
    i.silence_ok, i.notes, i.source, i.orig_filename, i.format, i.sample_rate, i.channels, i.imported_at,
    e.flags, e.l_int, e.longest_quiet_ms, e.longest_quiet_file_ms, e.cue_in, e.attack, e.intro, e.segue, e.fade_out, e.cue_out,
    i.playback_errors, i.analysis_failed, i.file_bytes";

const ITEM_FROM: &str = "FROM items i JOIN categories c ON c.id = i.category_id
    LEFT JOIN item_effective e ON e.item_id = i.id";

fn item_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let id: i64 = r.get(0)?;
    let art: Option<String> = r.get(10)?;
    let flags: Option<String> = r.get(25)?;
    Ok(json!({
        "id": id, "title": r.get::<_, String>(1)?, "artist": r.get::<_, String>(2)?, "album": r.get::<_, String>(3)?,
        "year": r.get::<_, Option<i64>>(4)?, "album_artist": r.get::<_, String>(5)?,
        "disc_no": r.get::<_, Option<i64>>(6)?, "track_no": r.get::<_, Option<i64>>(7)?,
        "track_total": r.get::<_, Option<i64>>(8)?, "album_key": r.get::<_, String>(9)?,
        "art_url": art.map(|_| format!("/api/v1/items/{id}/art?size=144")),
        "category": r.get::<_, String>(11)?, "kind": r.get::<_, String>(12)?,
        "duration_ms": r.get::<_, i64>(13)?, "enabled": r.get::<_, i64>(14)? != 0,
        "quarantined": r.get::<_, i64>(15)? != 0, "quarantine_reason": r.get::<_, Option<String>>(16)?,
        "silence_ok": r.get::<_, i64>(17)? != 0, "notes": r.get::<_, String>(18)?,
        "source": r.get::<_, String>(19)?, "orig_filename": r.get::<_, String>(20)?,
        "format": r.get::<_, String>(21)?, "sample_rate": r.get::<_, i64>(22)?, "channels": r.get::<_, i64>(23)?,
        "imported_at": r.get::<_, String>(24)?,
        "flags": flags.and_then(|f| serde_json::from_str::<Value>(&f).ok()).unwrap_or(json!([])),
        "l_int": r.get::<_, Option<f64>>(26)?,
        "longest_quiet_ms": r.get::<_, Option<i64>>(27)?, "longest_quiet_file_ms": r.get::<_, Option<i64>>(28)?,
        "markers": {"cue_in": r.get::<_, Option<i64>>(29)?, "attack": r.get::<_, Option<i64>>(30)?,
                    "intro": r.get::<_, Option<i64>>(31)?, "segue": r.get::<_, Option<i64>>(32)?,
                    "fade_out": r.get::<_, Option<i64>>(33)?, "cue_out": r.get::<_, Option<i64>>(34)?},
        "playback_errors": r.get::<_, i64>(35)?, "analysis_failed": r.get::<_, i64>(36)? != 0,
        "file_bytes": r.get::<_, i64>(37)?,
    }))
}

impl LibraryApi {
    pub fn new(opts: LibraryOptions) -> Arc<LibraryApi> {
        Arc::new(LibraryApi {
            opts,
            hub: Arc::new(Hub::default()),
            started: Instant::now(),
            notifications_sent: Arc::new(AtomicU64::new(0)),
            slots: Arc::new((Mutex::new(0), Condvar::new())),
            preview_cache: Mutex::new(vec![]),
            plex_jobs: Mutex::new(Default::default()),
            cpu: crate::sysinfo::CpuSampler::new("/proc/stat"),
        })
    }

    pub(crate) fn lib(&self) -> Result<Library, Response> {
        Library::open_worker(self.opts.paths.clone()).map_err(lerr)
    }

    pub(crate) fn audit(
        &self,
        c: &Connection,
        req: &Request,
        entity: &str,
        id: &str,
        diff: &Value,
    ) {
        let _ = c.execute(
            "INSERT INTO audit(at, actor, entity, entity_id, diff) VALUES(?1,?2,?3,?4,?5)",
            params![
                library::util::now_iso(),
                req.peer.ip().to_string(),
                entity,
                id,
                diff.to_string()
            ],
        );
    }

    /// The stations configured on this box: `(name, display_name, listen address)`.
    pub fn stations(&self) -> Vec<(String, String, String)> {
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(&self.opts.stations_dir) {
            for e in rd.flatten() {
                if let Ok(c) = common::config::StationConfig::load(&e.path()) {
                    out.push((c.name.clone(), c.display_name.unwrap_or(c.name), c.listen));
                }
            }
        }
        out.sort();
        out
    }

    /// Before a program is tied to a station: the library makes a station's row only when it first needs
    /// one, so a configured station may have none yet. An unknown station is refused.
    fn station_row(&self, c: &rusqlite::Connection, station: Option<&str>) -> Result<(), Response> {
        let Some(s) = station.filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let known: bool = c
            .query_row(
                "SELECT COUNT(*) > 0 FROM stations WHERE id = ?1",
                [s],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if known {
            return Ok(());
        }
        match self.stations().into_iter().find(|(n, _, _)| n == s) {
            Some((n, display, _)) => c
                .execute(
                    "INSERT OR IGNORE INTO stations(id, display_name) VALUES(?1, ?2)",
                    params![n, display],
                )
                .map(|_| ())
                .map_err(|e| Response::error(500, "internal", &e.to_string())),
            None => Err(Response::error(422, "invalid", &format!("no station {s}"))),
        }
    }

    pub(crate) fn station_addr(&self, name: &str) -> Option<String> {
        self.stations().into_iter().find(|s| s.0 == name).map(|s| {
            // A station bound to 0.0.0.0 is reached on loopback.
            let (host, port) = s.2.rsplit_once(':').unwrap_or(("127.0.0.1", "0"));
            let host = if host == "0.0.0.0" || host == "[::]" {
                "127.0.0.1"
            } else {
                host
            };
            format!("{host}:{port}")
        })
    }

    // ---- items -------------------------------------------------------------------------------------------------

    fn list_items(&self, req: &Request) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let q = req.query();
        let get = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let mut wh = vec!["i.deleted_at IS NULL".to_string()];
        let mut args: Vec<rusqlite::types::Value> = vec![];
        if let Some(t) = get("q").filter(|t| !t.trim().is_empty()) {
            for word in t.split_whitespace() {
                wh.push(
                    "(i.title LIKE ? OR i.artist LIKE ? OR i.album LIKE ? OR i.notes LIKE ?)"
                        .into(),
                );
                for _ in 0..4 {
                    args.push(format!("%{word}%").into());
                }
            }
        }
        if let Some(c) = get("category").filter(|c| !c.is_empty()) {
            wh.push("(c.name = ? OR CAST(c.id AS TEXT) = ?)".into());
            args.push(c.clone().into());
            args.push(c.into());
        }
        if let Some(t) = get("tag").filter(|t| !t.is_empty()) {
            wh.push("i.id IN (SELECT it.item_id FROM item_tags it JOIN tags t ON t.id = it.tag_id WHERE t.name = ?)".into());
            args.push(t.into());
        }
        if let Some(f) = get("flag").filter(|f| !f.is_empty()) {
            match f.as_str() {
                "quarantined" => wh.push("i.quarantined = 1".into()),
                "disabled" => wh.push("i.enabled = 0".into()),
                "no_art" => wh.push("i.art_sha IS NULL".into()),
                other => {
                    wh.push("e.flags LIKE ?".into());
                    args.push(format!("%\"{other}\"%").into());
                }
            }
        }
        let order = match get("sort").as_deref().unwrap_or("title") {
            "artist" => "i.artist COLLATE NOCASE, i.album COLLATE NOCASE, i.disc_no, i.track_no",
            "album" => "i.album COLLATE NOCASE, i.disc_no, i.track_no, i.title COLLATE NOCASE",
            "imported" => "i.imported_at DESC, i.id DESC",
            "duration" => "i.file_ms DESC",
            "id" => "i.id",
            _ => "i.title COLLATE NOCASE, i.id",
        };
        let per_page: i64 = get("per_page")
            .and_then(|p| p.parse().ok())
            .unwrap_or(50)
            .clamp(1, 500);
        let page: i64 = get("page").and_then(|p| p.parse().ok()).unwrap_or(1).max(1);
        let where_sql = wh.join(" AND ");
        let total: i64 = match lib.conn().query_row(
            &format!("SELECT COUNT(*) {ITEM_FROM} WHERE {where_sql}"),
            rusqlite::params_from_iter(args.iter()),
            |r| r.get(0),
        ) {
            Ok(n) => n,
            Err(e) => return dberr(e),
        };
        let sql = format!(
            "SELECT {ITEM_COLS} {ITEM_FROM} WHERE {where_sql} ORDER BY {order} LIMIT {per_page} OFFSET {}",
            (page - 1) * per_page
        );
        let items: Result<Vec<Value>, _> = lib
            .conn()
            .prepare(&sql)
            .and_then(|mut st| {
                st.query_map(rusqlite::params_from_iter(args.iter()), item_row)
                    .map(|r| r.collect::<Result<Vec<_>, _>>())
            })
            .and_then(|r| r);
        match items {
            Ok(items) => Response::json(
                200,
                &json!({"items": items, "total": total, "page": page, "per_page": per_page}),
            ),
            Err(e) => dberr(e),
        }
    }

    fn item_detail(&self, lib: &Library, id: i64) -> Result<Value, Response> {
        let mut v: Value = lib
            .conn()
            .query_row(
                &format!("SELECT {ITEM_COLS} {ITEM_FROM} WHERE i.id = ?1 AND i.deleted_at IS NULL"),
                [id],
                item_row,
            )
            .optional()
            .map_err(dberr)?
            .ok_or_else(|| not_found(&format!("no item {id}")))?;
        let o = v.as_object_mut().expect("object");
        // Tags.
        let tags: Vec<String> = lib
            .conn()
            .prepare("SELECT t.name FROM item_tags it JOIN tags t ON t.id = it.tag_id WHERE it.item_id = ?1 ORDER BY t.name")
            .and_then(|mut st| st.query_map([id], |r| r.get(0)).map(|r| r.flatten().collect()))
            .map_err(dberr)?;
        o.insert("tags".into(), json!(tags));
        // Analysis (auto values) and overrides.
        let a: Option<Value> = lib
            .conn()
            .query_row(
                "SELECT l_int, lra, true_peak, sample_peak, auto_cue_in, auto_attack, auto_segue, auto_cue_out, auto_intro, flags, algo_version
                 FROM analysis WHERE item_id = ?1",
                [id],
                |r| {
                    Ok(json!({
                        "l_int": r.get::<_, f64>(0)?, "lra": r.get::<_, f64>(1)?, "true_peak": r.get::<_, f64>(2)?,
                        "sample_peak": r.get::<_, f64>(3)?,
                        "auto": {"cue_in": r.get::<_, i64>(4)?, "attack": r.get::<_, i64>(5)?, "segue": r.get::<_, i64>(6)?,
                                 "cue_out": r.get::<_, i64>(7)?, "intro": r.get::<_, Option<i64>>(8)?},
                        "flags": serde_json::from_str::<Value>(&r.get::<_, String>(9)?).unwrap_or(json!([])),
                        "algo_version": r.get::<_, i64>(10)?,
                    }))
                },
            )
            .optional()
            .map_err(dberr)?;
        o.insert("analysis".into(), a.unwrap_or(Value::Null));
        let ov: Option<Value> = lib
            .conn()
            .query_row(
                "SELECT ovr_cue_in, ovr_attack, ovr_intro, ovr_segue, ovr_fade_out, ovr_cue_out, ovr_gain_trim_db, segue_profile, params
                 FROM item_overrides WHERE item_id = ?1",
                [id],
                |r| {
                    Ok(json!({
                        "cue_in": r.get::<_, Option<i64>>(0)?, "attack": r.get::<_, Option<i64>>(1)?,
                        "intro": r.get::<_, Option<i64>>(2)?, "segue": r.get::<_, Option<i64>>(3)?,
                        "fade_out": r.get::<_, Option<i64>>(4)?, "cue_out": r.get::<_, Option<i64>>(5)?,
                        "gain_trim_db": r.get::<_, Option<f64>>(6)?, "segue_profile": r.get::<_, Option<String>>(7)?,
                        "params": r.get::<_, Option<String>>(8)?.and_then(|p| serde_json::from_str::<Value>(&p).ok()),
                    }))
                },
            )
            .optional()
            .map_err(dberr)?;
        o.insert("overrides".into(), ov.unwrap_or(json!({})));
        if let Ok(e) = lib.item_effective(id) {
            o.insert("effective".into(), effective_json(&e));
        }
        let (rs, re) = lib.item_ramps(id).unwrap_or_default();
        o.insert("ramp_start".into(), json!(rs));
        o.insert("ramp_end".into(), json!(re));
        Ok(v)
    }

    fn patch_item(&self, req: &mut Request, id: i64) -> Response {
        let b = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let cur: Option<(String, String, String, String)> = c
            .query_row(
                "SELECT title, artist, album, album_artist FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .unwrap_or(None);
        let Some((mut title, mut artist, mut album, mut album_artist)) = cur else {
            return not_found(&format!("no item {id}"));
        };
        let tx = match c.unchecked_transaction() {
            Ok(t) => t,
            Err(e) => return dberr(e),
        };
        let mut recompute = false;
        let text = |k: &str| {
            b.get(k)
                .and_then(Value::as_str)
                .map(library::util::collapse_ws)
        };
        if let Some(t) = text("title") {
            if t.is_empty() {
                return Response::error(422, "invalid", "the title cannot be empty");
            }
            title = t;
        }
        if let Some(t) = text("artist") {
            artist = t;
        }
        if let Some(t) = text("album") {
            album = t;
        }
        if let Some(t) = text("album_artist") {
            album_artist = t;
        }
        let key = {
            let t = library::tags::ReadTags {
                album: (!album.is_empty()).then(|| album.clone()),
                artist: (!artist.is_empty()).then(|| artist.clone()),
                album_artist: (!album_artist.is_empty()).then(|| album_artist.clone()),
                ..Default::default()
            };
            library::tags::album_key(&t)
        };
        let r = tx.execute(
            "UPDATE items SET title=?2, artist=?3, album=?4, album_artist=?5, album_key=?6, updated_at=?7 WHERE id=?1",
            params![id, title, artist, album, album_artist, key, library::util::now_iso()],
        );
        if let Err(e) = r {
            return dberr(e);
        }
        for (k, col) in [
            ("year", "year"),
            ("disc_no", "disc_no"),
            ("track_no", "track_no"),
            ("track_total", "track_total"),
        ] {
            if let Some(v) = b.get(k) {
                let val: Option<i64> = v.as_i64();
                if let Err(e) = tx.execute(
                    &format!("UPDATE items SET {col} = ?2 WHERE id = ?1"),
                    params![id, val],
                ) {
                    return dberr(e);
                }
            }
        }
        for (k, col) in [("enabled", "enabled"), ("silence_ok", "silence_ok")] {
            if let Some(v) = b.get(k).and_then(Value::as_bool) {
                if let Err(e) = tx.execute(
                    &format!("UPDATE items SET {col} = ?2 WHERE id = ?1"),
                    params![id, i64::from(v)],
                ) {
                    return dberr(e);
                }
            }
        }
        if let Some(n) = b.get("notes").and_then(Value::as_str) {
            let _ = tx.execute("UPDATE items SET notes = ?2 WHERE id = ?1", params![id, n]);
        }
        if let Some(cat) = b.get("category") {
            let cid: Option<i64> = match cat {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => tx
                    .query_row("SELECT id FROM categories WHERE name = ?1", [s], |r| {
                        r.get(0)
                    })
                    .optional()
                    .unwrap_or(None),
                _ => None,
            };
            match cid {
                Some(cid) => {
                    let _ = tx.execute(
                        "UPDATE items SET category_id = ?2 WHERE id = ?1",
                        params![id, cid],
                    );
                    recompute = true;
                }
                None => return Response::error(422, "invalid", "no such category"),
            }
        }
        if let Some(tags) = b.get("tags").and_then(Value::as_array) {
            let _ = tx.execute("DELETE FROM item_tags WHERE item_id = ?1", [id]);
            for t in tags.iter().filter_map(Value::as_str) {
                let t = library::util::collapse_ws(t).to_lowercase();
                if t.is_empty() {
                    continue;
                }
                let _ = tx.execute("INSERT OR IGNORE INTO tags(name) VALUES(?1)", [&t]);
                let _ = tx.execute(
                    "INSERT OR IGNORE INTO item_tags(item_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
                    params![id, t],
                );
            }
        }
        if let Err(e) = tx.commit() {
            return dberr(e);
        }
        if recompute {
            if let Err(e) = lib.recompute_effective(id) {
                return lerr(e);
            }
        }
        let _ = Library::bump_revision(lib.conn());
        self.audit(lib.conn(), req, "item", &id.to_string(), &b);
        match self.item_detail(&lib, id) {
            Ok(v) => Response::json(200, &v),
            Err(r) => r,
        }
    }

    fn put_overrides(&self, req: &mut Request, id: i64) -> Response {
        let b = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let patch = OverridePatch {
            cue_in: opt_patch(&b, "cue_in"),
            attack: opt_patch(&b, "attack"),
            intro: opt_patch(&b, "intro"),
            segue: opt_patch(&b, "segue"),
            fade_out: opt_patch(&b, "fade_out"),
            fade_out_before_end: opt_patch(&b, "fade_out_before_end_ms"),
            cue_out: opt_patch(&b, "cue_out"),
            gain_trim_db: b.get("gain_trim_db").map(Value::as_f64),
            segue_profile: b
                .get("segue_profile")
                .map(|v| v.as_str().map(str::to_string)),
            params: b
                .get("params")
                .map(|v| if v.is_null() { None } else { Some(v.clone()) }),
        };
        match lib.set_overrides_noted(id, &patch) {
            Ok((e, notes)) => {
                let _ = Library::bump_revision(lib.conn());
                self.audit(lib.conn(), req, "item_overrides", &id.to_string(), &b);
                Response::json(
                    200,
                    &json!({"effective": effective_json(&e), "notes": notes}),
                )
            }
            Err(e) => lerr(e),
        }
    }

    fn delete_overrides(&self, req: &Request, id: i64) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let patch = OverridePatch {
            cue_in: Some(None),
            attack: Some(None),
            intro: Some(None),
            segue: Some(None),
            fade_out: Some(None),
            fade_out_before_end: None,
            cue_out: Some(None),
            gain_trim_db: Some(None),
            segue_profile: Some(None),
            params: Some(None),
        };
        match lib.set_overrides(id, &patch) {
            Ok(e) => {
                let _ = Library::bump_revision(lib.conn());
                self.audit(
                    lib.conn(),
                    req,
                    "item_overrides",
                    &id.to_string(),
                    &json!({"cleared": true}),
                );
                Response::json(200, &json!({"effective": effective_json(&e)}))
            }
            Err(e) => lerr(e),
        }
    }

    /// Is the item on air or committed on any station? Asks every station's `/state`.
    fn item_in_use(&self, id: i64) -> Option<String> {
        for (name, _, _) in self.stations() {
            let Some(addr) = self.station_addr(&name) else {
                continue;
            };
            let Ok(r) = client::request(
                &addr,
                "GET",
                "/api/v1/state",
                &[],
                b"",
                Duration::from_millis(800),
            ) else {
                continue;
            };
            let s = r.json();
            if s["now"]["item_id"].as_i64() == Some(id) {
                return Some(format!("it is on air on `{name}`"));
            }
            if s["queue"].as_array().is_some_and(|q| {
                q.iter()
                    .any(|e| e["item_id"].as_i64() == Some(id) && e["committed"] == true)
            }) {
                return Some(format!("it is committed to play next on `{name}`"));
            }
        }
        None
    }

    /// `POST /items/{id}/release`: release an item from quarantine. The library row is cleared (resetting
    /// `playback_errors`), and every station is told to release it too (a station may hold its own forced
    /// quarantine). Answers which of the two happened, and how each station answered.
    fn release_item(&self, req: &Request, id: i64) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let exists: bool = lib
            .conn()
            .query_row(
                "SELECT 1 FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);
        if !exists {
            return not_found(&format!("no item {id}"));
        }
        let was_quarantined = match library::health::release(lib.conn(), id) {
            Ok(b) => b,
            Err(e) => return lerr(e),
        };
        self.audit(
            lib.conn(),
            req,
            "item",
            &id.to_string(),
            &json!({"release": true}),
        );
        let mut stations = serde_json::Map::new();
        for (name, _, _) in self.stations() {
            let Some(addr) = self.station_addr(&name) else {
                continue;
            };
            let r = client::request(
                &addr,
                "POST",
                &format!("/api/v1/items/{id}/release"),
                &[("Content-Type", "application/json")],
                b"{}",
                Duration::from_secs(5),
            );
            stations.insert(
                name,
                match r {
                    Ok(r) if r.status < 300 => json!("released"),
                    Ok(r) => json!(format!("refused ({})", r.status)),
                    Err(_) => json!("not answering"),
                },
            );
        }
        Response::json(
            200,
            &json!({"id": id, "library": if was_quarantined { "released" } else { "was not quarantined" }, "stations": stations}),
        )
    }

    fn delete_item(&self, req: &Request, id: i64) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let sp: Option<String> = lib
            .conn()
            .query_row(
                "SELECT store_path FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        let Some(sp) = sp else {
            return not_found(&format!("no item {id}"));
        };
        if let Some(why) = self.item_in_use(id) {
            return conflict(&format!("item {id} cannot be deleted while {why}"));
        }
        // The file moves to `.trash/` (it can be brought back by hand); the row keeps its history.
        let src = self.opts.paths.media.join(&sp);
        let trash = self.opts.paths.media.join(".trash");
        let _ = std::fs::create_dir_all(&trash);
        if let Some(name) = src.file_name() {
            let _ = std::fs::rename(&src, trash.join(name));
        }
        if let Err(e) = lib.conn().execute(
            "UPDATE items SET deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![id, library::util::now_iso()],
        ) {
            return dberr(e);
        }
        let _ = Library::bump_revision(lib.conn());
        self.audit(
            lib.conn(),
            req,
            "item",
            &id.to_string(),
            &json!({"deleted": true}),
        );
        Response::new(204)
    }

    fn waveform(&self, id: i64) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let row: Option<WaveRow> = lib
            .conn()
            .query_row(
                "SELECT a.wave, a.env_k200, i.channels, COALESCE(o.segue_profile, c.segue_profile), a.l_int, i.file_ms
                 FROM analysis a JOIN items i ON i.id = a.item_id JOIN categories c ON c.id = i.category_id
                 LEFT JOIN item_overrides o ON o.item_id = i.id WHERE a.item_id = ?1 AND i.deleted_at IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .unwrap_or(None);
        let Some((wave, k200, channels, profile, l_int, file_ms)) = row else {
            return not_found(&format!("no analysed item {id}"));
        };
        let raw = zstd_decode(&wave);
        let ch = channels.max(1) as usize;
        let cols = raw.len() / (2 * ch);
        let (mut mins, mut maxs) = (Vec::with_capacity(cols), Vec::with_capacity(cols));
        for w in 0..cols {
            let (mut lo, mut hi) = (127i8, -128i8);
            for c in 0..ch {
                let i = (w * ch + c) * 2;
                lo = lo.min(raw[i] as i8);
                hi = hi.max(raw[i + 1] as i8);
            }
            mins.push(lo);
            maxs.push(hi);
        }
        let k = analysis::env::unpack(&k200).unwrap_or_default();
        let params: Value = lib
            .conn()
            .query_row(
                "SELECT params FROM segue_profiles WHERE name = ?1",
                [&profile],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|p| serde_json::from_str(&p).ok())
            .unwrap_or(Value::Null);
        Response::json(
            200,
            &json!({
                "hop_ms": 20, "file_ms": file_ms, "channels": channels, "min": mins, "max": maxs,
                "k200_db": k.iter().map(|v| f64::from(*v) / 10.0).collect::<Vec<_>>(), "k200_hop_ms": 20,
                "l_int": l_int, "profile": profile, "profile_params": params,
            }),
        )
    }

    fn audio(&self, req: &Request, id: i64) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let row: Option<(String, String)> = lib
            .conn()
            .query_row(
                "SELECT store_path, format FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap_or(None);
        let Some((sp, fmt)) = row else {
            return not_found(&format!("no item {id}"));
        };
        let path = self.opts.paths.media.join(sp);
        let Ok(mut f) = std::fs::File::open(&path) else {
            return not_found("the stored file is missing");
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let ct = match fmt.as_str() {
            "flac" => "audio/flac",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "aac" | "alac" => "audio/mp4",
            "vorbis" => "audio/ogg",
            _ => "application/octet-stream",
        };
        // One range at most, and never more than 8 MB per answer: the browser asks again.
        let (start, end, partial) = match req.header("range").and_then(|r| r.strip_prefix("bytes="))
        {
            Some(spec) => {
                let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
                let (s, e) = match (a.parse::<u64>(), b.parse::<u64>()) {
                    (Ok(s), Ok(e)) => (s, e),
                    (Ok(s), Err(_)) => (s, len.saturating_sub(1)),
                    (Err(_), Ok(n)) => (len.saturating_sub(n), len.saturating_sub(1)),
                    _ => return bad("bad Range header"),
                };
                if s >= len || e < s {
                    return Response::new(416)
                        .with_header("content-range", &format!("bytes */{len}"));
                }
                (s, e.min(len - 1).min(s + (8 << 20) - 1), true)
            }
            None => (0, len.saturating_sub(1).min((64 << 20) - 1), false),
        };
        let mut buf = vec![0u8; (end - start + 1) as usize];
        use std::io::Seek;
        if f.seek(std::io::SeekFrom::Start(start)).is_err() || f.read_exact(&mut buf).is_err() {
            return Response::error(500, "internal", "cannot read the stored file");
        }
        let mut r = Response::text(if partial { 206 } else { 200 }, ct, buf)
            .with_header("accept-ranges", "bytes");
        if partial {
            r = r.with_header("content-range", &format!("bytes {start}-{end}/{len}"));
        }
        r
    }

    fn put_art(&self, req: &mut Request, id: i64) -> Response {
        let bytes = match req.body_bytes(20 * 1024 * 1024) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let exists: bool = lib
            .conn()
            .query_row(
                "SELECT 1 FROM items WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |_| Ok(true),
            )
            .optional()
            .unwrap_or(None)
            .unwrap_or(false);
        if !exists {
            return not_found(&format!("no item {id}"));
        }
        let src = library::art::ArtSource {
            bytes,
            origin: "manual",
        };
        let n = match library::art::store_art(
            &self.opts.paths.media,
            &self.opts.paths.upload_tmp,
            &src,
        ) {
            Ok(n) => n,
            Err(e) => return Response::error(422, "invalid", &e),
        };
        let tx = match lib.conn().unchecked_transaction() {
            Ok(t) => t,
            Err(e) => return dberr(e),
        };
        let _ = tx.execute(
            "INSERT OR IGNORE INTO artwork(sha256, origin, src_w, src_h, bytes_600, bytes_144) VALUES(?1,?2,?3,?4,?5,?6)",
            params![n.sha, n.origin, n.src_w, n.src_h, n.bytes_600 as i64, n.bytes_144 as i64],
        );
        let _ = tx.execute(
            "UPDATE items SET art_sha = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, n.sha, library::util::now_iso()],
        );
        if let Err(e) = tx.commit() {
            return dberr(e);
        }
        let _ = Library::bump_revision(lib.conn());
        self.audit(
            lib.conn(),
            req,
            "item_art",
            &id.to_string(),
            &json!({"sha": n.sha}),
        );
        Response::json(
            200,
            &json!({"art_sha": n.sha, "art_url": format!("/api/v1/items/{id}/art?size=600")}),
        )
    }

    // ---- albums ------------------------------------------------------------------------------------------------

    fn albums(&self, req: &Request, key: Option<&str>) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        if let Some(key) = key {
            let tracks: Result<Vec<Value>, _> = lib
                .conn()
                .prepare(&format!(
                    "SELECT {ITEM_COLS} {ITEM_FROM} WHERE i.album_key = ?1 AND i.deleted_at IS NULL
                     ORDER BY COALESCE(i.disc_no, 1), COALESCE(i.track_no, 9999), i.id"
                ))
                .and_then(|mut st| {
                    st.query_map([key], item_row)
                        .map(|r| r.collect::<Result<Vec<_>, _>>())
                })
                .and_then(|r| r);
            let tracks = match tracks {
                Ok(t) => t,
                Err(e) => return dberr(e),
            };
            if tracks.is_empty() {
                return not_found("no such album");
            }
            // The adjacency the scheduler will use (3.7): a track follows the previous one when both have
            // numbers and they are consecutive on the same disc.
            let mut out = vec![];
            for (i, t) in tracks.iter().enumerate() {
                let adj = i > 0 && {
                    let p = &tracks[i - 1];
                    p["disc_no"].as_i64().unwrap_or(1) == t["disc_no"].as_i64().unwrap_or(1)
                        && t["track_no"]
                            .as_i64()
                            .zip(p["track_no"].as_i64())
                            .is_some_and(|(a, b)| a == b + 1)
                };
                let mut t = t.clone();
                t.as_object_mut()
                    .expect("object")
                    .insert("adjacent_to_previous".into(), json!(adj));
                out.push(t);
            }
            return Response::json(200, &json!({"album_key": key, "tracks": out}));
        }
        let q = req.query_get("q").unwrap_or_default();
        let like = format!("%{q}%");
        let rows: Result<Vec<Value>, _> = lib
            .conn()
            .prepare(
                "SELECT album_key, MIN(album), MIN(album_artist), MIN(artist), COUNT(*), SUM(file_ms), MIN(art_sha), MIN(id)
                 FROM items WHERE deleted_at IS NULL AND album_key <> '' AND (album LIKE ?1 OR artist LIKE ?1 OR album_artist LIKE ?1)
                 GROUP BY album_key ORDER BY MIN(album) COLLATE NOCASE LIMIT 500",
            )
            .and_then(|mut st| {
                st.query_map([&like], |r| {
                    let art: Option<String> = r.get(6)?;
                    let first: i64 = r.get(7)?;
                    Ok(json!({
                        "album_key": r.get::<_, String>(0)?, "album": r.get::<_, String>(1)?,
                        "album_artist": r.get::<_, String>(2)?, "artist": r.get::<_, String>(3)?,
                        "tracks": r.get::<_, i64>(4)?, "duration_ms": r.get::<_, i64>(5)?,
                        "art_url": art.map(|_| format!("/api/v1/items/{first}/art?size=144")), "first_item_id": first,
                    }))
                })
                .map(|r| r.collect::<Result<Vec<_>, _>>())
            })
            .and_then(|r| r);
        match rows {
            Ok(a) => Response::json(200, &json!({"albums": a})),
            Err(e) => dberr(e),
        }
    }

    // ---- imports -------------------------------------------------------------------------------------------------

    fn upload(&self, req: &mut Request) -> Response {
        let Some(boundary) = req.header("content-type").and_then(boundary_of) else {
            return bad("the upload has no multipart boundary");
        };
        if req.content_length > UPLOAD_REQUEST_LIMIT + (1 << 20) {
            return Response::error(
                413,
                "too_large",
                "one upload request may carry 16 GB in all (each file may be 1 GB); send fewer files at a time",
            );
        }
        // The upload is written to upload-tmp and then copied into the store: refuse it up front when the
        // disk would be left with less than UPLOAD_RESERVE, rather than fill the disk the stations' state and
        // logs live on.
        if let Some(free) = disk_free(&self.opts.paths.upload_tmp) {
            if free
                < req
                    .content_length
                    .saturating_mul(2)
                    .saturating_add(UPLOAD_RESERVE)
            {
                return Response::error(
                    507,
                    "insufficient_storage",
                    &format!(
                        "not enough disk space for this upload ({} MB free, {} MB needed with the reserve)",
                        free >> 20,
                        (req.content_length.saturating_mul(2) + UPLOAD_RESERVE) >> 20
                    ),
                );
            }
        }
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let mut category: Option<String> = None;
        let mut jobs: Vec<(i64, String, PathBuf)> = vec![];
        // A failure part-way (a client that went away, a part over the limit) must not leave the files
        // already received in upload-tmp, or their jobs `queued` for ever.
        let abandon = |jobs: &[(i64, String, PathBuf)], why: &str| {
            for (job, _, tmp) in jobs {
                let _ = std::fs::remove_file(tmp);
                let _ = lib.conn().execute(
                    "UPDATE import_jobs SET status='failed', error=?2, finished_at=?3 WHERE id = ?1",
                    params![job, why, library::util::now_iso()],
                );
            }
        };
        let mut mp = Multipart::new(req.body_reader(), &boundary);
        loop {
            let part = match mp.next_part() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(e) => {
                    let msg = format!("reading the upload: {e}");
                    abandon(&jobs, &msg);
                    return bad(&msg);
                }
            };
            match part.filename {
                None if part.name == "category" => {
                    category = mp.text().ok().filter(|c| !c.trim().is_empty())
                }
                None => {
                    let _ = mp.text();
                }
                Some(fname) => {
                    // The client's file name is a label, never a path (13).
                    let label: String = fname
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or("upload")
                        .chars()
                        .take(200)
                        .collect();
                    let tmp = library::store::tmp_name(&self.opts.paths.upload_tmp, "up");
                    let written = std::fs::File::create(&tmp)
                        .map_err(|e| e.to_string())
                        .and_then(|mut f| {
                            mp.body_to(&mut f, UPLOAD_LIMIT).map_err(|e| e.to_string())
                        });
                    if let Err(e) = written {
                        let _ = std::fs::remove_file(&tmp);
                        let msg = format!("{label}: {e}");
                        abandon(&jobs, &format!("the upload was not completed ({msg})"));
                        return if e.contains("too large") {
                            Response::error(
                                413,
                                "too_large",
                                &format!("{msg} (a file may be 1 GB)"),
                            )
                        } else {
                            bad(&msg)
                        };
                    }
                    let _ = lib.conn().execute(
                        "INSERT INTO import_jobs(source, status, created_at) VALUES(?1,'queued',?2)",
                        params![label, library::util::now_iso()],
                    );
                    let job = lib.conn().last_insert_rowid();
                    jobs.push((job, label, tmp));
                }
            }
        }
        if jobs.is_empty() {
            return bad("the upload holds no file");
        }
        let out: Vec<Value> = jobs
            .iter()
            .map(|(id, l, _)| json!({"id": id, "filename": l, "status": "queued"}))
            .collect();
        for (job, _label, tmp) in jobs {
            let paths = self.opts.paths.clone();
            let slots = self.slots.clone();
            let cat = category.clone();
            std::thread::spawn(move || run_import(paths, slots, job, tmp, cat));
        }
        Response::json(202, &json!({"jobs": out}))
    }

    fn import_jobs(&self) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let rows: Result<Vec<Value>, _> = lib
            .conn()
            .prepare("SELECT id, source, status, item_id, error, created_at, finished_at FROM import_jobs
                 WHERE source NOT LIKE 'upload:%' ORDER BY id DESC LIMIT 200")
            .and_then(|mut st| {
                st.query_map([], |r| {
                    Ok(json!({"id": r.get::<_, i64>(0)?, "source": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                              "item_id": r.get::<_, Option<i64>>(3)?, "error": r.get::<_, Option<String>>(4)?,
                              "created_at": r.get::<_, String>(5)?, "finished_at": r.get::<_, Option<String>>(6)?}))
                })
                .map(|r| r.collect::<Result<Vec<_>, _>>())
            })
            .and_then(|r| r);
        match rows {
            Ok(j) => Response::json(200, &json!({"jobs": j})),
            Err(e) => dberr(e),
        }
    }

    /// `GET /api/v1/reanalyze`: how many items an older version of the analysis made (`stale`), of how many.
    fn reanalyze_status(&self) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let total: i64 = lib
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        match lib.stale_analysis_ids() {
            Ok(ids) => Response::json(
                200,
                &json!({"algo_version": analysis::analyze::ALGO_VERSION, "stale": ids.len(), "items": total}),
            ),
            Err(e) => lerr(e),
        }
    }

    fn reanalyze(&self, req: &mut Request) -> Response {
        let b = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let ids: Vec<i64> = if b["stale"] == true {
            match lib.stale_analysis_ids() {
                Ok(ids) => ids,
                Err(e) => return lerr(e),
            }
        } else if b["all"] == true {
            lib.conn()
                .prepare("SELECT id FROM items WHERE deleted_at IS NULL ORDER BY id")
                .and_then(|mut st| {
                    st.query_map([], |r| r.get(0))
                        .map(|r| r.flatten().collect())
                })
                .unwrap_or_default()
        } else {
            b["item_ids"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default()
        };
        if ids.is_empty() {
            if b["stale"] == true {
                return Response::json(202, &json!({"queued": 0}));
            }
            return bad("give item_ids, all: true or stale: true");
        }
        let n = ids.len();
        let paths = self.opts.paths.clone();
        std::thread::spawn(move || {
            import_lower();
            if let Ok(l) = Library::open_worker(paths) {
                for id in ids {
                    match l.reanalyze_item(id, None) {
                        Ok(_) => {}
                        Err(e) => tracing::warn!("reanalyze {id}: {e}"),
                    }
                }
            }
        });
        Response::json(202, &json!({"queued": n}))
    }
}

fn import_lower() {
    library::import::lower_priority();
}

fn zstd_decode(b: &[u8]) -> Vec<u8> {
    zstd::decode_all(b).unwrap_or_default()
}

fn run_import(
    paths: Paths,
    slots: Arc<(Mutex<usize>, Condvar)>,
    job: i64,
    tmp: PathBuf,
    category: Option<String>,
) {
    // At most two imports run at once.
    {
        let (m, cv) = &*slots;
        let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= IMPORT_WORKERS {
            n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n += 1;
    }
    import_lower();
    let result = Library::open_worker(paths).map(|lib| {
        let _ = lib.conn().execute(
            "UPDATE import_jobs SET status='running' WHERE id = ?1",
            [job],
        );
        let out = library::import_file(
            &lib,
            &tmp,
            &ImportOptions {
                category,
                source: "upload".into(),
            },
        );
        let (status, item, error) = match out {
            ImportOutcome::Done {
                item_id, warnings, ..
            } => (
                "done",
                Some(item_id),
                (!warnings.is_empty()).then(|| warnings.join("; ")),
            ),
            ImportOutcome::Duplicate { item_id } => ("duplicate", Some(item_id), None),
            ImportOutcome::Failed { error } => ("failed", None, Some(error)),
        };
        let _ = lib.conn().execute(
            "UPDATE import_jobs SET status=?2, item_id=?3, error=?4, finished_at=?5 WHERE id = ?1",
            params![job, status, item, error, library::util::now_iso()],
        );
        // The pipeline keeps a job row of its own, named after the temporary file: this job (named after the
        // uploaded file) replaces it.
        let _ = lib.conn().execute(
            "DELETE FROM import_jobs WHERE source = ?1",
            [format!("upload:{}", tmp.display())],
        );
    });
    if let Err(e) = result {
        tracing::error!("import job {job}: {e}");
    }
    let _ = std::fs::remove_file(&tmp);
    let (m, cv) = &*slots;
    *m.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
    cv.notify_one();
}

// ---- programming: categories, tags, playlists ---------------------------------------------------------------------

impl LibraryApi {
    fn categories(&self, req: &mut Request, segs: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let cat_json = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
            Ok(
                json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?,
                      "segue_profile": r.get::<_, String>(3)?, "transition_preset": r.get::<_, Option<String>>(4)?,
                      "transition_patch": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(json!({})),
                      "gain_offset_db": r.get::<_, f64>(6)?, "color": r.get::<_, Option<String>>(7)?,
                      "items": r.get::<_, i64>(8)?}),
            )
        };
        const SEL: &str = "SELECT c.id, c.name, c.kind, c.segue_profile, c.transition_preset, c.transition_patch, c.gain_offset_db, c.color,
            (SELECT COUNT(*) FROM items i WHERE i.category_id = c.id AND i.deleted_at IS NULL) FROM categories c";
        match (req.method.as_str(), segs) {
            ("GET", []) => {
                let rows: Result<Vec<Value>, _> = c
                    .prepare(&format!("{SEL} ORDER BY c.id"))
                    .and_then(|mut st| {
                        st.query_map([], cat_json)
                            .map(|r| r.collect::<Result<Vec<_>, _>>())
                    })
                    .and_then(|r| r);
                match rows {
                    Ok(r) => Response::json(200, &json!({"categories": r})),
                    Err(e) => dberr(e),
                }
            }
            ("POST", []) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let Some(name) = b["name"].as_str().filter(|n| !n.trim().is_empty()) else {
                    return bad("`name` is required");
                };
                let r = c.execute(
                    "INSERT INTO categories(name, kind, segue_profile, transition_preset, transition_patch, gain_offset_db, color)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        name.trim(),
                        b["kind"].as_str().unwrap_or("other"),
                        b["segue_profile"].as_str().unwrap_or("music"),
                        b["transition_preset"].as_str(),
                        b.get("transition_patch").map_or("{}".to_string(), Value::to_string),
                        b["gain_offset_db"].as_f64().unwrap_or(0.0),
                        b["color"].as_str()
                    ],
                );
                match r {
                    Ok(_) => {
                        let id = c.last_insert_rowid();
                        let _ = Library::bump_revision(c);
                        self.audit(c, req, "category", &id.to_string(), &b);
                        c.query_row(&format!("{SEL} WHERE c.id = ?1"), [id], cat_json)
                            .map(|v| Response::json(201, &v))
                            .unwrap_or_else(dberr)
                    }
                    Err(e) => dberr(e),
                }
            }
            ("PATCH", [id]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                for (k, col) in [
                    ("name", "name"),
                    ("kind", "kind"),
                    ("segue_profile", "segue_profile"),
                    ("color", "color"),
                ] {
                    if let Some(v) = b.get(k) {
                        if let Err(e) = c.execute(
                            &format!("UPDATE categories SET {col} = ?2 WHERE id = ?1"),
                            params![id, v.as_str()],
                        ) {
                            return dberr(e);
                        }
                    }
                }
                if let Some(v) = b.get("transition_preset") {
                    let _ = c.execute(
                        "UPDATE categories SET transition_preset = ?2 WHERE id = ?1",
                        params![id, v.as_str()],
                    );
                }
                if let Some(v) = b.get("transition_patch") {
                    let _ = c.execute(
                        "UPDATE categories SET transition_patch = ?2 WHERE id = ?1",
                        params![id, v.to_string()],
                    );
                }
                if let Some(v) = b["gain_offset_db"].as_f64() {
                    let _ = c.execute(
                        "UPDATE categories SET gain_offset_db = ?2 WHERE id = ?1",
                        params![id, v],
                    );
                }
                // The effective rows of the category's items depend on it.
                let ids: Vec<i64> = c
                    .prepare("SELECT id FROM items WHERE category_id = ?1 AND deleted_at IS NULL")
                    .and_then(|mut st| {
                        st.query_map([id], |r| r.get(0))
                            .map(|r| r.flatten().collect())
                    })
                    .unwrap_or_default();
                for i in ids {
                    let _ = lib.recompute_effective(i);
                }
                let _ = Library::bump_revision(c);
                self.audit(c, req, "category", &id.to_string(), &b);
                c.query_row(&format!("{SEL} WHERE c.id = ?1"), [id], cat_json)
                    .optional()
                    .map(|v| {
                        v.map_or_else(
                            || not_found("no such category"),
                            |v| Response::json(200, &v),
                        )
                    })
                    .unwrap_or_else(dberr)
            }
            ("DELETE", [id]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                let n: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM items WHERE category_id = ?1 AND deleted_at IS NULL",
                        [id],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if n > 0 {
                    return conflict(&format!(
                        "{n} item(s) are in this category; move them first"
                    ));
                }
                match c.execute("DELETE FROM categories WHERE id = ?1", [id]) {
                    Ok(0) => not_found("no such category"),
                    Ok(_) => {
                        let _ = Library::bump_revision(c);
                        Response::new(204)
                    }
                    Err(e) => dberr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    fn tags(&self, req: &mut Request, segs: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        match (req.method.as_str(), segs) {
            ("GET", []) => {
                let rows: Result<Vec<Value>, _> = c
                    .prepare("SELECT t.id, t.name, (SELECT COUNT(*) FROM item_tags it WHERE it.tag_id = t.id) FROM tags t ORDER BY t.name")
                    .and_then(|mut st| {
                        st.query_map([], |r| Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "items": r.get::<_, i64>(2)?})))
                            .map(|r| r.collect::<Result<Vec<_>, _>>())
                    })
                    .and_then(|r| r);
                rows.map(|r| Response::json(200, &json!({"tags": r})))
                    .unwrap_or_else(dberr)
            }
            ("POST", []) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let Some(n) = b["name"]
                    .as_str()
                    .map(|n| library::util::collapse_ws(n).to_lowercase())
                    .filter(|n| !n.is_empty())
                else {
                    return bad("`name` is required");
                };
                match c.execute("INSERT INTO tags(name) VALUES(?1)", [&n]) {
                    Ok(_) => Response::json(201, &json!({"id": c.last_insert_rowid(), "name": n})),
                    Err(e) => dberr(e),
                }
            }
            ("PATCH", [id]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                let Some(n) = b["name"]
                    .as_str()
                    .map(|n| library::util::collapse_ws(n).to_lowercase())
                else {
                    return bad("`name` is required");
                };
                match c.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, n]) {
                    Ok(0) => not_found("no such tag"),
                    Ok(_) => Response::json(200, &json!({"id": id, "name": n})),
                    Err(e) => dberr(e),
                }
            }
            ("DELETE", [id]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match c.execute("DELETE FROM tags WHERE id = ?1", [id]) {
                    Ok(0) => not_found("no such tag"),
                    Ok(_) => Response::new(204),
                    Err(e) => dberr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    /// The validator of 4.4 over the whole programming, for one playlist and the station blocks of the
    /// station it belongs to.
    /// `GET /playlists/{id}/preview?t0=&now=`: the dry run (4.9). `t0` is `now`, `nominal`, an RFC 3339 time,
    /// or `HH:MM` (today, local). Nothing fires.
    fn playlist_preview(&self, c: &rusqlite::Connection, req: &Request, id: i64) -> Response {
        let rows = match library::playlists::read_playlists(c) {
            Ok(r) => r,
            Err(e) => return lerr(e),
        };
        let station: Option<String> = c
            .query_row(
                "SELECT station_id FROM playlists WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let sblocks = match &station {
            Some(st) => library::playlists::read_station_blocks(c, st).unwrap_or_default(),
            None => vec![],
        };
        let prog = station::programming::Programming::from_rows(&rows, &sblocks);
        if !prog.playlists.contains_key(&id) {
            return not_found("no such playlist");
        }
        let zone = common::timespec::Zone::named(&self.opts.time_zone)
            .unwrap_or_else(|_| common::timespec::Zone::utc());
        let now = crate::station_api::wall_ms();
        let spec = req.query_get("t0").unwrap_or_else(|| "now".into());
        let t0 = match spec.as_str() {
            "now" => now,
            "nominal" => {
                let occ = station::preview::nominal_occurrences(
                    &prog,
                    id,
                    &zone,
                    now - 86_400_000,
                    now + 8 * 86_400_000,
                );
                match occ
                    .iter()
                    .rev()
                    .find(|(_, t)| *t <= now)
                    .or_else(|| occ.first())
                {
                    Some((_, t)) => *t,
                    None => return bad("this playlist has no scheduled occurrence"),
                }
            }
            other => match parse_t0(other, &zone, now) {
                Some(t) => t,
                None => return bad("t0 is `now`, `nominal`, an RFC 3339 time, or HH:MM"),
            },
        };
        // "now" for the list of blocks already past: the request's own, or the real one.
        let at = req
            .query_get("now")
            .and_then(|n| {
                if n == "now" {
                    Some(now)
                } else {
                    parse_t0(&n, &zone, now)
                }
            })
            .unwrap_or(now);
        let Some(pv) = station::preview::dry_run(&prog, id, t0, Some(at), &zone) else {
            return not_found("no such playlist");
        };
        let mut body = pv.to_json(&zone);
        let mut problems = self.validate_playlist(c, id).unwrap_or_default();
        if let Some(w) = body["warnings"].as_array() {
            problems.extend(w.iter().cloned());
        }
        for g in &pv.gaps {
            let _ = g;
        }
        body["problems"] = Value::Array(problems);
        body["t0"] = json!(zone.local_string(t0));
        body["t0_ms"] = json!(t0);
        body["now"] = json!(zone.local_string(at));
        body["time_zone"] = json!(zone.name());
        Response::json(200, &body)
    }

    /// `POST /examples/{name}`: create an example program from the playlists the caller names (9).
    fn create_example(&self, req: &mut Request, name: &str) -> Response {
        if name != "ita_monday" {
            return not_found("no such example");
        }
        let b = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let (Some(pre), Some(bed)) = (
            b["pre_show_playlist_id"].as_i64(),
            b["elevator_playlist_id"].as_i64(),
        ) else {
            return bad("give pre_show_playlist_id and elevator_playlist_id");
        };
        let osc = library::examples::OscTarget {
            host: b["osc_host"]
                .as_str()
                .unwrap_or("192.0.2.100")
                .to_string(),
            port: b["osc_port"]
                .as_u64()
                .filter(|p| (1..=65535).contains(p))
                .unwrap_or(2223) as u16,
            address: b["osc_address"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("/ch/{}/mute", b["channel"].as_u64().unwrap_or(2))),
            value: b["osc_value"].as_i64().unwrap_or(1) as i32,
        };
        let mut ex = library::examples::ItaMonday::new(pre, bed, osc);
        if let Some(n) = b["name"].as_str().filter(|n| !n.trim().is_empty()) {
            ex.name = n.trim().to_string();
        }
        ex.station_id = b["station_id"].as_str().map(str::to_string);
        if let Err(r) = self.station_row(lib.conn(), ex.station_id.as_deref()) {
            return r;
        }
        if let Some(c) = b["expected_cron"].as_str() {
            ex.expected_cron = c.to_string();
        }
        match ex.create(&lib) {
            Ok(id) => {
                self.audit(lib.conn(), req, "playlist_example", &id.to_string(), &b);
                self.playlist_json(&lib, id)
                    .map(|v| Response::json(201, &v))
                    .unwrap_or_else(|r| r)
            }
            Err(e) => lerr(e),
        }
    }

    fn validate_playlist(&self, c: &rusqlite::Connection, id: i64) -> Result<Vec<Value>, Response> {
        let rows = library::playlists::read_playlists(c).map_err(lerr)?;
        let station: Option<String> = c
            .query_row(
                "SELECT station_id FROM playlists WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let sblocks = match &station {
            Some(st) => library::playlists::read_station_blocks(c, st).map_err(lerr)?,
            None => vec![],
        };
        let prog = station::programming::Programming::from_rows(&rows, &sblocks);
        let zone = common::timespec::Zone::named(&self.opts.time_zone).ok();
        let problems = prog
            .validate_with(&DbRefs(c), zone.as_ref())
            .into_iter()
            .filter(|p| p.playlist == Some(id) || (p.playlist.is_none() && station.is_some()))
            .map(|p| {
                json!({
                    "severity": if p.error { "error" } else { "warning" },
                    "code": p.code, "playlist": p.playlist, "block": p.block, "message": p.message,
                })
            })
            .collect();
        Ok(problems)
    }

    /// `GET /schedule/week?station=`: the scheduled starts of the station's playlists drawn on a week
    /// (the Schedule screen's grid, 9), the station blocks, and the next occurrences.
    fn schedule_week(&self, req: &Request) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let station = req.query_get("station");
        let rows = match library::playlists::read_playlists(c) {
            Ok(r) => r,
            Err(e) => return lerr(e),
        };
        let sblocks = match &station {
            Some(st) => library::playlists::read_station_blocks(c, st).unwrap_or_default(),
            None => vec![],
        };
        let prog = station::programming::Programming::from_rows(&rows, &sblocks);
        let zone = common::timespec::Zone::named(&self.opts.time_zone)
            .unwrap_or_else(|_| common::timespec::Zone::utc());
        let mut starts = vec![];
        for (pid, b, sb) in prog.scheduled_starts() {
            let owner: Option<String> = c
                .query_row(
                    "SELECT station_id FROM playlists WHERE id = ?1",
                    [pid],
                    |r| r.get(0),
                )
                .unwrap_or(None);
            if let (Some(want), Some(have)) = (&station, &owner) {
                if want != have {
                    continue;
                }
            }
            let name = prog
                .playlists
                .get(&pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let mut slots = vec![];
            let mut dates = vec![];
            for spec in &sb.schedule {
                match &spec.when {
                    common::timespec::When::Recurrence { cron } => {
                        let days: Vec<u8> = cron.weekdays().unwrap_or_else(|| (0..7).collect());
                        for (h, m) in cron.times_of_day() {
                            for d in &days {
                                slots.push(json!({"day": d, "time": format!("{h:02}:{m:02}"), "minute_of_day": u32::from(h) * 60 + u32::from(m)}));
                            }
                        }
                    }
                    common::timespec::When::Date { local } => dates.push(local.clone()),
                    _ => {}
                }
            }
            let skip: Vec<String> = sb
                .schedule
                .first()
                .map(|s| s.skip_dates.clone())
                .unwrap_or_default();
            starts.push(json!({
                "playlist_id": pid, "playlist": name, "block_id": b.id,
                "anchor": match sb.anchor { station::programming::Anchor::Manual => "manual", station::programming::Anchor::Absolute => "absolute" },
                "how": match sb.how { station::programming::StartHow::Now => "now", station::programming::StartHow::Segue => "segue", station::programming::StartHow::End => "end" },
                "mode": sb.mode.as_str(),
                "window_early_s": sb.early_s, "window_late_s": sb.late_s,
                "slots": slots, "dates": dates, "skip_dates": skip,
                "schedule": b.id,
            }));
        }
        let blocks: Vec<Value> = prog
            .station_blocks
            .iter()
            .map(|b| {
                json!({
                    "id": b.id, "name": b.name, "type": b.kind.type_name(), "enabled": b.enabled,
                    "when": b.timed.as_ref().map(|t| t.spec.describe()),
                    "mode": b.timed.as_ref().map(|t| t.mode.as_str()),
                    "window_early_s": b.timed.as_ref().map(|t| t.early_s),
                    "window_late_s": b.timed.as_ref().map(|t| t.late_s),
                    "problem": b.problem,
                })
            })
            .collect();
        Response::json(
            200,
            &json!({"station": station, "time_zone": zone.name(), "starts": starts, "station_blocks": blocks}),
        )
    }

    fn playlist_json(&self, lib: &Library, id: i64) -> Result<Value, Response> {
        let rows = library::playlists::read_playlists(lib.conn()).map_err(lerr)?;
        let p = rows
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| not_found("no such playlist"))?;
        let station_id: Option<String> = lib
            .conn()
            .query_row(
                "SELECT station_id FROM playlists WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(None);
        let mut entries = vec![];
        for e in &p.entries {
            let mut v = json!({"id": e.id, "position": e.position, "weight": e.weight});
            let o = v.as_object_mut().expect("object");
            match &e.kind {
                library::playlists::EntryKind::Item { item_id } => {
                    o.insert("type".into(), json!("item"));
                    o.insert("item_id".into(), json!(item_id));
                    if let Ok(item) = lib.conn().query_row(
                        &format!("SELECT {ITEM_COLS} {ITEM_FROM} WHERE i.id = ?1"),
                        [item_id],
                        item_row,
                    ) {
                        o.insert("item".into(), item);
                    }
                }
                library::playlists::EntryKind::Album {
                    album_key,
                    continuity,
                } => {
                    o.insert("type".into(), json!("album"));
                    o.insert("album_key".into(), json!(album_key));
                    o.insert("continuity".into(), json!(continuity));
                    let a: Option<(String, i64)> = lib
                        .conn()
                        .query_row("SELECT MIN(album), COUNT(*) FROM items WHERE album_key = ?1 AND deleted_at IS NULL", [album_key], |r| Ok((r.get(0)?, r.get(1)?)))
                        .ok();
                    if let Some((name, n)) = a {
                        o.insert("album".into(), json!(name));
                        o.insert("tracks".into(), json!(n));
                    }
                }
                library::playlists::EntryKind::Block(b) => {
                    o.insert("type".into(), json!("block"));
                    o.insert(
                        "block".into(),
                        json!({"id": b.id, "name": b.name, "kind": b.kind, "trigger": b.trigger, "priority": b.priority, "params": b.params, "enabled": b.enabled}),
                    );
                }
            }
            entries.push(v);
        }
        Ok(json!({
            "id": p.id, "name": p.name, "mode": p.mode.name(), "no_repeat_minutes": p.no_repeat_minutes,
            "separation": p.separation, "station_blocks": p.station_blocks, "enabled": p.enabled,
            "station_id": station_id, "entries": entries,
        }))
    }

    fn playlists(&self, req: &mut Request, segs: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let method = req.method.clone();
        match (method.as_str(), segs) {
            ("GET", []) => {
                let rows: Result<Vec<Value>, _> = c
                    .prepare(
                        "SELECT p.id, p.name, p.mode, p.no_repeat_minutes, p.enabled, p.station_id,
                                (SELECT COUNT(*) FROM playlist_entries e WHERE e.playlist_id = p.id AND e.block_id IS NULL)
                         FROM playlists p ORDER BY p.name COLLATE NOCASE",
                    )
                    .and_then(|mut st| {
                        st.query_map([], |r| {
                            Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "mode": r.get::<_, String>(2)?,
                                      "no_repeat_minutes": r.get::<_, i64>(3)?, "enabled": r.get::<_, i64>(4)? != 0,
                                      "station_id": r.get::<_, Option<String>>(5)?, "entries": r.get::<_, i64>(6)?}))
                        })
                        .map(|r| r.collect::<Result<Vec<_>, _>>())
                    })
                    .and_then(|r| r);
                rows.map(|r| Response::json(200, &json!({"playlists": r})))
                    .unwrap_or_else(dberr)
            }
            ("POST", []) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let mut p = NewPlaylist::shuffle(b["name"].as_str().unwrap_or(""));
                if b["mode"] == "sequential" {
                    p.mode = library::playlists::Mode::Sequential;
                }
                if let Some(n) = b["no_repeat_minutes"].as_i64() {
                    p.no_repeat_minutes = n;
                }
                if let Some(s) = b.get("separation").filter(|s| s.is_object()) {
                    p.separation = s.clone();
                }
                p.station_id = b["station_id"].as_str().map(str::to_string);
                if let Err(r) = self.station_row(c, p.station_id.as_deref()) {
                    return r;
                }
                match lib.create_playlist(&p) {
                    Ok(id) => {
                        self.audit(c, req, "playlist", &id.to_string(), &b);
                        match self.playlist_json(&lib, id) {
                            Ok(v) => Response::json(201, &v),
                            Err(r) => r,
                        }
                    }
                    Err(e) => lerr(e),
                }
            }
            ("GET", [id]) => match id_of(id).map(|i| self.playlist_json(&lib, i)) {
                Some(Ok(v)) => Response::json(200, &v),
                Some(Err(r)) => r,
                None => bad("bad id"),
            },
            ("PATCH", [id]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                if let Some(n) = b["name"].as_str() {
                    if let Err(e) = c.execute(
                        "UPDATE playlists SET name = ?2 WHERE id = ?1",
                        params![id, n],
                    ) {
                        return dberr(e);
                    }
                }
                if let Some(m) = b["mode"]
                    .as_str()
                    .filter(|m| matches!(*m, "shuffle" | "sequential"))
                {
                    let _ = c.execute(
                        "UPDATE playlists SET mode = ?2 WHERE id = ?1",
                        params![id, m],
                    );
                }
                if let Some(n) = b["no_repeat_minutes"].as_i64() {
                    if n < 0 {
                        return Response::error(
                            422,
                            "invalid",
                            "no_repeat_minutes cannot be negative",
                        );
                    }
                    let _ = c.execute(
                        "UPDATE playlists SET no_repeat_minutes = ?2 WHERE id = ?1",
                        params![id, n],
                    );
                }
                if let Some(s) = b.get("separation").filter(|s| s.is_object()) {
                    let _ = c.execute(
                        "UPDATE playlists SET separation = ?2 WHERE id = ?1",
                        params![id, s.to_string()],
                    );
                }
                if let Some(v) = b["enabled"].as_bool() {
                    if v {
                        // A playlist with errors cannot be saved as enabled (4.4).
                        if let Ok(problems) = self.validate_playlist(c, id) {
                            let errs: Vec<&Value> = problems
                                .iter()
                                .filter(|p| p["severity"] == "error")
                                .collect();
                            if !errs.is_empty() {
                                return Response::error(
                                    422,
                                    "invalid",
                                    &format!(
                                        "the playlist has {} error(s) and cannot be enabled: {}",
                                        errs.len(),
                                        errs.iter()
                                            .filter_map(|e| e["message"].as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ")
                                    ),
                                );
                            }
                        }
                    }
                    let _ = c.execute(
                        "UPDATE playlists SET enabled = ?2 WHERE id = ?1",
                        params![id, i64::from(v)],
                    );
                }
                if let Some(v) = b["station_blocks"].as_bool() {
                    let _ = c.execute(
                        "UPDATE playlists SET station_blocks = ?2 WHERE id = ?1",
                        params![id, i64::from(v)],
                    );
                }
                // The station a program belongs to (null: any station). A program of one station is refused by
                // every other.
                if let Some(v) = b.get("station_id") {
                    let st = v.as_str().filter(|s| !s.is_empty());
                    if let Err(r) = self.station_row(c, st) {
                        return r;
                    }
                    if let Err(e) = c.execute(
                        "UPDATE playlists SET station_id = ?2 WHERE id = ?1",
                        params![id, st],
                    ) {
                        return Response::error(
                            422,
                            "invalid",
                            &format!("no station {}: {e}", st.unwrap_or("")),
                        );
                    }
                }
                let _ = c.execute(
                    "UPDATE playlists SET updated_at = ?2 WHERE id = ?1",
                    params![id, library::util::now_iso()],
                );
                let _ = Library::bump_revision(c);
                self.audit(c, req, "playlist", &id.to_string(), &b);
                self.playlist_json(&lib, id)
                    .map(|v| Response::json(200, &v))
                    .unwrap_or_else(|r| r)
            }
            ("DELETE", [id]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match c.execute("DELETE FROM playlists WHERE id = ?1", [id]) {
                    Ok(0) => not_found("no such playlist"),
                    Ok(_) => {
                        let _ = Library::bump_revision(c);
                        self.audit(
                            c,
                            req,
                            "playlist",
                            &id.to_string(),
                            &json!({"deleted": true}),
                        );
                        Response::new(204)
                    }
                    Err(e) => dberr(e),
                }
            }
            ("POST", [id, "entries"]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                let r = if let Some(item) = b["item_id"].as_i64() {
                    lib.add_item_to_playlist(id, item, b["weight"].as_f64().unwrap_or(1.0))
                } else if let Some(key) = b["album_key"].as_str() {
                    lib.add_album_to_playlist(
                        id,
                        key,
                        b["continuity"].as_str().unwrap_or("gapless"),
                    )
                } else if let Some(blk) = b.get("block").filter(|v| v.is_object()) {
                    let kind = blk["kind"].as_str().unwrap_or("");
                    if kind == "insert_every" {
                        let mut nb = NewBlock::insert_every(
                            blk["name"].as_str().unwrap_or("insert"),
                            blk["n"].as_i64().unwrap_or(0),
                            blk["source"].clone(),
                        );
                        for k in [
                            "count_kind",
                            "min_gap_minutes",
                            "resets_on",
                            "position",
                            "inherit",
                        ] {
                            if let Some(v) = blk.get(k) {
                                nb.params[k] = v.clone();
                            }
                        }
                        lib.add_block_to_playlist(id, &nb)
                    } else {
                        // A block whose own fields do not read is refused here, on its row (4.4).
                        let trigger = blk["trigger"].as_str().unwrap_or("sequence");
                        if let Err(e) =
                            check_block(kind, trigger, blk.get("params").unwrap_or(&json!({})))
                        {
                            return Response::error(422, "invalid", &e);
                        }
                        lib.add_block_to_playlist(
                            id,
                            &NewBlock {
                                name: blk["name"].as_str().unwrap_or(kind).into(),
                                kind: kind.into(),
                                trigger: blk["trigger"].as_str().unwrap_or("sequence").into(),
                                priority: blk["priority"].as_i64().unwrap_or(100),
                                params: blk.get("params").cloned().unwrap_or(json!({})),
                            },
                        )
                    }
                } else {
                    return bad("give item_id, album_key or block");
                };
                match r {
                    Ok(_) => {
                        self.audit(c, req, "playlist_entry", &id.to_string(), &b);
                        self.playlist_json(&lib, id)
                            .map(|v| Response::json(201, &v))
                            .unwrap_or_else(|r| r)
                    }
                    Err(e) => lerr(e),
                }
            }
            ("DELETE", [id, "entries", eid]) => {
                let (Some(id), Some(eid)) = (id_of(id), id_of(eid)) else {
                    return bad("bad id");
                };
                match lib.remove_playlist_entry(eid) {
                    Ok(()) => self
                        .playlist_json(&lib, id)
                        .map(|v| Response::json(200, &v))
                        .unwrap_or_else(|r| r),
                    Err(e) => lerr(e),
                }
            }
            ("PUT", [id, "entries", "order"]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                let Some(ids) = b["ids"].as_array() else {
                    return bad("`ids` must be the entry ids in the new order");
                };
                for (pos, e) in ids.iter().filter_map(Value::as_i64).enumerate() {
                    let _ = c.execute(
                        "UPDATE playlist_entries SET position = ?3 WHERE id = ?1 AND playlist_id = ?2",
                        params![e, id, pos as i64],
                    );
                }
                let _ = Library::bump_revision(c);
                self.playlist_json(&lib, id)
                    .map(|v| Response::json(200, &v))
                    .unwrap_or_else(|r| r)
            }
            ("PATCH", [id, "entries", eid]) => {
                let (Some(_id), Some(eid), Ok(b)) = (id_of(id), id_of(eid), req.json()) else {
                    return bad("bad request");
                };
                if let Some(w) = b["weight"].as_f64() {
                    let _ = c.execute(
                        "UPDATE playlist_entries SET weight = ?2 WHERE id = ?1",
                        params![eid, w],
                    );
                }
                if let Some(k) = b["continuity"]
                    .as_str()
                    .filter(|k| matches!(*k, "gapless" | "normal"))
                {
                    let _ = c.execute(
                        "UPDATE playlist_entries SET continuity = ?2 WHERE id = ?1",
                        params![eid, k],
                    );
                }
                // A control block's own fields.
                if let Some(bid) = c
                    .query_row(
                        "SELECT block_id FROM playlist_entries WHERE id = ?1",
                        [eid],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .unwrap_or(None)
                {
                    if let Some(n) = b["name"].as_str() {
                        let _ =
                            c.execute("UPDATE blocks SET name = ?2 WHERE id = ?1", params![bid, n]);
                    }
                    let (kind, mut trigger): (String, String) = c
                        .query_row(
                            "SELECT type, trigger FROM blocks WHERE id = ?1",
                            [bid],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .unwrap_or_default();
                    if let Some(t) = b["trigger"]
                        .as_str()
                        .filter(|t| matches!(*t, "sequence" | "timed" | "standing" | "first"))
                    {
                        trigger = t.to_string();
                        let _ = c.execute(
                            "UPDATE blocks SET trigger = ?2 WHERE id = ?1",
                            params![bid, t],
                        );
                    }
                    if let Some(n) = b["priority"].as_i64() {
                        let _ = c.execute(
                            "UPDATE blocks SET priority = ?2 WHERE id = ?1",
                            params![bid, n],
                        );
                    }
                    if let Some(p) = b.get("params").filter(|p| p.is_object()) {
                        if let Err(e) = check_block(&kind, &trigger, p) {
                            return Response::error(422, "invalid", &e);
                        }
                        let _ = c.execute(
                            "UPDATE blocks SET params = ?2 WHERE id = ?1",
                            params![bid, p.to_string()],
                        );
                    }
                    if let Some(e) = b["enabled"].as_bool() {
                        let _ = c.execute(
                            "UPDATE blocks SET enabled = ?2 WHERE id = ?1",
                            params![bid, i64::from(e)],
                        );
                    }
                }
                let _ = Library::bump_revision(c);
                Response::json(200, &json!({"ok": true}))
            }
            ("POST", [id, "validate"]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match self.validate_playlist(c, id) {
                    Ok(problems) => Response::json(
                        200,
                        &json!({"ok": !problems.iter().any(|p| p["severity"] == "error"), "problems": problems}),
                    ),
                    Err(r) => r,
                }
            }
            ("POST", [id, "copy"]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                let src = match self.playlist_json(&lib, id) {
                    Ok(v) => v,
                    Err(r) => return r,
                };
                let name = b["name"].as_str().map(str::to_string).unwrap_or_else(|| {
                    format!("{} (copy)", src["name"].as_str().unwrap_or("playlist"))
                });
                let mut np = NewPlaylist::shuffle(&name);
                if src["mode"] == "sequential" {
                    np.mode = library::playlists::Mode::Sequential;
                }
                np.no_repeat_minutes = src["no_repeat_minutes"].as_i64().unwrap_or(120);
                np.separation = src["separation"].clone();
                np.station_id = src["station_id"].as_str().map(str::to_string);
                let new_id = match lib.create_playlist(&np) {
                    Ok(i) => i,
                    Err(e) => return lerr(e),
                };
                // A one-off copy (a Thursday show): its own start date replaces the recurrence (4.9).
                let start = b["start"].as_str().map(str::to_string);
                let entries = src["entries"].as_array().cloned().unwrap_or_default();
                if let Some(d) = &start {
                    if d.parse::<common::timespec::CivilDateTime>().is_err() {
                        return bad("`start` is a local date and time such as 2027-01-21T18:45:00");
                    }
                    let has_start = entries.iter().any(|e| e["block"]["kind"] == "start");
                    if !has_start {
                        let _ = lib.add_block_to_playlist(
                            new_id,
                            &NewBlock {
                                name: "Expected".into(),
                                kind: "start".into(),
                                trigger: "first".into(),
                                priority: 100,
                                params: json!({"schedule": [{"date": d}], "anchor": "manual", "arm_window_min": 60}),
                            },
                        );
                    }
                }
                for e in entries {
                    let _ = match e["type"].as_str() {
                        Some("item") => lib.add_item_to_playlist(
                            new_id,
                            e["item_id"].as_i64().unwrap_or(0),
                            e["weight"].as_f64().unwrap_or(1.0),
                        ),
                        Some("album") => lib.add_album_to_playlist(
                            new_id,
                            e["album_key"].as_str().unwrap_or(""),
                            e["continuity"].as_str().unwrap_or("gapless"),
                        ),
                        Some("block") => lib.add_block_to_playlist(
                            new_id,
                            &NewBlock {
                                name: e["block"]["name"].as_str().unwrap_or("").into(),
                                kind: e["block"]["kind"].as_str().unwrap_or("").into(),
                                trigger: e["block"]["trigger"]
                                    .as_str()
                                    .unwrap_or("standing")
                                    .into(),
                                priority: e["block"]["priority"].as_i64().unwrap_or(100),
                                params: {
                                    let mut p = e["block"]["params"].clone();
                                    if let (Some(d), "start") =
                                        (&start, e["block"]["kind"].as_str().unwrap_or(""))
                                    {
                                        p["schedule"] = json!([{"date": d}]);
                                        p["skip_dates"] = json!([]);
                                    }
                                    p
                                },
                            },
                        ),
                        _ => Ok(0),
                    };
                }
                self.playlist_json(&lib, new_id)
                    .map(|v| Response::json(201, &v))
                    .unwrap_or_else(|r| r)
            }
            ("GET", [id, "preview"]) => match id_of(id) {
                Some(id) => self.playlist_preview(c, req, id),
                None => bad("bad id"),
            },
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    fn station_blocks(&self, req: &mut Request, station: &str, rest: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        match (req.method.as_str(), rest) {
            ("GET", []) => match library::playlists::read_station_blocks(lib.conn(), station) {
                Ok(b) => Response::json(
                    200,
                    &json!({"blocks": b.iter().map(|b| json!({"id": b.id, "name": b.name, "kind": b.kind, "trigger": b.trigger, "priority": b.priority, "params": b.params, "enabled": b.enabled})).collect::<Vec<_>>()}),
                ),
                Err(e) => lerr(e),
            },
            ("POST", []) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let kind = b["kind"].as_str().unwrap_or("");
                let nb = if kind == "insert_every" {
                    NewBlock::insert_every(
                        b["name"].as_str().unwrap_or("insert"),
                        b["n"].as_i64().unwrap_or(0),
                        b["source"].clone(),
                    )
                } else {
                    let trigger = b["trigger"].as_str().unwrap_or("standing");
                    if let Err(e) =
                        check_block(kind, trigger, b.get("params").unwrap_or(&json!({})))
                    {
                        return Response::error(422, "invalid", &e);
                    }
                    NewBlock {
                        name: b["name"].as_str().unwrap_or(kind).into(),
                        kind: kind.into(),
                        trigger: trigger.into(),
                        priority: b["priority"].as_i64().unwrap_or(100),
                        params: b.get("params").cloned().unwrap_or(json!({})),
                    }
                };
                match lib.add_station_block(station, &nb) {
                    Ok(id) => {
                        let _ = Library::bump_revision(lib.conn());
                        Response::json(201, &json!({"id": id}))
                    }
                    Err(e) => lerr(e),
                }
            }
            ("PATCH", [id]) => {
                let (Some(id), Ok(b)) = (id_of(id), req.json()) else {
                    return bad("bad request");
                };
                let c = lib.conn();
                let Ok((kind, mut trigger)) = c.query_row(
                    "SELECT type, trigger FROM blocks WHERE id = ?1 AND station_id = ?2",
                    params![id, station],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                ) else {
                    return not_found("no such block");
                };
                if let Some(t) = b["trigger"]
                    .as_str()
                    .filter(|t| matches!(*t, "timed" | "standing"))
                {
                    trigger = t.to_string();
                    let _ = c.execute(
                        "UPDATE blocks SET trigger = ?2 WHERE id = ?1",
                        params![id, t],
                    );
                }
                if let Some(n) = b["name"].as_str() {
                    let _ = c.execute("UPDATE blocks SET name = ?2 WHERE id = ?1", params![id, n]);
                }
                if let Some(n) = b["priority"].as_i64() {
                    let _ = c.execute(
                        "UPDATE blocks SET priority = ?2 WHERE id = ?1",
                        params![id, n],
                    );
                }
                if let Some(e) = b["enabled"].as_bool() {
                    let _ = c.execute(
                        "UPDATE blocks SET enabled = ?2 WHERE id = ?1",
                        params![id, i64::from(e)],
                    );
                }
                if let Some(p) = b.get("params").filter(|p| p.is_object()) {
                    if let Err(e) = check_block(&kind, &trigger, p) {
                        return Response::error(422, "invalid", &e);
                    }
                    let _ = c.execute(
                        "UPDATE blocks SET params = ?2, updated_at = ?3 WHERE id = ?1",
                        params![id, p.to_string(), library::util::now_iso()],
                    );
                }
                let _ = Library::bump_revision(c);
                Response::json(200, &json!({"ok": true}))
            }
            ("DELETE", [id]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match lib.conn().execute(
                    "DELETE FROM blocks WHERE id = ?1 AND station_id = ?2",
                    params![id, station],
                ) {
                    Ok(0) => not_found("no such block"),
                    Ok(_) => {
                        let _ = Library::bump_revision(lib.conn());
                        Response::new(204)
                    }
                    Err(e) => dberr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    /// `/api/v1/processing-presets/{effect}`: the saved settings of an effect of the chain (M6a). Shipped ones
    /// are listed first and are read-only; `PUT` saves or replaces a user preset, `POST .../rename` renames it,
    /// `DELETE` removes it, `GET .../{name}/export` is the downloadable file, `POST .../import` takes one.
    fn processing_presets(&self, req: &mut Request, segs: &[&str]) -> Response {
        use library::presets;
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let [effect, rest @ ..] = segs else {
            return Response::json(200, &json!({"effects": presets::effects()}));
        };
        if !presets::effects().contains(effect) {
            return not_found(&format!("no effect `{effect}` has presets"));
        }
        let entry = |p: &presets::Preset| json!({"name": p.name, "params": p.params, "description": p.description, "builtin": p.builtin});
        match (req.method.as_str(), rest) {
            ("GET", []) => match presets::list(c, effect) {
                Ok(l) => Response::json(
                    200,
                    &json!({"effect": effect, "items": l.iter().map(entry).collect::<Vec<_>>()}),
                ),
                Err(e) => lerr(e),
            },
            ("GET", [name]) => match presets::get(c, effect, name) {
                Ok(Some(p)) => Response::json(200, &entry(&p)),
                Ok(None) => not_found(&format!("no preset `{name}`")),
                Err(e) => lerr(e),
            },
            ("GET", [name, "export"]) => match presets::get(c, effect, name) {
                Ok(Some(p)) => {
                    let mut r = Response::json(200, &presets::export(effect, &p));
                    let file: String = name
                        .chars()
                        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                        .collect();
                    r.headers.push((
                        "content-disposition".into(),
                        format!("attachment; filename=\"{effect}-{file}.json\""),
                    ));
                    r
                }
                Ok(None) => not_found(&format!("no preset `{name}`")),
                Err(e) => lerr(e),
            },
            ("PUT", [name]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                match presets::put(
                    c,
                    effect,
                    name,
                    &b["params"],
                    b["description"].as_str().unwrap_or(""),
                ) {
                    Ok(()) => Response::json(200, &json!({"name": name})),
                    Err(e) => lerr(e),
                }
            }
            ("DELETE", [name]) => match presets::delete(c, effect, name) {
                Ok(true) => Response::new(204),
                Ok(false) => not_found(&format!("no user preset `{name}`")),
                Err(e) => lerr(e),
            },
            ("POST", [name, "rename"]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let Some(to) = b["to"].as_str() else {
                    return bad("`to` is the new name");
                };
                match presets::rename(c, effect, name, to) {
                    Ok(()) => Response::json(200, &json!({"name": to})),
                    Err(e) => lerr(e),
                }
            }
            ("POST", ["import"]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                match presets::import(
                    c,
                    effect,
                    &b["file"],
                    b["overwrite"] == true,
                    b["name"].as_str(),
                ) {
                    Ok(n) => Response::json(200, &json!({"name": n})),
                    Err(e) => lerr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    fn named_table(&self, req: &mut Request, table: &str, segs: &[&str]) -> Response {
        // segue-profiles and transition-presets: list, create or replace a custom one.
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let presets = table == "transition_presets";
        match (req.method.as_str(), segs) {
            ("GET", []) => {
                let sql = if presets {
                    "SELECT name, params, description, builtin FROM transition_presets ORDER BY builtin DESC, name"
                } else {
                    "SELECT name, params, '', 0 FROM segue_profiles ORDER BY name"
                };
                let rows: Result<Vec<Value>, _> = c
                    .prepare(sql)
                    .and_then(|mut st| {
                        st.query_map([], |r| {
                            Ok(json!({"name": r.get::<_, String>(0)?,
                                      "params": serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                                      "description": r.get::<_, String>(2)?, "builtin": r.get::<_, i64>(3)? != 0}))
                        })
                        .map(|r| r.collect::<Result<Vec<_>, _>>())
                    })
                    .and_then(|r| r);
                rows.map(|r| Response::json(200, &json!({ "items": r })))
                    .unwrap_or_else(dberr)
            }
            ("PUT", [name]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                if presets {
                    let builtin: Option<i64> = c.query_row("SELECT builtin FROM transition_presets WHERE name = ?1", [name], |r| r.get(0)).optional().unwrap_or(None);
                    if builtin == Some(1) {
                        return conflict("a built-in preset cannot be changed; make a copy under another name");
                    }
                    if let Err(e) = common::transition::TransitionParams::from_json(&b["params"]) {
                        return Response::error(422, "invalid", &e);
                    }
                    let r = c.execute(
                        "INSERT INTO transition_presets(name, params, description, builtin) VALUES(?1,?2,?3,0)
                         ON CONFLICT(name) DO UPDATE SET params = ?2, description = ?3",
                        params![name, b["params"].to_string(), b["description"].as_str().unwrap_or("")],
                    );
                    if let Err(e) = r {
                        return dberr(e);
                    }
                } else if let Err(e) = c.execute(
                    "INSERT INTO segue_profiles(name, params) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET params = ?2",
                    params![name, b["params"].to_string()],
                ) {
                    return dberr(e);
                }
                let _ = Library::bump_revision(c);
                Response::json(200, &json!({"name": name}))
            }
            ("DELETE", [name]) => {
                let sql = if presets {
                    "DELETE FROM transition_presets WHERE name = ?1 AND builtin = 0"
                } else {
                    "DELETE FROM segue_profiles WHERE name = ?1 AND name NOT IN ('music','liner','ambient','longform')"
                };
                match c.execute(sql, [name]) {
                    Ok(0) => conflict("no such custom entry (built-in ones cannot be deleted)"),
                    Ok(_) => Response::new(204),
                    Err(e) => dberr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    // ---- notifications -----------------------------------------------------------------------------------------------

    fn notify(&self, req: &mut Request, segs: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let channel_json = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
            let id: i64 = r.get(0)?;
            let mut cfg: Value = serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(json!({}));
            if let Some(o) = cfg.as_object_mut() {
                o.remove("password");
            }
            Ok(
                json!({"id": id, "name": r.get::<_, String>(1)?, "kind": r.get::<_, String>(2)?, "config": cfg,
                      "enabled": r.get::<_, i64>(4)? != 0, "last_test_at": r.get::<_, Option<String>>(5)?,
                      "last_test_result": r.get::<_, Option<String>>(6)?,
                      "password_set": notify::secret_is_set(&self.opts.secrets, id)}),
            )
        };
        const SEL: &str = "SELECT id, name, kind, config, enabled, last_test_at, last_test_result FROM notify_channels";
        let method = req.method.clone();
        match (method.as_str(), segs) {
            ("GET", []) => {
                let channels: Vec<Value> = c
                    .prepare(&format!("{SEL} ORDER BY id"))
                    .and_then(|mut st| {
                        st.query_map([], channel_json)
                            .map(|r| r.flatten().collect())
                    })
                    .unwrap_or_default();
                let rules: Vec<Value> = notify::CODES
                    .iter()
                    .map(|code| notify::rule_json(&notify::rule_for(c, code)))
                    .collect();
                let log: Vec<Value> = c
                    .prepare("SELECT at, station, code, channel_id, result, kind FROM notify_log ORDER BY id DESC LIMIT 50")
                    .and_then(|mut st| {
                        st.query_map([], |r| {
                            Ok(json!({"at": r.get::<_, String>(0)?, "station": r.get::<_, String>(1)?, "code": r.get::<_, String>(2)?,
                                      "channel_id": r.get::<_, Option<i64>>(3)?, "result": r.get::<_, String>(4)?, "kind": r.get::<_, String>(5)?}))
                        })
                        .map(|r| r.flatten().collect())
                    })
                    .unwrap_or_default();
                Response::json(
                    200,
                    &json!({"channels": channels, "rules": rules, "log": log}),
                )
            }
            ("POST", ["channels"]) | ("PUT", ["channels", _]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let existing = segs.get(1).and_then(|s| id_of(s));
                let mut cfg = b["config"].clone();
                if !cfg.is_object() {
                    cfg = json!({});
                }
                if let Some(o) = cfg.as_object_mut() {
                    o.remove("password");
                }
                if let Err(e) = notify::SmtpConfig::from_channel(&cfg, None) {
                    return Response::error(422, "invalid", &e);
                }
                let name = b["name"].as_str().unwrap_or("email");
                let enabled = b["enabled"].as_bool().unwrap_or(true);
                let id = match existing {
                    Some(id) => {
                        match c.execute(
                            "UPDATE notify_channels SET name=?2, config=?3, enabled=?4 WHERE id=?1",
                            params![id, name, cfg.to_string(), i64::from(enabled)],
                        ) {
                            Ok(0) => return not_found("no such channel"),
                            Ok(_) => {}
                            Err(e) => return dberr(e),
                        }
                        id
                    }
                    None => {
                        if let Err(e) = c.execute(
                            "INSERT INTO notify_channels(name, kind, config, enabled) VALUES(?1,'smtp',?2,?3)",
                            params![name, cfg.to_string(), i64::from(enabled)],
                        ) {
                            return dberr(e);
                        }
                        c.last_insert_rowid()
                    }
                };
                // Write-only: stored in secrets.toml, never echoed.
                if let Some(pw) = b["password"].as_str() {
                    if let Err(e) = notify::write_secret(&self.opts.secrets, id, pw) {
                        return Response::error(500, "internal", &format!("secrets file: {e}"));
                    }
                }
                self.audit(
                    c,
                    req,
                    "notify_channel",
                    &id.to_string(),
                    &json!({"name": name, "password_changed": b["password"].is_string()}),
                );
                c.query_row(&format!("{SEL} WHERE id = ?1"), [id], channel_json)
                    .map(|v| Response::json(if existing.is_some() { 200 } else { 201 }, &v))
                    .unwrap_or_else(dberr)
            }
            ("DELETE", ["channels", id]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match c.execute("DELETE FROM notify_channels WHERE id = ?1", [id]) {
                    Ok(0) => not_found("no such channel"),
                    Ok(_) => {
                        // The password goes with the channel.
                        if let Err(e) = notify::remove_secret(&self.opts.secrets, id) {
                            return Response::error(500, "internal", &format!("secrets file: {e}"));
                        }
                        Response::new(204)
                    }
                    Err(e) => dberr(e),
                }
            }
            ("POST", ["channels", id, "test"]) => {
                let Some(id) = id_of(id) else {
                    return bad("bad id");
                };
                match notify::test_channel(c, &self.opts.secrets, &self.opts.product, id) {
                    Ok(reply) => Response::json(200, &json!({"ok": true, "reply": reply})),
                    Err(e) => Response::json(200, &json!({"ok": false, "error": e})),
                }
            }
            // The "Send the digest now" button: what the notifier does at the top of the hour (or 07:00 for
            // the daily one), on request. Empties the bucket when the message went out.
            ("POST", ["digest"]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let bucket = b["bucket"].as_str().unwrap_or("hourly_digest");
                if !matches!(bucket, "hourly_digest" | "daily_digest") {
                    return Response::error(
                        422,
                        "invalid",
                        "bucket is hourly_digest or daily_digest",
                    );
                }
                let pending = |c: &Connection| -> i64 {
                    c.query_row(
                        "SELECT COUNT(*) FROM notify_queue WHERE bucket = ?1",
                        [bucket],
                        |r| r.get(0),
                    )
                    .unwrap_or(0)
                };
                let before = pending(c);
                notify::Notifier::new(
                    self.opts.paths.db.clone(),
                    PathBuf::new(),
                    self.opts.secrets.clone(),
                    self.opts.product.clone(),
                )
                .send_digest(c, bucket);
                let after = pending(c);
                Response::json(
                    200,
                    &json!({"bucket": bucket, "events": before, "sent": before > 0 && after == 0, "left": after}),
                )
            }
            ("PUT", ["rules", code]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let cur = notify::rule_for(c, code);
                let delivery = b["delivery"].as_str().unwrap_or(&cur.delivery);
                if !matches!(
                    delivery,
                    "immediate" | "hourly_digest" | "daily_digest" | "off"
                ) {
                    return Response::error(
                        422,
                        "invalid",
                        "delivery is immediate, hourly_digest, daily_digest or off",
                    );
                }
                let channels = b
                    .get("channels")
                    .cloned()
                    .unwrap_or_else(|| json!(cur.channels));
                let r = c.execute(
                    "INSERT INTO notify_rules(code, enabled, severity, channels, delivery, rate_limit_min, notify_on_clear)
                     VALUES(?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(code) DO UPDATE SET enabled=?2, severity=?3, channels=?4, delivery=?5, rate_limit_min=?6, notify_on_clear=?7",
                    params![
                        code,
                        i64::from(b["enabled"].as_bool().unwrap_or(cur.enabled)),
                        b.get("severity").map_or(cur.severity.clone(), |s| s.as_str().map(str::to_string)),
                        channels.to_string(),
                        delivery,
                        b["rate_limit_min"].as_i64().unwrap_or(cur.rate_limit_min).max(0),
                        i64::from(b["notify_on_clear"].as_bool().unwrap_or(cur.notify_on_clear))
                    ],
                );
                match r {
                    Ok(_) => Response::json(200, &notify::rule_json(&notify::rule_for(c, code))),
                    Err(e) => dberr(e),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    // ---- display view ----------------------------------------------------------------------------------------------------

    fn display_json(&self, c: &Connection) -> Value {
        let mut v = c.query_row("SELECT view, station, rotate_s FROM display_view WHERE id = 1", [], |r| {
            Ok(json!({"view": r.get::<_, String>(0)?, "station": r.get::<_, Option<String>>(1)?, "rotate_s": r.get::<_, i64>(2)?}))
        })
        .unwrap_or(json!({"view": "single", "station": null, "rotate_s": 20}));
        // The settings of the global file that shape the views (a key of the file, not a column: `upgrade.sh
        // rollback` must keep opening the database). Read fresh, so a change on the Settings page shows at once.
        let d = common::config::GlobalConfig::load_or_default(&self.opts.global_config)
            .map(|g| g.display)
            .unwrap_or_default();
        v["rotate_pin_on_alert"] = json!(d.rotate_pin_on_alert);
        v["rotate_processing"] = json!(d.rotate_processing);
        v["proc_range_db"] = json!(d.proc_range_db);
        v["proc_spectrum"] = json!(d.proc_spectrum);
        v["spectrum_source"] = json!(d.spectrum_source);
        v["bypassed_stages"] = json!(d.bypassed_stages);
        v
    }

    fn display(&self, req: &mut Request) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        if req.method == "PUT" {
            let b = match req.json() {
                Ok(b) => b,
                Err(r) => return r,
            };
            let view = b["view"].as_str().unwrap_or("single");
            if !matches!(view, "single" | "split" | "rotate" | "processing") {
                return Response::error(
                    422,
                    "invalid",
                    "view is single, split, rotate or processing",
                );
            }
            let cur = self.display_json(lib.conn());
            let station = b
                .get("station")
                .map_or(cur["station"].as_str().map(str::to_string), |s| {
                    s.as_str().map(str::to_string)
                });
            let rot = b["rotate_s"]
                .as_i64()
                .unwrap_or_else(|| cur["rotate_s"].as_i64().unwrap_or(20))
                .clamp(3, 600);
            let _ = lib.conn().execute(
                "UPDATE display_view SET view = ?1, station = ?2, rotate_s = ?3 WHERE id = 1",
                params![view, station, rot],
            );
            let now = self.display_json(lib.conn());
            self.hub.send_with("display", &|seq| {
                json!({"v": 1, "type": "display", "seq": seq, "data": now}).to_string()
            });
            return Response::json(200, &now);
        }
        Response::json(200, &self.display_json(lib.conn()))
    }

    /// What the top bar of the box's screen shows in `split` besides the clock, and which items are on. The page
    /// asks every 2 s, so a change of a `display.bar_*` setting shows within that.
    pub(crate) fn display_bar_json(&self) -> Value {
        let d = common::config::GlobalConfig::load_or_default(&self.opts.global_config)
            .map(|g| g.display)
            .unwrap_or_default();
        let net = crate::sysinfo::network_up(
            std::path::Path::new("/proc/net"),
            std::path::Path::new("/sys/class/net"),
        );
        json!({
            "product": self.opts.product,
            "version": crate::sysinfo::short_version(&self.opts.version),
            "cpu_percent": self.cpu.percent(),
            "network": net.map(|up| if up { "up" } else { "down" }),
            "items": {"product": d.bar_product, "clock": d.bar_clock, "date": d.bar_date, "cpu": d.bar_cpu, "network": d.bar_network},
        })
    }

    // ---- the meters' ballistics (11.2, M7) ------------------------------------------------------------------------------

    pub(crate) fn meters_path(&self) -> std::path::PathBuf {
        self.opts.paths.db.with_file_name("display-meters.json")
    }

    /// The box's meter ballistics: the saved ones, else the default preset. A file next to the library database
    /// (a new column would make the database unreadable to the previous package, and `upgrade.sh rollback` must work).
    pub fn meters_current(&self) -> common::meters::MeterSettings {
        std::fs::read_to_string(self.meters_path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| common::meters::MeterSettings::from_json(&v).ok())
            .unwrap_or_default()
    }

    /// What one asker sees: the default (no station), or the station's effective setting and whether it inherits.
    fn meters_json(&self, m: &common::meters::MeterSettings, station: Option<&str>) -> Value {
        let mut v = match station {
            Some(st) => {
                let mut v = m.effective(st).to_json();
                v["station"] = json!(st);
                v["inherits"] = json!(m.inherits(st));
                v
            }
            None => m.default.to_json(),
        };
        v["presets"] = common::meters::Ballistics::presets_json();
        v["settings"] = m.to_json();
        v
    }

    /// `GET`/`PUT /api/v1/display/meters`. Without a station it is the box default, as before; `?station=<name>` (or a
    /// `station` field in the PUT body) reads or sets that station's own setting, and `{"inherit": true}` takes it back
    /// to the box default. The WebSocket `display` topic carries the whole map (`display_meters`, and the snapshot).
    fn meters(&self, req: &mut Request) -> Response {
        let mut cur = self.meters_current();
        let mut station = req.query_get("station");
        let body = if req.method == "PUT" {
            match req.json() {
                Ok(b) => Some(b),
                Err(r) => return r,
            }
        } else {
            None
        };
        if station.is_none() {
            station = body
                .as_ref()
                .and_then(|b| b["station"].as_str().map(str::to_string));
        }
        if let Some(st) = &station {
            if !self.stations().iter().any(|(n, _, _)| n == st) {
                return Response::error(404, "not_found", &format!("no station `{st}`"));
            }
        }
        if let Some(body) = body {
            if let Err(e) = cur.apply(station.as_deref(), &body) {
                return Response::error(422, "invalid", &e);
            }
            let tmp = self.meters_path().with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, cur.to_json().to_string())
                .and_then(|()| std::fs::rename(&tmp, self.meters_path()))
            {
                return Response::error(
                    500,
                    "internal",
                    &format!("cannot save the meter settings: {e}"),
                );
            }
            let all = cur.to_json();
            self.hub.send_with("display", &|seq| {
                json!({"v": 1, "type": "display_meters", "seq": seq, "data": all}).to_string()
            });
        }
        Response::json(200, &self.meters_json(&cur, station.as_deref()))
    }

    // ---- the station proxy -------------------------------------------------------------------------------------------------

    fn proxy(&self, req: &mut Request, station: &str, rest: &str) -> Reply {
        let Some(addr) = self.station_addr(station) else {
            return not_found(&format!("no station `{station}`")).into();
        };
        // `rest` was percent-decoded by the server: encode it again, so what reaches the station's request
        // line is exactly the path that was asked for (a decoded space or `?` would change its meaning).
        let rest = encode_path(rest);
        let upstream_path = format!(
            "/{rest}{}",
            if req.raw_query.is_empty() {
                String::new()
            } else {
                format!("?{}", req.raw_query)
            }
        );
        let is_ws = req
            .header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        if is_ws {
            let head = req.head_bytes.clone();
            let peer_addr = addr.clone();
            return Reply::Takeover(Box::new(move |mut client_stream, buffered| {
                // Replay the client's handshake to the station with the Host and path rewritten (and no
                // Origin: the station sees a local client), then copy bytes both ways.
                let Ok(mut up) = TcpStream::connect(&peer_addr) else {
                    let _ = client_stream
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n");
                    return;
                };
                let text = String::from_utf8_lossy(&head).into_owned();
                let mut lines = text.split("\r\n");
                let _ = lines.next();
                let mut out = format!("GET {upstream_path} HTTP/1.1\r\nHost: {peer_addr}\r\n");
                for l in lines {
                    let low = l.to_ascii_lowercase();
                    if l.is_empty() || low.starts_with("host:") || low.starts_with("origin:") {
                        continue;
                    }
                    out.push_str(l);
                    out.push_str("\r\n");
                }
                out.push_str("\r\n");
                if up.write_all(out.as_bytes()).is_err() || up.write_all(&buffered).is_err() {
                    return;
                }
                pipe(client_stream, up);
            }));
        }
        let mut headers: Vec<(&str, String)> = vec![];
        for k in ["content-type", "if-none-match", "range", "x-requested-with"] {
            if let Some(v) = req.header(k) {
                headers.push((k, v.to_string()));
            }
        }
        let body = match req.body_bytes(crate::http::MAX_JSON_BODY.max(24 << 20)) {
            Ok(b) => b,
            Err(r) => return r.into(),
        };
        let h: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match client::request(
            &addr,
            &req.method,
            &upstream_path,
            &h,
            &body,
            Duration::from_secs(8),
        ) {
            Ok(r) => {
                let mut out = Response::new(r.status);
                for (k, v) in &r.headers {
                    if matches!(
                        k.as_str(),
                        "content-type"
                            | "etag"
                            | "cache-control"
                            | "content-disposition"
                            | "content-range"
                    ) {
                        out.headers.push((k.clone(), v.clone()));
                    }
                }
                out.body = r.body;
                out.into()
            }
            Err(e) => Response::error(
                502,
                "bad_gateway",
                &format!("station `{station}` did not answer: {e}"),
            )
            .into(),
        }
    }

    fn stations_json(&self) -> Response {
        let lib = self.lib().ok();
        let mut arr = vec![];
        for (name, display, listen) in self.stations() {
            let addr = self.station_addr(&name).unwrap_or_default();
            let health = client::request(&addr, "GET", "/health", &[], b"", Duration::from_millis(700))
                .ok()
                .map(|r| json!({"http": r.status, "status": r.json()["status"], "state": r.json()["state"]}));
            arr.push(json!({"id": name, "display_name": display, "listen": listen, "url": format!("/api/v1/stations/{name}"), "health": health}));
        }
        let _ = lib;
        Response::json(200, &json!({"stations": arr}))
    }

    fn preview(&self, req: &Request) -> Response {
        let q = |k: &str| req.query_get(k);
        let (Some(from), Some(to)) = (
            q("from").and_then(|s| s.parse::<i64>().ok()),
            q("to").and_then(|s| s.parse::<i64>().ok()),
        ) else {
            return bad("`from` and `to` (item ids) are required");
        };
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let pre_s: f64 = q("pre_s")
            .and_then(|s| s.parse().ok())
            .unwrap_or(10.0f64)
            .clamp(0.0, 60.0);
        let post_s: f64 = q("post_s")
            .and_then(|s| s.parse().ok())
            .unwrap_or(10.0f64)
            .clamp(0.0, 60.0);
        // `preset` names the preset the pair is auditioned under, as the highest layer (the per-entry
        // override of 2.6), so it wins over the categories' and the items' own presets; `station` or no
        // value plays the pair as a station would, with the station default `plexamp` and every layer
        // the items and categories carry. `params` is a JSON patch on top, for the same boundary only.
        let preset = q("preset").filter(|p| !p.is_empty() && p != "station");
        let patch: Value = match q("params").filter(|p| !p.is_empty()) {
            Some(p) => match serde_json::from_str(&p) {
                Ok(v) => v,
                Err(e) => return bad(&format!("`params` is not JSON: {e}")),
            },
            None => Value::Null,
        };
        let format = q("format").unwrap_or_else(|| "wav".into());
        let depth32 = q("depth").as_deref() == Some("32");
        let key = format!(
            "{from}|{to}|{}|{patch}|{pre_s}|{post_s}|{depth32}|{}",
            preset.as_deref().unwrap_or("station"),
            lib.revision().unwrap_or(0)
        );
        if let Ok(cache) = self.preview_cache.lock() {
            if let Some((_, wav, meta)) = cache.iter().find(|(k, _, _)| *k == key) {
                return preview_answer(&format, wav.clone(), meta.clone());
            }
        }
        // The chosen preset is the station's too (it decides how the first item is bounded and placed) and the
        // boundary's own override, so no category or item preset can take the audition away from it.
        // The items are built from the stored rows, the way a station builds them: effective markers
        // (overrides included), loudness inputs, ramps and the item's and category's transition layers.
        let mut items = std::collections::BTreeMap::new();
        let mut rows = vec![];
        for (name, id) in [("a", from), ("b", to)] {
            let row = match library::tracks::read_track(lib.conn(), id) {
                Ok(Some(r)) if !r.deleted => r,
                Ok(_) => return not_found(&format!("no item {id}")),
                Err(e) => return lerr(e),
            };
            let mut item =
                match station::item::Item::from_row(&row, row.path(&self.opts.paths.media)) {
                    Ok(i) => i,
                    Err(e) => return Response::error(422, "invalid", &format!("item {id}: {e}")),
                };
            // A station's track cache carries only the K10 blocks; the `fm` preset's last-crossing exit reads
            // the K200 curve (3.3), which the library has. The preview gives the item both, so it renders
            // what the scenario renderer renders for the same file.
            let k200: Option<Vec<u8>> = lib
                .conn()
                .query_row(
                    "SELECT env_k200 FROM analysis WHERE item_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .unwrap_or(None);
            if let Some(k) = k200.and_then(|k| analysis::env::unpack(&k).ok()) {
                let mut env = (*item.env).clone();
                env.k200 = analysis::env::dequantize(&k);
                item.env = Arc::new(env);
            }
            items.insert(name.to_string(), Arc::new(item));
            rows.push(row);
        }
        // The library's presets (custom ones included) are available to the render by name.
        let mut presets = serde_json::Map::new();
        if let Ok(mut st) = lib
            .conn()
            .prepare("SELECT name, params FROM transition_presets")
        {
            if let Ok(it) =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            {
                for (n, p) in it.flatten() {
                    if let Ok(v) = serde_json::from_str::<Value>(&p) {
                        presets.insert(n, v);
                    }
                }
            }
        }
        if let Some(p) = &preset {
            if !presets.contains_key(p) {
                return bad(&format!("no transition preset `{p}`"));
            }
        }
        let mut second = json!({"item": "b"});
        if preset.is_some() || !patch.is_null() {
            let mut t = serde_json::Map::new();
            if let Some(p) = &preset {
                t.insert("preset".into(), json!(p));
            }
            if !patch.is_null() {
                t.insert("patch".into(), patch.clone());
            }
            second["transition"] = Value::Object(t);
        }
        let scenario = json!({
            "name": "preview", "station": {"preset": preset.as_deref().unwrap_or("plexamp"), "start_s": 0.1, "presets": presets},
            "items": {}, "queue": [{"item": "a"}, second],
        });
        let sc = match station::scenario::Scenario::from_yaml(&scenario.to_string()) {
            Ok(s) => s,
            Err(e) => return Response::error(422, "invalid", &e),
        };
        let r = match station::scenario::preview_pair_items(&sc, items, pre_s, post_s) {
            Ok(r) => r,
            Err(e) => return Response::error(422, "invalid", &e),
        };
        let meta = preview_meta(
            &r,
            &rows,
            preset.as_deref().unwrap_or("station"),
            pre_s,
            post_s,
        );
        let wav = if depth32 {
            wav32f(&r.samples, r.rate)
        } else {
            wav16(&r.samples, r.rate)
        };
        if let Ok(mut cache) = self.preview_cache.lock() {
            cache.push((key, wav.clone(), meta.clone()));
            if cache.len() > 4 {
                cache.remove(0);
            }
        }
        preview_answer(&format, wav, meta)
    }

    fn health(&self) -> Response {
        let lib = self.lib();
        let (status, items, rev) = match &lib {
            Ok(l) => (
                "ok",
                l.conn()
                    .query_row(
                        "SELECT COUNT(*) FROM items WHERE deleted_at IS NULL",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(-1),
                l.revision().unwrap_or(-1),
            ),
            Err(_) => ("critical", -1, -1),
        };
        let queued: i64 = lib
            .as_ref()
            .ok()
            .and_then(|l| {
                l.conn()
                    .query_row(
                        "SELECT COUNT(*) FROM import_jobs WHERE status IN ('queued','running')",
                        [],
                        |r| r.get(0),
                    )
                    .ok()
            })
            .unwrap_or(0);
        let snap = library::backup::read_status(&self.opts.paths.backup, "library");
        let body = json!({
            "status": status, "product": self.opts.product, "service": "library", "version": self.opts.version,
            "uptime_s": self.started.elapsed().as_secs(), "items": items, "revision": rev, "import_jobs_active": queued,
            "disk": {"free_gb": disk_free(&self.opts.paths.media).map(|b| (b as f64 / 1e9 * 10.0).round() / 10.0)},
            "notifications_sent": self.notifications_sent.load(Ordering::Relaxed),
            "stations": self.stations().into_iter().map(|s| s.0).collect::<Vec<_>>(),
            "db": {
                "last_snapshot": snap.as_ref().map(|i| station::state::iso_ms(i.taken_ms)),
                "snapshot_check": snap.as_ref().map(|i| i.check.clone()),
            },
        });
        Response::json(if status == "ok" { 200 } else { 503 }, &body)
    }

    fn metrics(&self) -> Response {
        let mut o = String::new();
        let lib = self.lib().ok();
        let mut put = |help: &str, typ: &str, name: &str, labels: &str, v: f64| {
            let _ = writeln!(
                o,
                "# HELP {name} {help}\n# TYPE {name} {typ}\n{name}{labels} {v}"
            );
        };
        let items = lib
            .as_ref()
            .and_then(|l| {
                l.conn()
                    .query_row(
                        "SELECT COUNT(*) FROM items WHERE deleted_at IS NULL",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok()
            })
            .unwrap_or(-1);
        put(
            "Items in the library.",
            "gauge",
            "library_items_total",
            "",
            items as f64,
        );
        if let Some(l) = &lib {
            let mut seen = false;
            for st in ["queued", "running", "done", "duplicate", "failed"] {
                let n: i64 = l
                    .conn()
                    .query_row(
                        "SELECT COUNT(*) FROM import_jobs WHERE status = ?1",
                        [st],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if !seen {
                    let _ = writeln!(o, "# HELP library_import_jobs Import jobs by status.\n# TYPE library_import_jobs gauge");
                    seen = true;
                }
                let _ = writeln!(o, "library_import_jobs{{status=\"{st}\"}} {n}");
            }
        }
        let mut put2 = |help: &str, typ: &str, name: &str, v: f64| {
            let _ = writeln!(o, "# HELP {name} {help}\n# TYPE {name} {typ}\n{name} {v}");
        };
        if let Some(b) = disk_free(&self.opts.paths.media) {
            put2(
                "Free bytes on the media store's filesystem.",
                "gauge",
                "library_disk_free_bytes",
                b as f64,
            );
        }
        put2(
            "Notification messages sent.",
            "counter",
            "hotclock_notifications_sent_total",
            self.notifications_sent.load(Ordering::Relaxed) as f64,
        );
        Response::text(200, "text/plain; version=0.0.4; charset=utf-8", o)
    }
}

use std::fmt::Write as _;

fn disk_free(p: &std::path::Path) -> Option<u64> {
    let s = rustix::fs::statvfs(p).ok()?;
    Some(s.f_bavail * s.f_frsize)
}

fn preview_answer(format: &str, wav: Vec<u8>, meta: Value) -> Response {
    if format == "json" {
        return Response::json(200, &meta);
    }
    Response::text(200, "audio/wav", wav).with_header("x-preview", &meta.to_string())
}

/// What the editor draws over the two waveforms: per item the file position of every output frame
/// (`file_start_ms` at `started_frame`), the markers, the ramps that were sent for it and the stop; frames are
/// on the render's timeline, and `crop_from` is the timeline frame of the first sample of the WAV.
fn preview_meta(
    r: &station::scenario::Rendered,
    rows: &[library::tracks::TrackRow],
    preset: &str,
    pre_s: f64,
    post_s: f64,
) -> Value {
    let sc = &r.sidecar;
    let side = |i: usize| -> Value {
        let row = &rows[i];
        let v = sc.voices.get(i);
        json!({
            "item": row.item_id, "title": row.title, "artist": row.artist,
            "markers": {"cue_in": row.cue_in, "attack": row.attack, "intro": row.intro, "segue": row.segue,
                        "fade_out": row.fade_out, "cue_out": row.cue_out},
            "voice": v.map(|v| json!({
                "start_frame": v.started_frame.unwrap_or(v.start_frame), "ended_frame": v.ended_frame,
                "file_start_ms": v.file_start_ms, "gain_db": v.gain_db,
                "attack_frame": v.attack_frame, "segue_frame": v.segue_frame, "cue_out_frame": v.cue_out_frame,
                "stop": v.stop.as_ref().map(|s| json!({"at": s.at, "frames": s.frames, "curve": s.curve})),
                "ramps": v.envelope.iter().map(|e| json!({"start": e.start, "end": e.end, "from": e.from, "to": e.to, "curve": e.curve})).collect::<Vec<_>>(),
            })),
        })
    };
    json!({
        "rate": r.rate, "frames": r.samples.len() / 2, "crop_from": r.crop_from, "pre_s": pre_s, "post_s": post_s,
        "preset": preset,
        "boundary": serde_json::to_value(sc.boundaries.first()).unwrap_or(Value::Null),
        "ramps": serde_json::to_value(sc.voices.iter().map(|v| &v.envelope).collect::<Vec<_>>()).unwrap_or(Value::Null),
        "a": side(0), "b": side(1),
    })
}

/// 32-bit float WAV of interleaved stereo floats: the render's own samples, unquantized.
fn wav32f(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 4) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 8).to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for x in samples {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

/// 16-bit PCM WAV of interleaved stereo floats.
fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 4).to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    v
}

/// Percent-encode a decoded path for a request line: everything but unreserved characters, `/` and the
/// sub-delimiters RFC 3986 allows in a segment.
fn encode_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Copy bytes both ways until either side closes.
fn pipe(a: TcpStream, b: TcpStream) {
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let (mut a1, mut b1) = (a, b);
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a1, &mut b1);
        let _ = b1.shutdown(std::net::Shutdown::Both);
        let _ = a1.shutdown(std::net::Shutdown::Both);
    });
    let _ = std::io::copy(&mut b2, &mut a2);
    let _ = a2.shutdown(std::net::Shutdown::Both);
    let _ = b2.shutdown(std::net::Shutdown::Both);
    let _ = t.join();
}

// ---- the router ---------------------------------------------------------------------------------------------------------

pub struct LibraryHandler {
    pub api: Arc<LibraryApi>,
}

impl Handler for LibraryHandler {
    fn handle(&self, req: &mut Request) -> Reply {
        let api = &self.api;
        let path = req.path.clone();
        let segs: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        let get = req.method == "GET" || req.method == "HEAD";
        match segs.as_slice() {
            [] if get => return web::page("app.html", &api.opts.product, "").into(),
            ["health"] if get => return api.health().into(),
            ["metrics"] if get => return api.metrics().into(),
            // `/display?station=` is one station; without it, the box's screen with its view (11.2b, 11.4)
            ["display"] if get => {
                return if req.query_get("station").is_some() {
                    web::page(
                        "display.html",
                        &api.opts.product,
                        "/api/v1/stations/{station}",
                    )
                    .into()
                } else {
                    web::page("screen.html", &api.opts.product, "").into()
                };
            }
            ["static", name] if get => return web::static_asset(name).into(),
            ["api", "v1", ..] => {}
            _ if known_shape(&segs.join("/")) => {
                return Response::error(405, "method_not_allowed", "not allowed here").into()
            }
            _ => return not_found("no such page").into(),
        }
        let rest = &segs[2..];
        let m = req.method.clone();
        let r: Response = match (m.as_str(), rest) {
            ("GET", ["ws"]) => {
                let topics: Vec<String> = req
                    .query_get("topics")
                    .unwrap_or_else(|| "display,library".into())
                    .split(',')
                    .map(str::to_string)
                    .collect();
                let hub = api.hub.clone();
                let a2 = api.clone();
                return ws::upgrade(req, move |stream, _| {
                    let a3 = a2.clone();
                    serve_ws(
                        hub,
                        Arc::new(move |seq| {
                            let rev = a3.lib().ok().and_then(|l| l.revision().ok()).unwrap_or(0);
                            let disp = a3
                                .lib()
                                .map(|l| a3.display_json(l.conn()))
                                .unwrap_or(Value::Null);
                            json!({"v": 1, "type": "snapshot", "seq": seq, "data": {"revision": rev, "display": disp, "meters": a3.meters_current().to_json()}}).to_string()
                        }),
                        stream,
                        topics,
                    )
                });
            }
            ("GET", ["items"]) => api.list_items(req),
            ("GET", ["items", id]) => match id_of(id) {
                Some(i) => api
                    .lib()
                    .and_then(|l| api.item_detail(&l, i))
                    .map(|v| Response::json(200, &v))
                    .unwrap_or_else(|r| r),
                None => bad("bad id"),
            },
            ("PATCH", ["items", id]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.patch_item(req, i))
            }
            ("DELETE", ["items", id]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.delete_item(req, i))
            }
            ("POST", ["items", id, "release"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.release_item(req, i))
            }
            ("PUT", ["items", id, "overrides"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.put_overrides(req, i))
            }
            ("DELETE", ["items", id, "overrides"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.delete_overrides(req, i))
            }
            ("GET", ["items", id, "waveform"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.waveform(i))
            }
            ("GET", ["items", id, "audio"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.audio(req, i))
            }
            ("GET", ["items", id, "art"]) => match id_of(id) {
                Some(i) => art_response(
                    &api.opts.paths.db,
                    &api.opts.paths.media,
                    i,
                    &req.query_get("size").unwrap_or_else(|| "600".into()),
                    req.header("if-none-match"),
                ),
                None => bad("bad id"),
            },
            ("PUT", ["items", id, "art"]) => {
                id_of(id).map_or_else(|| bad("bad id"), |i| api.put_art(req, i))
            }
            ("GET", ["albums"]) => api.albums(req, None),
            ("GET", ["albums", key]) => api.albums(req, Some(key)),
            ("GET", ["preview", "transition"]) => api.preview(req),
            ("GET", ["block-templates"]) => Response::json(200, &station::side::templates()),
            ("GET", ["examples"]) => Response::json(200, &examples_json()),
            ("POST", ["examples", name]) => api.create_example(req, name),
            ("POST", ["imports"]) => api.upload(req),
            ("GET", ["imports"]) => api.import_jobs(),
            ("POST", ["reanalyze"]) => api.reanalyze(req),
            ("GET", ["reanalyze"]) => api.reanalyze_status(),
            (_, ["categories", tail @ ..]) => api.categories(req, tail),
            (_, ["tags", tail @ ..]) => api.tags(req, tail),
            (_, ["playlists", tail @ ..]) => api.playlists(req, tail),
            (_, ["processing-presets", tail @ ..]) => api.processing_presets(req, tail),
            (_, ["segue-profiles", tail @ ..]) => api.named_table(req, "segue_profiles", tail),
            (_, ["transition-presets", tail @ ..]) => {
                api.named_table(req, "transition_presets", tail)
            }
            (_, ["transition-matrix", ..]) => Response::error(
                501,
                "not_implemented",
                "context-aware transitions are the optional milestone M9",
            ),
            (_, ["notify", tail @ ..]) => api.notify(req, tail),
            (_, ["plex", tail @ ..]) => api.plex(req, tail),
            ("GET", ["display"]) | ("PUT", ["display"]) => api.display(req),
            ("GET", ["display", "meters"]) | ("PUT", ["display", "meters"]) => api.meters(req),
            ("GET", ["display", "bar"]) => Response::json(200, &api.display_bar_json()),
            ("GET", ["stations"]) => api.stations_json(),
            ("GET", ["settings"]) => api.settings_get(),
            ("PUT", ["settings", "station", name]) => api.settings_put(req, Some(name)),
            ("PUT", ["settings", "global"]) => api.settings_put(req, None),
            ("POST", ["system", "restart"]) => api.system_restart(req),
            (_, ["station-admin", tail @ ..]) => api.station_admin(req, tail),
            // The station's own block log (5.3) is not one of its block definitions.
            ("GET", ["stations", st, "blocks", "log"]) => {
                return api.proxy(req, st, "api/v1/blocks/log");
            }
            (_, ["stations", st, "blocks", tail @ ..]) => api.station_blocks(req, st, tail),
            (_, ["stations", st, tail @ ..]) => {
                let rest = format!("api/v1/{}", tail.join("/"));
                let health_path = tail.first() == Some(&"health");
                let _ = health_path;
                return api.proxy(req, st, &rest);
            }
            ("GET", ["schedule", "week"]) => api.schedule_week(req),
            _ if known_shape(&format!("api/v1/{}", rest.join("/"))) => {
                Response::error(405, "method_not_allowed", "that method is not allowed here")
            }
            _ => not_found("no such endpoint"),
        };
        r.into()
    }
}

/// The path shapes the library service has (a wrong method answers 405, an unknown path 404).
pub fn known_shape(path: &str) -> bool {
    const SHAPES: &[&str] = &[
        "health",
        "metrics",
        "display",
        "static/*",
        "api/v1/ws",
        "api/v1/items",
        "api/v1/items/*",
        "api/v1/items/*/overrides",
        "api/v1/items/*/release",
        "api/v1/items/*/waveform",
        "api/v1/items/*/audio",
        "api/v1/items/*/art",
        "api/v1/albums",
        "api/v1/albums/*",
        "api/v1/preview/transition",
        "api/v1/imports",
        "api/v1/reanalyze",
        "api/v1/categories",
        "api/v1/categories/*",
        "api/v1/tags",
        "api/v1/tags/*",
        "api/v1/playlists",
        "api/v1/playlists/*",
        "api/v1/playlists/*/entries",
        "api/v1/playlists/*/entries/*",
        "api/v1/playlists/*/entries/order",
        "api/v1/playlists/*/validate",
        "api/v1/playlists/*/copy",
        "api/v1/playlists/*/preview",
        "api/v1/processing-presets",
        "api/v1/processing-presets/*",
        "api/v1/processing-presets/*/*",
        "api/v1/processing-presets/*/*/*",
        "api/v1/segue-profiles",
        "api/v1/segue-profiles/*",
        "api/v1/transition-presets",
        "api/v1/transition-presets/*",
        "api/v1/notify",
        "api/v1/notify/channels",
        "api/v1/notify/channels/*",
        "api/v1/notify/channels/*/test",
        "api/v1/notify/digest",
        "api/v1/plex",
        "api/v1/plex/playlists",
        "api/v1/plex/import",
        "api/v1/plex/fill-art",
        "api/v1/plex/jobs",
        "api/v1/plex/jobs/*",
        "api/v1/notify/rules/*",
        "api/v1/display",
        "api/v1/display/meters",
        "api/v1/display/bar",
        "api/v1/stations",
        "api/v1/settings",
        "api/v1/settings/station/*",
        "api/v1/settings/global",
        "api/v1/system/restart",
        "api/v1/station-admin",
        "api/v1/station-admin/*",
        "api/v1/station-admin/*/*",
        "api/v1/stations/*/blocks",
        "api/v1/stations/*/blocks/*",
        "api/v1/schedule/week",
        "api/v1/transition-matrix",
    ];
    crate::station_http::shape_matches(SHAPES, path)
}

/// `now`-relative or absolute time text of the preview's query: RFC 3339, `YYYY-MM-DDTHH:MM[:SS]` or `HH:MM`
/// in the station's zone.
fn parse_t0(text: &str, zone: &common::timespec::Zone, now: i64) -> Option<i64> {
    if let Some(ms) = common::timespec::parse_rfc3339_ms(text) {
        return Some(ms);
    }
    if let Ok(dt) = text.parse::<common::timespec::CivilDateTime>() {
        return zone.compatible(dt);
    }
    // HH:MM (today).
    let (h, m) = text.split_once(':')?;
    let (h, m): (u8, u8) = (h.parse().ok()?, m.get(..2)?.parse().ok()?);
    let today = zone.date_string(now);
    format!("{today}T{h:02}:{m:02}:00")
        .parse::<common::timespec::CivilDateTime>()
        .ok()
        .and_then(|dt| zone.compatible(dt))
}

fn examples_json() -> Value {
    json!([{
        "id": "ita_monday",
        "title": "Eve Monday program",
        "description": "The Monday show: start by hand, pre-show music, a 7 s fade at T0 + 14:40, a console mute at T0 + 15:00, the elevator bed at T0 + 15:01, and a stop at 00:30 the next day.",
        "needs": [
            {"key": "pre_show_playlist_id", "label": "Pre-show playlist", "kind": "playlist"},
            {"key": "elevator_playlist_id", "label": "Elevator-bed playlist", "kind": "playlist"},
            {"key": "osc_host", "label": "Console address", "kind": "text", "default": "192.0.2.100"},
            {"key": "channel", "label": "Console channel to mute", "kind": "number", "default": 2}
        ]
    }])
}
