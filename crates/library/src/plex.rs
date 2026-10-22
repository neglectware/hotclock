//! `import-plex` (architecture 5.4, milestone M2): a one-time, read-only copy of Plex playlists into the
//! library. The only requests made are `GET`s: `/identity`, `/playlists`, `/playlists/{key}/items` and the
//! part download Plex offers to its own apps (`/library/parts/.../file.ext?download=1`). Nothing is written to
//! the Plex server. The token travels in the `X-Plex-Token` header, never in a URL, and is never printed.
//!
//! The client is a few dozen lines of HTTP/1.1 over `std::net` (the library service has no HTTP client and
//! this runs once, from the command line). It speaks plain `http://`, which is what a Plex Media Server on a
//! LAN offers; a server that insists on TLS is refused with a message that says so.

use crate::art;
use crate::db::Library;
use crate::import::{import_file, ImportOptions, ImportOutcome};
use crate::playlists::{EntryKind, Mode, NewPlaylist};
use crate::util::now_iso;
use crate::{LibraryError, Result};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Plex pages its lists; this many entries per request.
const PAGE: usize = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct PlexPlaylist {
    pub rating_key: String,
    pub title: String,
    pub playlist_type: String,
    pub smart: bool,
    pub leaf_count: i64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlexTrack {
    pub rating_key: String,
    pub title: String,
    /// The track artist: Plex's `originalTitle` when it differs from the album artist, else the album artist
    /// (`grandparentTitle`). The same field the file's artist tag fills.
    pub artist: String,
    pub album: String,
    pub year: Option<i64>,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: i64,
    /// The part's download path, `/library/parts/<id>/<changestamp>/file.<ext>`.
    pub part_key: String,
    /// The file name Plex knows the part by (the last component of `Part.file`).
    pub file_name: String,
    pub size: i64,
    /// The album's picture on the server: `parentThumb`, else `thumb`, else `grandparentThumb`.
    pub thumb: Option<String>,
}

#[derive(Debug)]
pub struct PlexClient {
    host: String,
    port: u16,
    base: String,
    token: String,
}

struct Response {
    status: u16,
    reader: BufReader<TcpStream>,
    length: Option<u64>,
    chunked: bool,
}

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(LibraryError::Invalid(m.into()))
}

/// Read a token file: the first non-empty line, trimmed. The file may also be the `Preferences.xml` style
/// `PlexOnlineToken="..."` or `X-Plex-Token=...`; the bare token is the documented form.
pub fn read_token_file(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        LibraryError::Invalid(format!(
            "cannot read the token file {}: {e}",
            path.display()
        ))
    })?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty());
    let Some(line) = line else {
        return err(format!("the token file {} is empty", path.display()));
    };
    let t = line
        .trim_start_matches("X-Plex-Token=")
        .trim_matches(|c| c == '"' || c == '\'');
    if t.is_empty() || t.contains(char::is_whitespace) {
        return err(format!(
            "the token file {} should hold the token alone on its first line",
            path.display()
        ));
    }
    Ok(t.to_string())
}

impl PlexClient {
    /// `server` is `http://host:32400`, or `host:32400` (plain HTTP is assumed).
    pub fn new(server: &str, token: String) -> Result<PlexClient> {
        let s = server.trim();
        if s.starts_with("https://") {
            return err("this importer speaks plain http:// only; use the server's LAN address on port 32400");
        }
        let s = s.strip_prefix("http://").unwrap_or(s).trim_end_matches('/');
        let (hostport, base) = match s.find('/') {
            Some(i) => (&s[..i], s[i..].to_string()),
            None => (s, String::new()),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| LibraryError::Invalid(format!("bad port in `{server}`")))?,
            ),
            None => (hostport.to_string(), 32400),
        };
        if host.is_empty() {
            return err(format!("no host in `{server}`"));
        }
        Ok(PlexClient {
            host,
            port,
            base,
            token,
        })
    }

    fn request(&self, path: &str, extra: &[(&str, String)]) -> Result<Response> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| LibraryError::Invalid(format!("cannot resolve {}: {e}", self.host)))?
            .next()
            .ok_or_else(|| LibraryError::Invalid(format!("no address for {}", self.host)))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| {
            LibraryError::Invalid(format!(
                "cannot connect to {}:{}: {e}",
                self.host, self.port
            ))
        })?;
        tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
        let mut req = format!(
            "GET {}{} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n\
             User-Agent: hotclock-import-plex\r\nX-Plex-Product: hotclock-import-plex\r\nX-Plex-Client-Identifier: hotclock-import-plex\r\n",
            self.base, path, self.host, self.port
        );
        if !extra.iter().any(|(k, _)| *k == "Accept") {
            req.push_str("Accept: application/json\r\n");
        }
        if !self.token.is_empty() {
            req.push_str(&format!("X-Plex-Token: {}\r\n", self.token));
        }
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        (&tcp).write_all(req.as_bytes())?;
        let mut reader = BufReader::new(tcp);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                LibraryError::Invalid(format!("not an HTTP answer: `{}`", line.trim()))
            })?;
        let (mut length, mut chunked) = (None, false);
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-length" => length = v.trim().parse().ok(),
                    "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
                    _ => {}
                }
            }
        }
        if status == 401 || status == 403 {
            return err(format!(
                "Plex answered {status}: the token in the token file was refused (is it current, and does it belong to the server's owner?)"
            ));
        }
        if status >= 400 {
            return err(format!("Plex answered {status} for {path}"));
        }
        if (300..400).contains(&status) {
            return err(format!(
                "Plex answered {status} (a redirect) for {path}; this importer does not follow redirects"
            ));
        }
        Ok(Response {
            status,
            reader,
            length,
            chunked,
        })
    }

    fn body(r: &mut Response) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        Self::copy_body(r, &mut out)?;
        Ok(out)
    }

    /// Copy the body to `w`; returns the byte count.
    fn copy_body(r: &mut Response, w: &mut dyn Write) -> Result<u64> {
        let mut total = 0u64;
        let mut buf = vec![0u8; 256 * 1024];
        if r.chunked {
            loop {
                let mut sz = String::new();
                if r.reader.read_line(&mut sz)? == 0 {
                    return err("the connection closed inside a chunked body");
                }
                let n = usize::from_str_radix(sz.trim().split(';').next().unwrap_or(""), 16)
                    .map_err(|_| LibraryError::Invalid("bad chunk size".into()))?;
                if n == 0 {
                    let mut t = String::new();
                    let _ = r.reader.read_line(&mut t);
                    break;
                }
                let mut left = n;
                while left > 0 {
                    let want = left.min(buf.len());
                    let k = r.reader.read(&mut buf[..want])?;
                    if k == 0 {
                        return err("the connection closed inside a chunk");
                    }
                    w.write_all(&buf[..k])?;
                    total += k as u64;
                    left -= k;
                }
                let mut crlf = String::new();
                r.reader.read_line(&mut crlf)?;
            }
        } else {
            let mut left = r.length;
            loop {
                let want = match left {
                    Some(0) => break,
                    Some(l) => (l as usize).min(buf.len()),
                    None => buf.len(),
                };
                let k = r.reader.read(&mut buf[..want])?;
                if k == 0 {
                    if let Some(l) = left {
                        if l > 0 {
                            return err(format!("the download stopped {l} bytes short"));
                        }
                    }
                    break;
                }
                w.write_all(&buf[..k])?;
                total += k as u64;
                if let Some(l) = left.as_mut() {
                    *l -= k as u64;
                }
            }
        }
        Ok(total)
    }

    fn json(&self, path: &str, extra: &[(&str, String)]) -> Result<Value> {
        let mut r = self.request(path, extra)?;
        let body = Self::body(&mut r)?;
        serde_json::from_slice(&body).map_err(|e| {
            LibraryError::Invalid(format!(
                "Plex's answer to {path} is not JSON ({e}); status {}",
                r.status
            ))
        })
    }

    /// `/identity` needs no token: the machine id and API version.
    pub fn identity(&self) -> Result<Value> {
        let v = self.json("/identity", &[])?;
        Ok(v["MediaContainer"].clone())
    }

    fn paged(&self, path: &str) -> Result<Vec<Value>> {
        let mut out: Vec<Value> = Vec::new();
        loop {
            let v = self.json(
                path,
                &[
                    ("X-Plex-Container-Start", out.len().to_string()),
                    ("X-Plex-Container-Size", PAGE.to_string()),
                ],
            )?;
            let mc = &v["MediaContainer"];
            let items = mc["Metadata"].as_array().cloned().unwrap_or_default();
            let n = items.len();
            out.extend(items);
            // `totalSize` comes with paged answers; a server that ignores paging sends everything at once.
            let total = mc["totalSize"]
                .as_i64()
                .or_else(|| mc["size"].as_i64())
                .unwrap_or(out.len() as i64);
            if n == 0 || out.len() as i64 >= total {
                break;
            }
        }
        Ok(out)
    }

    pub fn playlists(&self) -> Result<Vec<PlexPlaylist>> {
        Ok(self
            .paged("/playlists?playlistType=audio")?
            .iter()
            .map(|m| PlexPlaylist {
                rating_key: sval(&m["ratingKey"]),
                title: sval(&m["title"]),
                playlist_type: sval(&m["playlistType"]),
                smart: m["smart"]
                    .as_bool()
                    .unwrap_or_else(|| sval(&m["smart"]) == "1"),
                leaf_count: ival(&m["leafCount"]).unwrap_or(0),
                duration_ms: ival(&m["duration"]).unwrap_or(0),
            })
            .collect())
    }

    /// The tracks of a playlist, in playlist order. Entries that are not audio tracks with a part are skipped
    /// and returned by name in the second value.
    pub fn playlist_items(&self, rating_key: &str) -> Result<(Vec<PlexTrack>, Vec<String>)> {
        let mut tracks = vec![];
        let mut skipped = vec![];
        for m in self.paged(&format!("/playlists/{rating_key}/items"))? {
            let t = parse_track(&m);
            if t.part_key.is_empty() {
                skipped.push(format!("{} (no media part)", t.title));
            } else {
                tracks.push(t);
            }
        }
        Ok((tracks, skipped))
    }

    /// One track's metadata by rating key (the part may be absent): what the art backfill reads.
    pub fn track_meta(&self, rating_key: &str) -> Result<PlexTrack> {
        let v = self.json(&format!("/library/metadata/{rating_key}"), &[])?;
        let m = v["MediaContainer"]["Metadata"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| LibraryError::Invalid(format!("Plex has no item {rating_key}")))?;
        Ok(parse_track(m))
    }

    /// A picture the server holds (a `thumb` path), at most [`IMAGE_LIMIT`] bytes.
    pub fn image(&self, path: &str) -> Result<Vec<u8>> {
        let mut r = self.request(path, &[("Accept", "image/*".to_string())])?;
        if r.length.is_some_and(|l| l > IMAGE_LIMIT as u64) {
            return err(format!("the picture at {path} is over {IMAGE_LIMIT} bytes"));
        }
        let mut out = Capped(Vec::new());
        Self::copy_body(&mut r, &mut out)?;
        Ok(out.0)
    }

    /// Download one part to `dest` through Plex's download endpoint; returns the bytes written.
    pub fn download(&self, part_key: &str, dest: &Path) -> Result<u64> {
        let sep = if part_key.contains('?') { '&' } else { '?' };
        let mut r = self.request(&format!("{part_key}{sep}download=1"), &[])?;
        let mut f = std::fs::File::create(dest)?;
        let n = Self::copy_body(&mut r, &mut f);
        let n = match n {
            Ok(n) => n,
            Err(e) => {
                drop(f);
                let _ = std::fs::remove_file(dest);
                return Err(e);
            }
        };
        f.sync_all()?;
        Ok(n)
    }
}

/// A picture is refused above this (the same limit as embedded art).
const IMAGE_LIMIT: usize = crate::art::MAX_BYTES;

struct Capped(Vec<u8>);
impl Write for Capped {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + b.len() > IMAGE_LIMIT {
            return Err(std::io::Error::other("the picture is too large"));
        }
        self.0.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A track from a Plex `Metadata` entry. `part_key` is empty when the entry has no media part.
fn parse_track(m: &Value) -> PlexTrack {
    let part = m["Media"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|md| md["Part"].as_array())
        .and_then(|p| p.first())
        .cloned()
        .filter(|p| p["key"].is_string())
        .unwrap_or(Value::Null);
    let album_artist = sval(&m["grandparentTitle"]);
    let orig = sval(&m["originalTitle"]);
    let file = sval(&part["file"]);
    let file_name = file
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let key = sval(&part["key"]);
            key.rsplit('/').next().unwrap_or("track").to_string()
        });
    let thumb = ["parentThumb", "thumb", "grandparentThumb"]
        .iter()
        .map(|k| sval(&m[*k]))
        .find(|t| t.starts_with('/'));
    PlexTrack {
        rating_key: sval(&m["ratingKey"]),
        title: sval(&m["title"]),
        artist: if orig.is_empty() { album_artist } else { orig },
        album: sval(&m["parentTitle"]),
        year: ival(&m["parentYear"]).or_else(|| ival(&m["year"])),
        track_no: ival(&m["index"]),
        disc_no: ival(&m["parentIndex"]),
        duration_ms: ival(&m["duration"]).unwrap_or(0),
        part_key: sval(&part["key"]),
        file_name,
        size: ival(&part["size"]).unwrap_or(0),
        thumb,
    }
}

fn sval(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn ival(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// A file name that is safe as a path component.
fn safe_name(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '\0' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let t = t.trim_matches('.').trim().to_string();
    if t.is_empty() {
        "track".into()
    } else {
        t
    }
}

/// Where progress lines go as they happen (the web UI's job log); the report keeps them too.
#[derive(Clone, Default)]
pub struct Sink(pub Option<LineFn>);

pub type LineFn = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

#[derive(Debug, Clone)]
pub struct PlexImportOptions {
    /// Category for every item (a category name; default `music`).
    pub category: Option<String>,
    /// The library playlist's mode when it is created.
    pub mode: Mode,
    /// Name the library playlist differently from Plex's.
    pub as_name: Option<String>,
    /// List what would be downloaded and added; change nothing (the library is only read).
    pub dry_run: bool,
    /// Fill the album picture from Plex for items whose file has none (default on).
    pub fill_art: bool,
    pub on_line: Sink,
}

impl Default for PlexImportOptions {
    fn default() -> Self {
        Self {
            category: None,
            mode: Mode::Sequential,
            as_name: None,
            dry_run: false,
            fill_art: true,
            on_line: Sink::default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct PlexImportReport {
    pub playlist: String,
    pub library_playlist_id: Option<i64>,
    pub playlist_created: bool,
    pub tracks: usize,
    pub downloaded: usize,
    pub imported: usize,
    /// Already in the library (by Plex rating key, or the same file under another source).
    pub known: usize,
    pub failed: Vec<String>,
    pub entries_added: usize,
    pub entries_present: usize,
    pub metadata_filled: usize,
    pub skipped: Vec<String>,
    pub lines: Vec<String>,
    pub art: ArtStats,
    sink: Sink,
}

impl PlexImportReport {
    fn say(&mut self, line: String) {
        if let Some(f) = &self.sink.0 {
            f(&line);
        }
        self.lines.push(line);
    }
}

/// What a picture fill did, item by item.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct ArtStats {
    pub checked: usize,
    /// The item already had a picture (from its file, a folder image, or an earlier fill): left alone.
    pub had_art: usize,
    pub filled: usize,
    /// A dry run: Plex has a picture for these.
    pub would_fill: usize,
    pub plex_none: usize,
    pub failed: usize,
    /// Titles, artists, albums or years taken from Plex (a dry run: that would be).
    pub meta_filled: usize,
    pub not_in_library: usize,
    pub errors: Vec<String>,
}

fn known_item(lib: &Library, rating_key: &str) -> Result<Option<i64>> {
    Ok(lib
        .conn
        .query_row(
            "SELECT id FROM items WHERE deleted_at IS NULL AND (source = ?1 OR source LIKE ?1 || ';%') ORDER BY id LIMIT 1",
            params![format!("plex:{rating_key}")],
            |r| r.get(0),
        )
        .optional()?)
}

/// Where downloads wait before they are imported: beside the database, outside `upload-tmp/` (which opening
/// the library empties).
fn download_dir(lib: &Library) -> PathBuf {
    lib.paths
        .db
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("plex-download")
}

/// Fill what the file's own tags left empty from Plex, so the library shows what Plexamp showed: the title
/// when it is the file name (the file had no title tag), the artist, the year, and the album's display text
/// when they are empty. The album's key is left alone (it joins album runs). Returns whether anything was (a
/// dry run: would be) changed.
fn fill_meta(
    lib: &Library,
    item_id: i64,
    t: &PlexTrack,
    file_name: &str,
    dry: bool,
) -> Result<bool> {
    let (title, artist, album, year): (String, String, String, Option<i64>) = lib.conn.query_row(
        "SELECT title, artist, album, year FROM items WHERE id = ?1",
        params![item_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let path = Path::new(file_name);
    let is_derived = |t: &str| {
        t == crate::tags::title_from_filename(path, true)
            || t == crate::tags::title_from_filename(path, false)
    };
    let mut sets: Vec<(&str, rusqlite::types::Value)> = vec![];
    if !t.title.is_empty() && (title.is_empty() || is_derived(&title)) && title != t.title {
        sets.push(("title", t.title.clone().into()));
    }
    if artist.is_empty() && !t.artist.is_empty() {
        sets.push(("artist", t.artist.clone().into()));
    }
    if album.is_empty() && !t.album.is_empty() {
        sets.push(("album", t.album.clone().into()));
    }
    if let (None, Some(y)) = (year, t.year) {
        if (1000..3000).contains(&y) {
            sets.push(("year", y.into()));
        }
    }
    if sets.is_empty() || dry {
        return Ok(!sets.is_empty());
    }
    for (col, v) in sets {
        lib.conn.execute(
            &format!("UPDATE items SET {col} = ?2, updated_at = ?3 WHERE id = ?1"),
            params![item_id, v, now_iso()],
        )?;
    }
    Library::bump_revision(&lib.conn)?;
    Ok(true)
}

/// Pictures fetched in one run, by the thumb path (the tracks of an album share one).
#[derive(Default)]
pub struct ArtCache(HashMap<String, std::result::Result<std::sync::Arc<Vec<u8>>, String>>);

pub enum ArtOutcome {
    HadArt,
    Filled,
    WouldFill,
    PlexNone,
    Failed(String),
}

/// Give an item the picture Plex holds for its album, when the item has none. Never replaces a picture: the
/// update names `art_sha IS NULL`, so even a picture that arrived a moment ago stays. Stored the way embedded
/// art is (normalized, shared by hash), with the origin `plex`.
pub fn fill_item_art(
    lib: &Library,
    plex: &PlexClient,
    item_id: i64,
    thumb: Option<&str>,
    cache: &mut ArtCache,
    dry: bool,
) -> ArtOutcome {
    let has: Option<Option<String>> = lib
        .conn
        .query_row("SELECT art_sha FROM items WHERE id = ?1", [item_id], |r| {
            r.get(0)
        })
        .optional()
        .unwrap_or(None);
    match has {
        Some(None) => {}
        Some(Some(_)) => return ArtOutcome::HadArt,
        None => return ArtOutcome::Failed(format!("no item {item_id}")),
    }
    let Some(thumb) = thumb else {
        return ArtOutcome::PlexNone;
    };
    let got = cache
        .0
        .entry(thumb.to_string())
        .or_insert_with(|| {
            plex.image(thumb)
                .map(std::sync::Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone();
    let bytes = match got {
        Ok(b) => b,
        // A picture Plex does not have (404) is "none", not a failure.
        Err(e) if e.contains("answered 404") => return ArtOutcome::PlexNone,
        Err(e) => return ArtOutcome::Failed(e),
    };
    let src = art::ArtSource {
        bytes: bytes.to_vec(),
        origin: "plex",
    };
    if dry {
        // Decodable and within the limits, without storing it.
        return match image::ImageReader::new(std::io::Cursor::new(&src.bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())
            .and_then(|r| r.into_dimensions().map_err(|e| e.to_string()))
        {
            Ok((w, h)) if w > 0 && h > 0 && w <= art::MAX_SIDE && h <= art::MAX_SIDE => {
                ArtOutcome::WouldFill
            }
            Ok((w, h)) => ArtOutcome::Failed(format!("the picture is {w} x {h} px")),
            Err(e) => ArtOutcome::Failed(format!("not a picture: {e}")),
        };
    }
    let n = match art::store_art(&lib.paths.media, &lib.paths.upload_tmp, &src) {
        Ok(n) => n,
        Err(e) => return ArtOutcome::Failed(e),
    };
    let r = (|| -> Result<usize> {
        let tx = lib.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO artwork(sha256, origin, src_w, src_h, bytes_600, bytes_144) VALUES(?1,?2,?3,?4,?5,?6)",
            params![n.sha, n.origin, n.src_w, n.src_h, n.bytes_600 as i64, n.bytes_144 as i64],
        )?;
        let changed = tx.execute(
            "UPDATE items SET art_sha = ?2, updated_at = ?3 WHERE id = ?1 AND art_sha IS NULL",
            params![item_id, n.sha, now_iso()],
        )?;
        tx.commit()?;
        Ok(changed)
    })();
    match r {
        Ok(1) => {
            let _ = Library::bump_revision(&lib.conn);
            ArtOutcome::Filled
        }
        Ok(_) => ArtOutcome::HadArt,
        Err(e) => ArtOutcome::Failed(e.to_string()),
    }
}

impl ArtStats {
    fn count(&mut self, o: &ArtOutcome, label: &str) -> Option<String> {
        self.checked += 1;
        match o {
            ArtOutcome::HadArt => {
                self.had_art += 1;
                None
            }
            ArtOutcome::Filled => {
                self.filled += 1;
                Some(format!("art       {label}: filled from Plex"))
            }
            ArtOutcome::WouldFill => {
                self.would_fill += 1;
                Some(format!("art       {label}: Plex has a picture"))
            }
            ArtOutcome::PlexNone => {
                self.plex_none += 1;
                Some(format!("art       {label}: Plex has none"))
            }
            ArtOutcome::Failed(e) => {
                self.failed += 1;
                self.errors.push(format!("{label}: {e}"));
                Some(format!("FAILED    art {label}: {e}"))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum FillScope {
    /// The items of this Plex playlist that are in the library.
    Playlist(String),
    /// Every item imported from Plex.
    AllPlexItems,
}

/// The backfill: walk items imported from Plex and give those without a picture the one Plex holds (and fill
/// blank title, artist, album and year). Idempotent: an item with a picture is only counted.
pub fn fill_art(
    lib: &Library,
    plex: &PlexClient,
    scope: &FillScope,
    dry_run: bool,
    on_line: &Sink,
) -> Result<ArtStats> {
    let mut stats = ArtStats::default();
    let mut cache = ArtCache::default();
    let say = |l: String| {
        if let Some(f) = &on_line.0 {
            f(&l);
        }
    };
    // (item id, rating key, label, the track when the scope already read it from Plex)
    let mut work: Vec<(i64, String, String, Option<PlexTrack>)> = vec![];
    match scope {
        FillScope::AllPlexItems => {
            let mut st = lib.conn.prepare(
                "SELECT id, source, artist, title FROM items WHERE deleted_at IS NULL AND source LIKE 'plex:%' ORDER BY id",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            for r in rows.flatten() {
                let rk = r.1["plex:".len()..]
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .to_string();
                work.push((r.0, rk, format!("{} / {}", r.2, r.3), None));
            }
        }
        FillScope::Playlist(name) => {
            let lists = plex.playlists()?;
            let pl = lists
                .iter()
                .find(|p| p.title.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    LibraryError::Invalid(format!("Plex has no audio playlist called `{name}`"))
                })?;
            let (tracks, _) = plex.playlist_items(&pl.rating_key)?;
            for t in tracks {
                match known_item(lib, &t.rating_key)? {
                    Some(id) => work.push((
                        id,
                        t.rating_key.clone(),
                        format!("{} / {}", t.artist, t.title),
                        Some(t),
                    )),
                    None => {
                        stats.not_in_library += 1;
                        say(format!("not in the library: {} / {}", t.artist, t.title));
                    }
                }
            }
        }
    }
    for (id, rk, label, known) in work {
        // Items that have a picture need no request to Plex.
        let has_art: bool = lib
            .conn
            .query_row(
                "SELECT art_sha IS NOT NULL FROM items WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap_or(false);
        let file_name: String = lib
            .conn
            .query_row("SELECT orig_filename FROM items WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap_or_default();
        let (track, thumb_err) = match known {
            Some(t) => (Some(t), None),
            None => match plex.track_meta(&rk) {
                Ok(t) => (Some(t), None),
                Err(e) => (None, Some(e.to_string())),
            },
        };
        let outcome = if has_art {
            ArtOutcome::HadArt
        } else if let Some(e) = thumb_err {
            ArtOutcome::Failed(e)
        } else {
            let thumb = track.as_ref().and_then(|t| t.thumb.clone());
            fill_item_art(lib, plex, id, thumb.as_deref(), &mut cache, dry_run)
        };
        if let Some(l) = stats.count(&outcome, &label) {
            say(l);
        }
        if let Some(t) = &track {
            if fill_meta(lib, id, t, &file_name, dry_run)? {
                stats.meta_filled += 1;
                say(format!(
                    "meta      {label}: {} from Plex",
                    if dry_run { "would be filled" } else { "filled" }
                ));
            }
        }
    }
    Ok(stats)
}

/// Import one playlist. Re-running adds nothing: a track already imported from the same Plex rating key is
/// not downloaded again, and an item already in the library playlist is not added again.
pub fn import_playlist(
    lib: &Library,
    plex: &PlexClient,
    name: &str,
    opts: &PlexImportOptions,
) -> Result<PlexImportReport> {
    let lists = plex.playlists()?;
    let wanted: Vec<&PlexPlaylist> = lists
        .iter()
        .filter(|p| p.title.eq_ignore_ascii_case(name))
        .collect();
    let pl = match wanted.as_slice() {
        [one] => (*one).clone(),
        [] => {
            let names: Vec<String> = lists.iter().map(|p| format!("`{}`", p.title)).collect();
            return err(format!(
                "Plex has no audio playlist called `{name}`; it has: {}",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            ));
        }
        many => {
            let keys: Vec<String> = many.iter().map(|p| p.rating_key.clone()).collect();
            return err(format!(
                "{} Plex playlists are called `{name}` (rating keys {}); rename one",
                many.len(),
                keys.join(", ")
            ));
        }
    };
    let (tracks, skipped) = plex.playlist_items(&pl.rating_key)?;
    let lib_name = opts.as_name.clone().unwrap_or_else(|| pl.title.clone());
    let mut rep = PlexImportReport {
        playlist: lib_name.clone(),
        tracks: tracks.len(),
        skipped,
        sink: opts.on_line.clone(),
        ..Default::default()
    };
    let mut art_cache = ArtCache::default();
    let dl_dir = download_dir(lib);
    if !opts.dry_run {
        std::fs::create_dir_all(&dl_dir)?;
    }
    let mut item_ids: Vec<(usize, i64)> = vec![];
    for (i, t) in tracks.iter().enumerate() {
        let label = format!("{:>3}. {} / {}", i + 1, t.artist, t.title);
        if let Some(id) = known_item(lib, &t.rating_key)? {
            rep.known += 1;
            rep.say(format!("known     {label}  item {id}"));
            item_ids.push((i, id));
            continue;
        }
        if opts.dry_run {
            rep.say(format!("would get {label}  ({} bytes)", t.size));
            continue;
        }
        // The file keeps Plex's name so an untagged track gets the same title Plex derived it from; one
        // directory per rating key keeps equal names apart.
        let sub = dl_dir.join(safe_name(&t.rating_key));
        std::fs::create_dir_all(&sub)?;
        let local = sub.join(safe_name(&t.file_name));
        let got = plex.download(&t.part_key, &local);
        let bytes = match got {
            Ok(n) => n,
            Err(e) => {
                rep.failed.push(format!("{label}: download: {e}"));
                rep.say(format!("FAILED    {label}: {e}"));
                let _ = std::fs::remove_dir_all(&sub);
                continue;
            }
        };
        rep.downloaded += 1;
        let outcome = import_file(
            lib,
            &local,
            &ImportOptions {
                category: opts.category.clone(),
                source: format!("plex:{}", t.rating_key),
            },
        );
        let name_on_disk = local
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let _ = std::fs::remove_dir_all(&sub);
        match outcome {
            ImportOutcome::Done {
                item_id, warnings, ..
            } => {
                rep.imported += 1;
                rep.say(format!(
                    "imported  {label}  item {item_id}  ({bytes} bytes)"
                ));
                for w in warnings {
                    rep.say(format!("  warning: {w}"));
                }
                if fill_meta(lib, item_id, t, &name_on_disk, false)? {
                    rep.metadata_filled += 1;
                }
                item_ids.push((i, item_id));
            }
            ImportOutcome::Duplicate { item_id } => {
                rep.known += 1;
                rep.say(format!("same file  {label}  is item {item_id}"));
                item_ids.push((i, item_id));
            }
            ImportOutcome::Failed { error } => {
                rep.failed.push(format!("{label}: {error}"));
                rep.say(format!("FAILED    {label}: {error}"));
            }
        }
    }
    if !opts.dry_run {
        let _ = std::fs::remove_dir(&dl_dir);
    }

    // Pictures: an item whose file gave none gets the album's picture from Plex (never replacing one).
    if opts.fill_art {
        for (i, id) in &item_ids {
            let t = &tracks[*i];
            let label = format!("{:>3}. {} / {}", i + 1, t.artist, t.title);
            let o = fill_item_art(
                lib,
                plex,
                *id,
                t.thumb.as_deref(),
                &mut art_cache,
                opts.dry_run,
            );
            if let Some(l) = rep.art.count(&o, &label) {
                rep.say(l);
            }
            if opts.dry_run && fill_meta(lib, *id, t, &t.file_name, true)? {
                rep.metadata_filled += 1;
            }
        }
    }

    // The library playlist: found by name or created, then every item not yet in it appended in Plex's order.
    let existing = lib.playlist_id(&lib_name)?;
    if opts.dry_run {
        rep.library_playlist_id = existing;
        return Ok(rep);
    }
    let pid = match existing {
        Some(id) => id,
        None => {
            let mut p = NewPlaylist::shuffle(&lib_name);
            p.mode = opts.mode;
            rep.playlist_created = true;
            lib.create_playlist(&p)?
        }
    };
    rep.library_playlist_id = Some(pid);
    let mut present: HashSet<i64> = lib
        .playlists()?
        .into_iter()
        .find(|p| p.id == pid)
        .map(|p| {
            p.entries
                .iter()
                .filter_map(|e| match e.kind {
                    EntryKind::Item { item_id } => Some(item_id),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    for (_, id) in item_ids {
        if present.contains(&id) {
            // Already there from an earlier run, or the same item twice in Plex's list.
            rep.entries_present += 1;
            continue;
        }
        lib.add_item_to_playlist(pid, id, 1.0)?;
        present.insert(id);
        rep.entries_added += 1;
    }
    Ok(rep)
}
