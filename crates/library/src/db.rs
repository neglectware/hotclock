//! `library.db`: opening, migrations and the seed rows (architecture 5.2).

use crate::{util::now_iso, LibraryError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SCHEMA_VERSION: i64 = 6;
const SCHEMA_V1: &str = include_str!("schema_v1.sql");
const SCHEMA_V2: &str = include_str!("schema_v2.sql");
const SCHEMA_V3: &str = include_str!("schema_v3.sql");
const SCHEMA_V4: &str = include_str!("schema_v4.sql");
const SCHEMA_V5: &str = include_str!("schema_v5.sql");
const SCHEMA_V6: &str = include_str!("schema_v6.sql");

/// Where everything lives (architecture 5.1).
#[derive(Debug, Clone)]
pub struct Paths {
    pub db: PathBuf,
    pub media: PathBuf,
    pub upload_tmp: PathBuf,
    pub inbox: PathBuf,
    pub backup: PathBuf,
}

impl Paths {
    pub fn from_data_dir(dir: &Path) -> Paths {
        Paths {
            db: dir.join("library.db"),
            media: dir.join("media"),
            upload_tmp: dir.join("upload-tmp"),
            inbox: dir.join("inbox"),
            backup: dir.join("backup"),
        }
    }

    pub fn from_config(cfg: &common::config::GlobalConfig) -> Paths {
        Paths {
            db: cfg.library_db_path(),
            media: cfg.media_dir(),
            upload_tmp: cfg.upload_tmp_dir(),
            inbox: cfg.inbox_dir(),
            backup: cfg.backup_dir(),
        }
    }
}

pub struct Library {
    pub(crate) conn: Connection,
    pub paths: Paths,
}

impl Library {
    /// Open (creating directories, the database and the schema as needed) and clean
    /// `upload-tmp/`: nothing in it can be a finished file (import renames out of it
    /// atomically), so whatever is there is the remains of an interrupted copy.
    pub fn open(paths: Paths) -> Result<Library> {
        crate::store::clean_upload_tmp(&paths.upload_tmp)?;
        Self::open_worker(paths)
    }

    /// Open without touching `upload-tmp/`: for import workers and the CLI, which may run
    /// beside a service whose copies are in flight.
    pub fn open_worker(paths: Paths) -> Result<Library> {
        for d in [&paths.media, &paths.upload_tmp, &paths.inbox, &paths.backup] {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(&paths.db)?;
        conn.busy_timeout(Duration::from_secs(30))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut lib = Library { conn, paths };
        lib.migrate()?;
        Ok(lib)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    fn schema_version(&self) -> Result<i64> {
        let has: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='meta'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if has.is_none() {
            return Ok(0);
        }
        let v: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    fn migrate(&mut self) -> Result<()> {
        let v = self.schema_version()?;
        if v > SCHEMA_VERSION {
            return Err(LibraryError::Invalid(format!(
                "library.db has schema version {v}; this build knows {SCHEMA_VERSION}"
            )));
        }
        if v < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','1'),('created_at',?1),('library_revision','0')",
                params![now_iso()],
            )?;
            seed(&tx)?;
            tx.commit()?;
        }
        if v < 2 {
            // Playlists as entry lists with control blocks (eighth round). A v1 library has no
            // playlist data worth keeping: nothing wrote to those tables before M1.5.
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V2)?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','2')
                 ON CONFLICT(key) DO UPDATE SET value = '2'",
                [],
            )?;
            tx.commit()?;
        }
        if v < 3 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V3)?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','3')
                 ON CONFLICT(key) DO UPDATE SET value = '3'",
                [],
            )?;
            tx.commit()?;
        }
        if v < 4 {
            // The quiet profile of every item already analysed, from its stored envelope (no decode). The
            // revision moves, so stations pick the new value up at their next sync.
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V4)?;
            type Row = (i64, i64, i64, i64, Vec<u8>);
            let rows: Vec<Row> = {
                let mut st = tx.prepare(
                    "SELECT e.item_id, e.cue_in, e.cue_out, i.file_ms, a.env_e50
                     FROM item_effective e JOIN analysis a ON a.item_id = e.item_id
                     JOIN items i ON i.id = e.item_id",
                )?;
                let r = st.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?;
                r.collect::<std::result::Result<_, _>>()?
            };
            for (id, c_in, c_out, file_ms, blob) in rows {
                let Ok(q) = analysis::env::unpack(&blob) else {
                    continue;
                };
                let p = analysis::quiet::QuietProfile::measure(
                    &analysis::env::dequantize(&q),
                    c_in,
                    c_out,
                    file_ms,
                );
                tx.execute(
                    "UPDATE item_effective SET quiet_profile = ?2, longest_quiet_ms = ?3,
                        longest_quiet_file_ms = ?4, revision = revision + 1 WHERE item_id = ?1",
                    params![
                        id,
                        p.to_json(),
                        p.longest_below(crate::items::QUIET_DISPLAY_DB, false),
                        p.longest_below(crate::items::QUIET_DISPLAY_DB, true)
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','4')
                 ON CONFLICT(key) DO UPDATE SET value = '4'",
                [],
            )?;
            Library::bump_revision(&tx)?;
            tx.commit()?;
        }
        if v < 5 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V5)?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','5')
                 ON CONFLICT(key) DO UPDATE SET value = '5'",
                [],
            )?;
            tx.commit()?;
        }
        if v < 6 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(SCHEMA_V6)?;
            tx.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version','6')
                 ON CONFLICT(key) DO UPDATE SET value = '6'",
                [],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Every write transaction bumps this (5.2, "Change notification").
    pub fn bump_revision(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key='library_revision'",
            [],
        )?;
        Ok(())
    }

    pub fn revision(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='library_revision'",
            [],
            |r| r.get(0),
        )?)
    }
}

fn seed(tx: &rusqlite::Transaction) -> Result<()> {
    for name in ["music", "liner", "ambient", "longform"] {
        let p = analysis::markers::ProfileParams::builtin(name).expect("builtin profile");
        tx.execute(
            "INSERT INTO segue_profiles(name, params) VALUES(?1, ?2)",
            params![name, p.to_json().to_string()],
        )?;
    }
    for (name, params_json, desc) in common::transition::BUILTIN_PRESETS {
        tx.execute(
            "INSERT INTO transition_presets(name, params, description, builtin) VALUES(?1,?2,?3,1)",
            params![name, params_json, desc],
        )?;
    }
    for (name, kind, profile, off, patch) in common::transition::BUILTIN_CATEGORIES {
        tx.execute(
            "INSERT INTO categories(name, kind, segue_profile, gain_offset_db, transition_patch) VALUES(?1,?2,?3,?4,?5)",
            params![name, kind, profile, off, patch],
        )?;
    }
    Ok(())
}
