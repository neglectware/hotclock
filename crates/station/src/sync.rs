//! Library sync (architecture 5.2, "Change notification"): the station polls `PRAGMA data_version` of
//! its own read-only connection to `library.db` and, when it changes, reloads the playlists, blocks and
//! the tracks they reference. What it reads is copied into `state.db` (track cache and programming
//! cache), so a station that finds `library.db` missing keeps playing from the cache and a restart
//! without it still works.

use crate::catalog::Catalog;
use crate::programming::{Entry, Programming, Source};
use library::playlists::{read_playlists, read_station_blocks};
use library::tracks::{read_album_track_ids, read_category_track_ids, read_track, TrackRow};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct SyncOut {
    /// Rows that are new or changed (already applied to the catalog).
    pub changed_rows: Vec<TrackRow>,
    /// The programming differs from what the station had.
    pub programming: Option<Arc<Programming>>,
    /// `library.db` could not be read this time.
    pub unavailable: bool,
}

impl SyncOut {
    pub fn is_empty(&self) -> bool {
        self.changed_rows.is_empty() && self.programming.is_none()
    }
}

pub struct LibrarySync {
    path: PathBuf,
    station_id: String,
    conn: Option<Connection>,
    last_version: Option<i64>,
    /// The file the connection has open (device and inode): a `library.db` replaced under the same
    /// name (a restore, a rename over it) is a different file, and `data_version` of the old one would
    /// never change again.
    ident: Option<(u64, u64)>,
}

#[cfg(unix)]
fn file_ident(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_ident(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

impl LibrarySync {
    pub fn new(path: PathBuf, station_id: &str) -> Self {
        Self {
            path,
            station_id: station_id.into(),
            conn: None,
            last_version: None,
            ident: None,
        }
    }

    /// Open the connection if there is none. True when it was (re)opened just now: a new connection
    /// has its own `data_version` sequence, so the caller must reload rather than compare.
    fn open(&mut self) -> Option<bool> {
        let ident = file_ident(&self.path);
        if self.conn.is_some() && ident != self.ident {
            tracing::info!("library.db was replaced; reopening it");
            self.conn = None;
        }
        if self.conn.is_none() {
            let c = Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .ok()?;
            c.busy_timeout(std::time::Duration::from_millis(500)).ok()?;
            self.conn = Some(c);
            self.ident = ident;
            self.last_version = None;
            return Some(true);
        }
        Some(false)
    }

    /// Reload if the library changed since the last call (or `force`). Applies track rows to `cat` and
    /// returns what changed.
    pub fn poll(&mut self, force: bool, cat: &mut Catalog, current: &Programming) -> SyncOut {
        if !self.path.exists() {
            self.conn = None;
            return SyncOut {
                unavailable: true,
                ..SyncOut::default()
            };
        }
        let Some(reopened) = self.open() else {
            return SyncOut {
                unavailable: true,
                ..SyncOut::default()
            };
        };
        let force = force || reopened;
        let Some(conn) = self.conn.as_ref() else {
            return SyncOut::default();
        };
        let version: Option<i64> = conn.query_row("PRAGMA data_version", [], |r| r.get(0)).ok();
        if !force && version.is_some() && version == self.last_version {
            return SyncOut::default();
        }
        let out = match reload(conn, &self.station_id, cat, current) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("library sync failed, keeping the cache: {e}");
                // Reopen next time (a replaced file is a new inode).
                self.conn = None;
                return SyncOut {
                    unavailable: true,
                    ..SyncOut::default()
                };
            }
        };
        self.last_version = version;
        out
    }
}

/// The item ids the programming references: item entries, album blocks' tracks, and the members of the
/// category and playlist sources of blocks.
fn referenced_ids(conn: &Connection, prog: &Programming) -> rusqlite::Result<BTreeSet<i64>> {
    let mut ids = BTreeSet::new();
    // `play album` (4.7) can name any album the library holds, so every tagged album track is loaded.
    if let Ok(mut st) =
        conn.prepare("SELECT id FROM items WHERE album_key <> '' AND deleted_at IS NULL")
    {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, i64>(0)) {
            ids.extend(rows.flatten());
        }
    }
    let mut blocks: Vec<&crate::programming::Block> = prog.station_blocks.iter().collect();
    for pl in prog.playlists.values() {
        for e in &pl.entries {
            match e {
                Entry::Item { item_id, .. } => {
                    ids.insert(*item_id);
                }
                Entry::Album { album_key, .. } => {
                    ids.extend(read_album_track_ids(conn, album_key).unwrap_or_default());
                }
                Entry::Block(b) => blocks.push(b),
            }
        }
    }
    for b in blocks {
        for src in b.sources() {
            match src {
                Source::Category { id, name } => {
                    let cid = match (id, name) {
                        (Some(id), _) => Some(*id),
                        (None, Some(n)) => conn
                            .query_row(
                                "SELECT id FROM categories WHERE lower(name) = lower(?1)",
                                [n],
                                |r| r.get::<_, i64>(0),
                            )
                            .ok(),
                        _ => None,
                    };
                    if let Some(cid) = cid {
                        ids.extend(read_category_track_ids(conn, cid).unwrap_or_default());
                    }
                }
                Source::Playlist(_) => {
                    // Its items are among the playlists' entries already.
                }
                Source::Item(i) => {
                    ids.insert(*i);
                }
            }
        }
    }
    Ok(ids)
}

fn reload(
    conn: &Connection,
    station_id: &str,
    cat: &mut Catalog,
    current: &Programming,
) -> Result<SyncOut, String> {
    // One read transaction: the playlists and the tracks come from the same snapshot of the library
    // (an import writing between two reads could otherwise give a playlist naming an item whose row
    // is not read yet).
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let out = reload_in(&tx, station_id, cat, current);
    let _ = tx.rollback();
    out
}

fn reload_in(
    conn: &Connection,
    station_id: &str,
    cat: &mut Catalog,
    current: &Programming,
) -> Result<SyncOut, String> {
    let playlists = read_playlists(conn).map_err(|e| e.to_string())?;
    let station_blocks = read_station_blocks(conn, station_id).map_err(|e| e.to_string())?;
    let prog = Programming::from_rows(&playlists, &station_blocks);
    let ids = referenced_ids(conn, &prog).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for id in ids {
        match read_track(conn, id) {
            Ok(Some(r)) => rows.push(r),
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let changed = cat.apply(rows.clone());
    let changed_rows: Vec<TrackRow> = rows
        .into_iter()
        .filter(|r| changed.contains(&r.item_id))
        .collect();
    let programming = (prog != *current).then(|| Arc::new(prog));
    Ok(SyncOut {
        changed_rows,
        programming,
        unavailable: false,
    })
}
