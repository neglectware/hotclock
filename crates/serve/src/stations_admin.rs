//! Station management from the web UI (architecture 5.1, 10): create, rename and delete a station without a shell.
//!
//! This file holds the parts that need no HTTP: the name rules, the file a new station gets, port choice, the
//! undo list that makes every multi-step change roll back, and the file and database moves of a rename and a
//! delete. The routes are in [`crate::stations_http`].
//!
//! **Boot persistence without `systemctl enable`.** polkit cannot limit `enable`/`disable` to one unit (the action
//! `manage-unit-files` carries no unit name; checked on systemd 255), so the service user is not given them. A
//! station is started at boot by a systemd generator (`packaging/systemd/hotclock-stations-generator`) that adds
//! `hotclock-station@<name>` for every `/etc/hotclock/stations/<name>.toml`, and the unit template has
//! `ConditionPathExists=` on that file, so a station exists exactly when its file does. The page only needs
//! `start`, `stop` and `restart`, which polkit can limit to `hotclock-station@*.service`.

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The first port offered to a new station (office 8301, Eve 8302).
pub const FIRST_PORT: u16 = 8301;
/// Longest short name.
pub const MAX_NAME: usize = 32;
/// Names a station may not have: they are taken by a command, a directory or a page of the box.
pub const RESERVED: &[&str] = &[
    "test", "library", "display", "notify", "backup", "restore", "archive", "stations", "media",
    "inbox", "default", "hotclock",
];
/// The processing presets a new station may start with (the shipped ones).
pub const PROCESSING_PRESETS: &[&str] = &["bypass", "gentle", "radio"];

/// One station change at a time (the second caller is told, not queued).
pub static BUSY: Mutex<()> = Mutex::new(());

/// Why a short name is not allowed (`None` when it is). Does not look at the files.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("the short name cannot be empty".into());
    }
    if name.len() > MAX_NAME {
        return Some(format!("the short name is at most {MAX_NAME} characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Some("use only lowercase letters a to z, digits, `-` and `_`".into());
    }
    if name.starts_with(['-', '_']) {
        return Some("start with a letter or a digit".into());
    }
    if RESERVED.contains(&name) {
        return Some(format!("`{name}` is reserved; choose another"));
    }
    None
}

pub fn unit_of(name: &str) -> String {
    format!("hotclock-station@{name}.service")
}

/// What a new station is. Built from the request after validation.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: String,
    pub display_name: String,
    /// `null`, an ALSA name, or whatever the request chose.
    pub device: String,
    pub stream: bool,
    pub port: u16,
    pub default_playlist: Option<String>,
    pub emergency_playlist: Option<String>,
    pub transition_preset: String,
    pub processing_preset: String,
    pub loudness: bool,
    pub expect_listener: bool,
}

/// The text of a new station's file. Only the keys that differ from the defaults are written, so the file stays short and
/// the Settings page shows everything else as `default`.
pub fn render_toml(s: &Spec, today: &str) -> Result<String, String> {
    use toml_edit::{value, DocumentMut};
    let mut d = DocumentMut::new();
    d.decor_mut().set_prefix(format!(
        "# {} ({}). Created by the Stations page on {today}; the Settings page edits every key.\n",
        s.display_name.replace('\n', " "),
        s.name
    ));
    d["name"] = value(s.name.as_str());
    if s.display_name != s.name {
        d["display_name"] = value(s.display_name.as_str());
    }
    d["listen"] = value(format!("0.0.0.0:{}", s.port));
    d["device"] = value(s.device.as_str());
    if let Some(p) = &s.default_playlist {
        d["default_playlist"] = value(p.as_str());
    }
    if let Some(p) = &s.emergency_playlist {
        d["emergency_playlist"] = value(p.as_str());
    }
    d["transition_preset"] = value(s.transition_preset.as_str());
    d["processing_preset"] = value(s.processing_preset.as_str());
    if s.stream {
        let mut t = toml_edit::Table::new();
        t["enabled"] = value(true);
        t["mount"] = value("program");
        if !s.expect_listener {
            t["expect_listener"] = value(false);
        }
        d["stream"] = toml_edit::Item::Table(t);
    }
    if s.loudness {
        let mut t = toml_edit::Table::new();
        t["enabled"] = value(true);
        d["loudness"] = toml_edit::Item::Table(t);
    }
    Ok(d.to_string())
}

/// Ports taken by the station files in `dir` (`listen = host:port`), whatever they are named.
pub fn ports_in_use(dir: &Path) -> Vec<(String, u16)> {
    let mut v = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let Some(n) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".toml"))
        else {
            continue;
        };
        if let Ok(c) = common::config::StationConfig::load(&p) {
            if let Some(port) = c.listen.rsplit(':').next().and_then(|x| x.parse().ok()) {
                v.push((n.to_string(), port));
            }
        }
    }
    v
}

/// Whether something already listens on (or holds) `port` on this box.
pub fn port_busy(port: u16) -> bool {
    std::net::TcpListener::bind(("0.0.0.0", port)).is_err()
}

/// The first port from [`FIRST_PORT`] that no station file names, that is not the library's and that nothing holds.
pub fn next_port(dir: &Path, library_port: Option<u16>) -> u16 {
    let used: Vec<u16> = ports_in_use(dir).into_iter().map(|x| x.1).collect();
    (FIRST_PORT..FIRST_PORT + 500)
        .find(|p| !used.contains(p) && Some(*p) != library_port && !port_busy(*p))
        .unwrap_or(FIRST_PORT)
}

// ---- the undo list -------------------------------------------------------------------------------------------

type UndoFn = Box<dyn FnOnce() -> Result<(), String> + Send>;

/// Steps that can be taken back, run in reverse order when a later step fails.
#[derive(Default)]
pub struct Undo {
    steps: Vec<(String, UndoFn)>,
}

impl Undo {
    pub fn push(
        &mut self,
        label: impl Into<String>,
        f: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) {
        self.steps.push((label.into(), Box::new(f)));
    }
    /// Everything done is now final.
    pub fn commit(&mut self) {
        self.steps.clear();
    }
    /// Take every step back, newest first. Returns what could not be taken back (empty when all went well) and
    /// the labels of the steps undone.
    pub fn rollback(&mut self) -> (Vec<String>, Vec<String>) {
        let (mut failed, mut done) = (vec![], vec![]);
        while let Some((label, f)) = self.steps.pop() {
            match f() {
                Ok(()) => done.push(label),
                Err(e) => failed.push(format!("{label}: {e}")),
            }
        }
        (failed, done)
    }
}

// ---- files -----------------------------------------------------------------------------------------------------

/// Rename, or copy and remove when the two paths are on different file systems (`/etc` and `/var/lib`).
pub fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {
            // EXDEV
            if from.is_dir() {
                copy_dir(from, to)?;
                std::fs::remove_dir_all(from)
                    .map_err(|e| format!("cannot remove {}: {e}", from.display()))
            } else {
                std::fs::copy(from, to)
                    .map_err(|e| format!("cannot copy {}: {e}", from.display()))?;
                std::fs::remove_file(from)
                    .map_err(|e| format!("cannot remove {}: {e}", from.display()))
            }
        }
        Err(e) => Err(format!(
            "cannot move {} to {}: {e}",
            from.display(),
            to.display()
        )),
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for e in std::fs::read_dir(from)
        .map_err(|e| e.to_string())?
        .flatten()
    {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("cannot copy {}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// Snapshot files of a station in the backup directory: `station-<name>-YYYYMMDD.db` (and `.db.bad`).
pub fn snapshot_files(backup: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("station-{name}-");
    let mut v: Vec<PathBuf> = std::fs::read_dir(backup)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let f = e.file_name().into_string().ok()?;
            let rest = f.strip_prefix(&prefix)?;
            (rest.len() >= 11
                && rest.as_bytes()[..8].iter().all(u8::is_ascii_digit)
                && rest[8..].starts_with(".db"))
            .then(|| e.path())
        })
        .collect();
    v.sort();
    v
}

/// Move a station's snapshots from `from` to `to` names. Returns the list of `(old path, new path)` moved so a
/// caller can put them back.
pub fn rename_snapshots(
    backup: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut done: Vec<(PathBuf, PathBuf)> = vec![];
    for f in snapshot_files(backup, from) {
        let fname = f
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let new =
            backup.join(fname.replacen(&format!("station-{from}-"), &format!("station-{to}-"), 1));
        if let Err(e) = std::fs::rename(&f, &new) {
            for (o, n) in done.iter().rev() {
                let _ = std::fs::rename(n, o);
            }
            return Err(format!("cannot rename {}: {e}", f.display()));
        }
        done.push((f, new));
    }
    Ok(done)
}

// ---- the library's references to a station ---------------------------------------------------------------------

/// Every `(table, column)` of the library that holds a station name (`station_id` or `station`).
pub fn station_columns(c: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut out = vec![];
    let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let tables: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for t in tables {
        let mut ti = c.prepare(&format!("PRAGMA table_info(\"{t}\")"))?;
        let cols: Vec<String> = ti
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        for col in cols {
            if col == "station_id" || col == "station" {
                out.push((t.clone(), col));
            }
        }
    }
    Ok(out)
}

/// How many rows of the library mention `name`, per table (the `stations` row itself is not counted).
pub fn library_refs(c: &Connection, name: &str) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut out = vec![];
    for (t, col) in station_columns(c)? {
        let n: i64 = c.query_row(
            &format!("SELECT COUNT(*) FROM \"{t}\" WHERE \"{col}\" = ?1"),
            [name],
            |r| r.get(0),
        )?;
        if n > 0 {
            out.push((t, n));
        }
    }
    Ok(out)
}

/// Point every reference at `to` in one transaction (foreign keys are checked at the commit).
pub fn library_rename(c: &Connection, from: &str, to: &str) -> Result<(), String> {
    let cols = station_columns(c).map_err(|e| e.to_string())?;
    c.execute_batch("BEGIN IMMEDIATE; PRAGMA defer_foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    let r = (|| -> rusqlite::Result<()> {
        // A bare `stations` row with nothing behind it (made by a block that was since removed) is no obstacle.
        c.execute("DELETE FROM stations WHERE id = ?1", params![to])?;
        c.execute(
            "UPDATE stations SET id = ?2 WHERE id = ?1",
            params![from, to],
        )?;
        for (t, col) in &cols {
            c.execute(
                &format!("UPDATE \"{t}\" SET \"{col}\" = ?2 WHERE \"{col}\" = ?1"),
                params![from, to],
            )?;
        }
        Ok(())
    })();
    match r.and_then(|()| c.execute_batch("COMMIT")) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(format!("library database: {e}"))
        }
    }
}

/// Remove a station's records from the library: its station blocks, rules and notification history go; playlists that
/// were limited to it become usable by any station.
pub fn library_forget(c: &Connection, name: &str) -> Result<(), String> {
    let cols = station_columns(c).map_err(|e| e.to_string())?;
    c.execute_batch("BEGIN IMMEDIATE; PRAGMA defer_foreign_keys = ON;")
        .map_err(|e| e.to_string())?;
    let r = (|| -> rusqlite::Result<()> {
        for (t, col) in &cols {
            if t == "playlists" || t == "display_view" {
                c.execute(
                    &format!("UPDATE \"{t}\" SET \"{col}\" = NULL WHERE \"{col}\" = ?1"),
                    params![name],
                )?;
            } else {
                c.execute(
                    &format!("DELETE FROM \"{t}\" WHERE \"{col}\" = ?1"),
                    params![name],
                )?;
            }
        }
        c.execute("DELETE FROM stations WHERE id = ?1", params![name])?;
        Ok(())
    })();
    match r.and_then(|()| c.execute_batch("COMMIT")) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(format!("library database: {e}"))
        }
    }
}

/// Only the display's saved view mentions a kept station: point it away (used by a delete that keeps the records).
pub fn display_view_clear(c: &Connection, name: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE display_view SET station = NULL WHERE station = ?1",
        params![name],
    )?;
    Ok(())
}

/// Compact UTC stamp `YYYYMMDD-HHMMSS` from an ISO time text.
pub fn stamp(iso: &str) -> String {
    let d: String = iso.chars().filter(char::is_ascii_digit).take(14).collect();
    if d.len() == 14 {
        format!("{}-{}", &d[..8], &d[8..])
    } else {
        "unknown".into()
    }
}

pub fn plan_error(code: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_parser_and_the_reserved_list() {
        for ok in ["office", "eve", "a", "x9", "late-night", "m_2"] {
            assert_eq!(name_problem(ok), None, "{ok}");
        }
        for bad in [
            "",
            "Office",
            "has space",
            "-x",
            "_x",
            "test",
            "library",
            "ü",
            &"a".repeat(33),
        ] {
            assert!(name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn the_rendered_file_passes_the_station_parser_for_each_output() {
        for (device, stream, loud) in [
            ("null", true, true),
            ("hw:CARD=PCH,DEV=0", false, false),
            ("hw:CARD=PCH,DEV=0", true, false),
        ] {
            let s = Spec {
                name: "late".into(),
                display_name: "Late Night".into(),
                device: device.into(),
                stream,
                port: 8305,
                default_playlist: Some("Rot".into()),
                emergency_playlist: None,
                transition_preset: "plexamp".into(),
                processing_preset: "gentle".into(),
                loudness: loud,
                expect_listener: false,
            };
            let t = render_toml(&s, "2026-10-07").unwrap();
            let c = common::config::StationConfig::parse(&t, Path::new("/x/late.toml"))
                .unwrap_or_else(|e| panic!("{e:?}\n{t}"));
            assert_eq!(
                (c.stream.enabled, c.loudness.enabled, c.listen.as_str()),
                (stream, loud, "0.0.0.0:8305")
            );
            assert_eq!(c.display_name.as_deref(), Some("Late Night"));
        }
    }

    #[test]
    fn undo_runs_newest_first_and_reports_what_failed() {
        let log = std::sync::Arc::new(Mutex::new(vec![]));
        let mut u = Undo::default();
        for (i, fail) in [(1, false), (2, true), (3, false)] {
            let l = log.clone();
            u.push(format!("step{i}"), move || {
                l.lock().unwrap().push(i);
                if fail {
                    Err("boom".into())
                } else {
                    Ok(())
                }
            });
        }
        let (failed, done) = u.rollback();
        assert_eq!(*log.lock().unwrap(), vec![3, 2, 1]);
        assert_eq!(failed, vec!["step2: boom"]);
        assert_eq!(done, vec!["step3", "step1"]);
    }

    #[test]
    fn snapshots_match_only_this_station() {
        let d = std::env::temp_dir().join(format!("hc-snap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for f in [
            "station-a-20261001.db",
            "station-a-b-20261001.db",
            "station-a-20261002.db.bad",
            "station-a-notes.txt",
            "library-20261001.db",
        ] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let names: Vec<String> = snapshot_files(&d, "a")
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into())
            .collect();
        assert_eq!(
            names,
            vec!["station-a-20261001.db", "station-a-20261002.db.bad"]
        );
        let moved = rename_snapshots(&d, "a", "z").unwrap();
        assert_eq!(moved.len(), 2);
        assert!(
            d.join("station-z-20261001.db").exists() && d.join("station-a-b-20261001.db").exists()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ports_skip_the_used_ones() {
        let d = std::env::temp_dir().join(format!("hc-ports-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let free = next_port(&d, None);
        std::fs::write(
            d.join("a.toml"),
            format!("name = \"a\"\nlisten = \"0.0.0.0:{free}\"\ndevice = \"null\"\n"),
        )
        .unwrap();
        assert!(next_port(&d, None) > free);
        assert_eq!(
            next_port(&d, None),
            (free + 1..).find(|p| !port_busy(*p)).unwrap()
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
