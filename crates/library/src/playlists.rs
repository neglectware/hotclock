//! Playlists as ordered entry lists (architecture 4.1, 4.4, 4.5, 5.2): items, album blocks and
//! control blocks. M1.5 stores every block type the design lists; only `insert_every` is run by
//! the station yet (timed blocks are M3, the rest of the block engine M5).

use crate::db::Library;
use crate::util::now_iso;
use crate::{LibraryError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Shuffle,
    Sequential,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Shuffle => "shuffle",
            Mode::Sequential => "sequential",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewPlaylist {
    pub name: String,
    pub mode: Mode,
    pub no_repeat_minutes: i64,
    /// Advanced separation (4.3): `sep_title_minutes`, `sep_artist_minutes`, `sep_album_minutes`.
    pub separation: Value,
    pub station_id: Option<String>,
}

impl NewPlaylist {
    /// A shuffle playlist with the default 120-minute window.
    pub fn shuffle(name: &str) -> Self {
        Self {
            name: name.into(),
            mode: Mode::Shuffle,
            no_repeat_minutes: 120,
            separation: Value::Object(Default::default()),
            station_id: None,
        }
    }
}

/// A control block (4.5). `kind` is the block type (`insert_every`, `play`, `stop`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub trigger: String,
    pub priority: i64,
    pub params: Value,
    pub enabled: bool,
}

/// What is needed to create a block. Timed-block time fields arrive with M3.
#[derive(Debug, Clone, PartialEq)]
pub struct NewBlock {
    pub name: String,
    pub kind: String,
    pub trigger: String,
    pub priority: i64,
    pub params: Value,
}

impl NewBlock {
    /// A standing `insert_every` block (4.5): a liner from `source` every `n` songs.
    pub fn insert_every(name: &str, n: i64, source: Value) -> Self {
        Self {
            name: name.into(),
            kind: "insert_every".into(),
            trigger: "standing".into(),
            priority: 100,
            params: serde_json::json!({
                "n": n, "source": source, "count_kind": "music", "min_gap_minutes": 0,
                "resets_on": ["id"], "position": "between", "inherit": true,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntryKind {
    Item {
        item_id: i64,
    },
    Album {
        album_key: String,
        /// `gapless` or `normal` (3.7).
        continuity: String,
    },
    Block(BlockRow),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryRow {
    pub id: i64,
    pub position: i64,
    pub weight: f64,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRow {
    pub id: i64,
    pub name: String,
    pub mode: Mode,
    pub no_repeat_minutes: i64,
    pub separation: Value,
    pub station_blocks: bool,
    pub enabled: bool,
    pub entries: Vec<EntryRow>,
    /// The station the playlist belongs to (a program made for one station), or none.
    pub station_id: Option<String>,
}

fn block_from_row(r: &rusqlite::Row) -> rusqlite::Result<BlockRow> {
    let params_text: String = r.get(5)?;
    Ok(BlockRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        trigger: r.get(3)?,
        priority: r.get(4)?,
        params: serde_json::from_str(&params_text).unwrap_or(Value::Null),
        enabled: r.get::<_, i64>(6)? != 0,
    })
}

const BLOCK_COLS: &str = "b.id, b.name, b.type, b.trigger, b.priority, b.params, b.enabled";

/// Every playlist with its entries in order, read straight from the connection (the station
/// reads `library.db` with its own read-only connection).
pub fn read_playlists(conn: &Connection) -> Result<Vec<PlaylistRow>> {
    let mut out = Vec::new();
    let mut st = conn.prepare(
        "SELECT id, name, mode, no_repeat_minutes, separation, station_blocks, enabled, station_id
         FROM playlists ORDER BY id",
    )?;
    type Head = (i64, String, String, i64, String, i64, i64, Option<String>);
    let heads: Vec<Head> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (id, name, mode, nr, sep, sb, en, station_id) in heads {
        out.push(PlaylistRow {
            id,
            name,
            mode: if mode == "sequential" {
                Mode::Sequential
            } else {
                Mode::Shuffle
            },
            no_repeat_minutes: nr,
            separation: serde_json::from_str(&sep).unwrap_or(Value::Null),
            station_blocks: sb != 0,
            enabled: en != 0,
            entries: read_entries(conn, id)?,
            station_id,
        });
    }
    Ok(out)
}

fn read_entries(conn: &Connection, playlist_id: i64) -> Result<Vec<EntryRow>> {
    let sql = format!(
        "SELECT e.id, e.position, e.weight, e.item_id, e.album_key, e.continuity, {BLOCK_COLS}
         FROM playlist_entries e LEFT JOIN blocks b ON b.id = e.block_id
         WHERE e.playlist_id = ?1 ORDER BY e.position, e.id"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![playlist_id], |r| {
        let item_id: Option<i64> = r.get(3)?;
        let album_key: Option<String> = r.get(4)?;
        let continuity: String = r.get(5)?;
        let kind = if let Some(item_id) = item_id {
            EntryKind::Item { item_id }
        } else if let Some(album_key) = album_key {
            EntryKind::Album {
                album_key,
                continuity,
            }
        } else {
            let params_text: String = r.get(11)?;
            EntryKind::Block(BlockRow {
                id: r.get(6)?,
                name: r.get(7)?,
                kind: r.get(8)?,
                trigger: r.get(9)?,
                priority: r.get(10)?,
                params: serde_json::from_str(&params_text).unwrap_or(Value::Null),
                enabled: r.get::<_, i64>(12)? != 0,
            })
        };
        Ok(EntryRow {
            id: r.get(0)?,
            position: r.get(1)?,
            weight: r.get(2)?,
            kind,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The station blocks of `station_id` (4.4): blocks with no playlist, applied under every program.
pub fn read_station_blocks(conn: &Connection, station_id: &str) -> Result<Vec<BlockRow>> {
    let sql = format!("SELECT {BLOCK_COLS} FROM blocks b WHERE b.station_id = ?1 ORDER BY b.id");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![station_id], block_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

impl Library {
    pub fn create_playlist(&self, p: &NewPlaylist) -> Result<i64> {
        if p.name.trim().is_empty() {
            return Err(LibraryError::Invalid("a playlist needs a name".into()));
        }
        if p.no_repeat_minutes < 0 {
            return Err(LibraryError::Invalid(
                "no_repeat_minutes cannot be negative".into(),
            ));
        }
        self.conn.execute(
            "INSERT INTO playlists(name, station_id, kind, mode, no_repeat_minutes, separation, updated_at)
             VALUES(?1, ?2, 'static', ?3, ?4, ?5, ?6)",
            params![
                p.name,
                p.station_id,
                p.mode.name(),
                p.no_repeat_minutes,
                p.separation.to_string(),
                now_iso()
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Library::bump_revision(&self.conn)?;
        Ok(id)
    }

    fn next_position(&self, playlist_id: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_entries WHERE playlist_id = ?1",
            params![playlist_id],
            |r| r.get(0),
        )?)
    }

    pub fn add_item_to_playlist(&self, playlist_id: i64, item_id: i64, weight: f64) -> Result<i64> {
        let pos = self.next_position(playlist_id)?;
        self.conn.execute(
            "INSERT INTO playlist_entries(playlist_id, item_id, position, weight) VALUES(?1,?2,?3,?4)",
            params![playlist_id, item_id, pos, weight],
        )?;
        let id = self.conn.last_insert_rowid();
        Library::bump_revision(&self.conn)?;
        Ok(id)
    }

    /// An album block (3.7): `continuity` is `gapless` or `normal`.
    pub fn add_album_to_playlist(
        &self,
        playlist_id: i64,
        album_key: &str,
        continuity: &str,
    ) -> Result<i64> {
        if continuity != "gapless" && continuity != "normal" {
            return Err(LibraryError::Invalid(format!(
                "continuity must be gapless or normal, got `{continuity}`"
            )));
        }
        let pos = self.next_position(playlist_id)?;
        self.conn.execute(
            "INSERT INTO playlist_entries(playlist_id, album_key, continuity, position) VALUES(?1,?2,?3,?4)",
            params![playlist_id, album_key, continuity, pos],
        )?;
        let id = self.conn.last_insert_rowid();
        Library::bump_revision(&self.conn)?;
        Ok(id)
    }

    /// Append a control block to a playlist; returns the block id.
    pub fn add_block_to_playlist(&self, playlist_id: i64, b: &NewBlock) -> Result<i64> {
        let block_id = self.insert_block(None, b)?;
        let pos = self.next_position(playlist_id)?;
        self.conn.execute(
            "INSERT INTO playlist_entries(playlist_id, block_id, position) VALUES(?1,?2,?3)",
            params![playlist_id, block_id, pos],
        )?;
        Library::bump_revision(&self.conn)?;
        Ok(block_id)
    }

    /// A station block (4.4): standing and timed side blocks under every program.
    pub fn add_station_block(&self, station_id: &str, b: &NewBlock) -> Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO stations(id, display_name) VALUES(?1, ?1)",
            params![station_id],
        )?;
        let id = self.insert_block(Some(station_id), b)?;
        Library::bump_revision(&self.conn)?;
        Ok(id)
    }

    fn insert_block(&self, station: Option<&str>, b: &NewBlock) -> Result<i64> {
        if b.kind == "insert_every" {
            let n = b.params.get("n").and_then(Value::as_i64).unwrap_or(0);
            if n < 1 {
                return Err(LibraryError::Invalid(
                    "an insert_every block needs n of at least 1".into(),
                ));
            }
        }
        self.conn.execute(
            "INSERT INTO blocks(station_id, name, type, trigger, priority, params, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                station,
                b.name,
                b.kind,
                b.trigger,
                b.priority,
                b.params.to_string(),
                now_iso()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn remove_playlist_entry(&self, entry_id: i64) -> Result<()> {
        let block: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT block_id FROM playlist_entries WHERE id = ?1",
                params![entry_id],
                |r| r.get(0),
            )
            .optional()?;
        self.conn.execute(
            "DELETE FROM playlist_entries WHERE id = ?1",
            params![entry_id],
        )?;
        if let Some(Some(b)) = block {
            self.conn
                .execute("DELETE FROM blocks WHERE id = ?1", params![b])?;
        }
        Library::bump_revision(&self.conn)?;
        Ok(())
    }

    pub fn playlists(&self) -> Result<Vec<PlaylistRow>> {
        read_playlists(&self.conn)
    }

    pub fn playlist_id(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM playlists WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn category_id(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM categories WHERE lower(name) = lower(?1)",
                params![name],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Rename an item's title (what the item editor does; tests use it for library sync).
    pub fn set_item_title(&self, item_id: i64, title: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE items SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![item_id, title, now_iso()],
        )?;
        if n == 0 {
            return Err(LibraryError::NoItem(item_id));
        }
        Library::bump_revision(&self.conn)?;
        Ok(())
    }
}
