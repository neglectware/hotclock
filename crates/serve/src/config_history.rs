//! The history of a station's configuration file (`/etc/hotclock/stations/<name>.toml`).
//!
//! Every save of that file, from whatever route made it, is stored as a version: the time, the full text, a
//! short source ("processing: eq", "loudness", "station settings", "restore") and a list of what changed, worked
//! out from the parsed files (`chain.eq.b2_db: -6.0 → -8.5`). Changes from the same source within ten seconds are
//! one version (a knob drag is one entry), and the newest 200 versions are kept.
//!
//! The versions live in `config-history.db`, a small SQLite file in the station's own data directory
//! (`/var/lib/hotclock/stations/<name>/`). Two processes write the configuration file (the station, for the
//! processing chain and the loudness controller, and the library service, for the Settings page); both run as the
//! same user and both can write that directory, so the history is one file with a busy timeout, not a
//! per-process list. It is a file of its own, not a table in `state.db`, because the station moves a damaged
//! `state.db` aside at start and the history has to survive that.
//!
//! The history never stands in the way of a save: when it cannot be written, the save has already happened and
//! the error goes to the log.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// How many versions are kept per station.
pub const KEEP: usize = 200;
/// Changes from the same source within this long of the last one are one version.
pub const COALESCE_MS: i64 = 10_000;
/// The most changes stored with one version.
const MAX_CHANGES: usize = 200;

/// Sources that are never merged with the next change.
const SEALED: [&str; 3] = ["restore", "loaded at start", "edited outside the page"];

pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---- the difference between two files ------------------------------------------------------------------------

/// One difference: a key (dotted, `[section]` for a whole table that came or went) and its two values as text.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The two values as parsed, for naming and formatting (`config_labels`).
    pub bv: Option<toml::Value>,
    pub av: Option<toml::Value>,
}

impl Change {
    /// `key: old → new`.
    pub fn text(&self) -> String {
        if self.key.starts_with('[') {
            return format!(
                "{}: {}",
                self.key,
                self.after
                    .as_deref()
                    .or(self.before.as_deref())
                    .unwrap_or("")
            );
        }
        format!(
            "{}: {} → {}",
            self.key,
            self.before.as_deref().unwrap_or("(not set)"),
            self.after.as_deref().unwrap_or("(not set)")
        )
    }

    pub fn json(&self) -> Value {
        json!({"key": self.key, "before": self.before, "after": self.after})
    }
}

fn show(v: &toml::Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 80 {
        let cut: String = s.chars().take(77).collect();
        format!("{cut}…")
    } else {
        s
    }
}

fn walk(prefix: &str, a: Option<&toml::Value>, b: Option<&toml::Value>, out: &mut Vec<Change>) {
    use toml::Value::Table;
    match (a, b) {
        (Some(Table(ta)), Some(Table(tb))) => {
            let mut keys: Vec<&String> = ta.keys().chain(tb.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                walk(&p, ta.get(k), tb.get(k), out);
            }
        }
        (None, Some(Table(tb))) => {
            if !prefix.is_empty() {
                out.push(Change {
                    key: format!("[{prefix}]"),
                    before: None,
                    after: Some("section added".into()),
                    bv: None,
                    av: None,
                });
            }
            walk_one_side(prefix, tb, false, out);
        }
        (Some(Table(ta)), None) => {
            if !prefix.is_empty() {
                out.push(Change {
                    key: format!("[{prefix}]"),
                    before: Some("section removed".into()),
                    after: None,
                    bv: None,
                    av: None,
                });
            }
            walk_one_side(prefix, ta, true, out);
        }
        (x, y) => {
            if x != y {
                out.push(Change {
                    key: prefix.to_string(),
                    before: x.map(show),
                    after: y.map(show),
                    bv: x.cloned(),
                    av: y.cloned(),
                });
            }
        }
    }
}

/// The keys of a table that is wholly new (or wholly gone).
fn walk_one_side(prefix: &str, t: &toml::Table, gone: bool, out: &mut Vec<Change>) {
    for (k, v) in t {
        let p = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(sub) => {
                out.push(Change {
                    key: format!("[{p}]"),
                    before: gone.then(|| "section removed".into()),
                    after: (!gone).then(|| "section added".into()),
                    bv: None,
                    av: None,
                });
                walk_one_side(&p, sub, gone, out);
            }
            _ => out.push(Change {
                key: p,
                before: gone.then(|| show(v)),
                after: (!gone).then(|| show(v)),
                bv: gone.then(|| v.clone()),
                av: (!gone).then(|| v.clone()),
            }),
        }
    }
}

/// What differs between two texts of a configuration file, by parsed key. A text that does not parse counts as
/// one change of the whole file.
pub fn diff_text(before: &str, after: &str) -> Vec<Change> {
    let pa = toml::from_str::<toml::Value>(before);
    let pb = toml::from_str::<toml::Value>(after);
    match (pa, pb) {
        (Ok(a), Ok(b)) => {
            let mut out = vec![];
            walk("", Some(&a), Some(&b), &mut out);
            // Whole sections that came or went go after the keys, so a short list leads with the values.
            out.sort_by_key(|c| c.key.starts_with('['));
            out
        }
        _ => {
            if before == after {
                vec![]
            } else {
                vec![Change {
                    key: "(file)".into(),
                    before: Some("a file that does not parse".into()),
                    after: Some("a file that does not parse".into()),
                    bv: None,
                    av: None,
                }]
            }
        }
    }
}

fn texts(changes: &[Change]) -> Vec<String> {
    changes.iter().take(MAX_CHANGES).map(Change::text).collect()
}

// ---- the store -----------------------------------------------------------------------------------------------

/// A version without its text, for the list.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub ts_ms: i64,
    pub source: String,
    /// For a restore: the time of the version that was restored.
    pub of_ts_ms: Option<i64>,
    pub changes: Vec<Change>,
    pub size: i64,
}

impl Entry {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "ts_ms": self.ts_ms,
            "time": station::state::iso_ms(self.ts_ms),
            "source": self.source,
            "of_ts_ms": self.of_ts_ms,
            "of_time": self.of_ts_ms.map(station::state::iso_ms),
            "changes": self.changes.iter().take(MAX_CHANGES).map(|c| crate::config_labels::describe(c).text()).collect::<Vec<_>>(),
            "change_count": self.changes.len(),
            "size": self.size,
        })
    }
}

/// A version with its text.
#[derive(Debug, Clone)]
pub struct Version {
    pub entry: Entry,
    pub toml: String,
}

/// The history of one station's configuration file.
#[derive(Debug, Clone)]
pub struct ConfigHistory {
    db: PathBuf,
    toml: PathBuf,
}

impl ConfigHistory {
    pub fn new(db: PathBuf, toml: PathBuf) -> ConfigHistory {
        ConfigHistory { db, toml }
    }

    /// The history file that sits beside a station's `state.db`.
    pub fn beside(state_db: &Path, toml: &Path) -> ConfigHistory {
        ConfigHistory::new(
            state_db.with_file_name("config-history.db"),
            toml.to_path_buf(),
        )
    }

    pub fn toml_path(&self) -> &Path {
        &self.toml
    }

    fn open(&self) -> Result<Connection, String> {
        if let Some(d) = self.db.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let c = Connection::open(&self.db).map_err(|e| format!("{}: {e}", self.db.display()))?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        let _ = c.pragma_update(None, "journal_mode", "WAL");
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS config_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_ms INTEGER NOT NULL,
                source TEXT NOT NULL,
                of_ts_ms INTEGER,
                toml TEXT NOT NULL,
                changes TEXT NOT NULL
            );",
        )
        .map_err(|e| e.to_string())?;
        Ok(c)
    }

    /// Run `f` (which writes the configuration file) and store what it changed. `f`'s own result passes through;
    /// a history that cannot be written is logged and changes nothing for the caller.
    pub fn track<T>(&self, source: &str, f: impl FnOnce() -> T) -> T {
        self.track_at(source, None, f)
    }

    pub fn track_at<T>(&self, source: &str, of_ts_ms: Option<i64>, f: impl FnOnce() -> T) -> T {
        let before = std::fs::read_to_string(&self.toml).unwrap_or_default();
        let r = f();
        let after = std::fs::read_to_string(&self.toml).unwrap_or_default();
        if before != after {
            if let Err(e) = self.record(wall_ms(), source, of_ts_ms, &before, &after) {
                eprintln!("hotclock: configuration history: {e}");
            }
        }
        r
    }

    /// Store one change of the file from `before` to `after`.
    pub fn record(
        &self,
        now_ms: i64,
        source: &str,
        of_ts_ms: Option<i64>,
        before: &str,
        after: &str,
    ) -> Result<Option<i64>, String> {
        let mut c = self.open()?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let mut newest = newest_of(&tx)?;
        // The first change ever: what the file was before it is the starting point.
        // A change the history did not see (a hand edit): the file as it was is a version of its own.
        let unseen = match &newest {
            None => Some(("earlier state", before.to_string())),
            Some((_, _, _, t)) if t != before => {
                Some(("edited outside the page", before.to_string()))
            }
            _ => None,
        };
        if let Some((src, text)) = unseen {
            if !(newest.is_none() && text.is_empty()) {
                let changes = match &newest {
                    Some((_, _, _, t)) => diff_text(t, &text),
                    None => vec![],
                };
                insert(&tx, now_ms - 1, src, None, &text, &texts(&changes))?;
                newest = newest_of(&tx)?;
            }
        }
        let mut out_id = None;
        let merge = newest.as_ref().filter(|(_, ts, src, _)| {
            src == source && !SEALED.contains(&source) && now_ms - ts <= COALESCE_MS
        });
        if let Some((id, _, _, _)) = merge {
            let prev: Option<String> = tx
                .query_row(
                    "SELECT toml FROM config_history WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let base = prev.unwrap_or_default();
            let changes = diff_text(&base, after);
            if after == base {
                // The drag came back to where it started: nothing is left to undo.
                tx.execute("DELETE FROM config_history WHERE id = ?1", [id])
                    .map_err(|e| e.to_string())?;
            } else {
                tx.execute(
                    "UPDATE config_history SET ts_ms = ?1, toml = ?2, changes = ?3 WHERE id = ?4",
                    params![now_ms, after, json!(texts(&changes)).to_string(), id],
                )
                .map_err(|e| e.to_string())?;
                out_id = Some(*id);
            }
        } else {
            let changes = diff_text(before, after);
            out_id = Some(insert(
                &tx,
                now_ms,
                source,
                of_ts_ms,
                after,
                &texts(&changes),
            )?);
        }
        tx.execute(
            "DELETE FROM config_history WHERE id NOT IN (SELECT id FROM config_history ORDER BY id DESC LIMIT ?1)",
            [KEEP as i64],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out_id)
    }

    /// At the start of the station: the file as it was loaded is a version if the newest one differs from it, so
    /// a hand edit or a restore outside the page is kept too.
    pub fn capture_start(&self, now_ms: i64) -> Result<(), String> {
        let text = std::fs::read_to_string(&self.toml).map_err(|e| e.to_string())?;
        let mut c = self.open()?;
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let changes = match newest_of(&tx)? {
            Some((_, _, _, t)) if t == text => return Ok(()),
            Some((_, _, _, t)) => texts(&diff_text(&t, &text)),
            None => vec![],
        };
        insert(&tx, now_ms, "loaded at start", None, &text, &changes)?;
        tx.execute(
            "DELETE FROM config_history WHERE id NOT IN (SELECT id FROM config_history ORDER BY id DESC LIMIT ?1)",
            [KEEP as i64],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// The versions, newest first.
    pub fn list(&self) -> Result<Vec<Entry>, String> {
        let c = self.open()?;
        let mut st = c
            .prepare(
                "SELECT id, ts_ms, source, of_ts_ms, toml FROM config_history ORDER BY id DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, i64, String, Option<i64>, String)> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        // What a version changed is its text against the text of the one under it; the oldest has nothing under it.
        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, (id, ts_ms, source, of_ts_ms, toml))| Entry {
                id: *id,
                ts_ms: *ts_ms,
                source: source.clone(),
                of_ts_ms: *of_ts_ms,
                changes: rows
                    .get(i + 1)
                    .map(|p| diff_text(&p.4, toml))
                    .unwrap_or_default(),
                size: toml.len() as i64,
            })
            .collect())
    }

    /// One version with its text, and the text of the version before it.
    pub fn get(&self, id: i64) -> Result<Option<(Version, Option<String>)>, String> {
        let c = self.open()?;
        let v = c
            .query_row(
                "SELECT id, ts_ms, source, of_ts_ms, toml FROM config_history WHERE id = ?1",
                [id],
                |r| {
                    let toml: String = r.get(4)?;
                    Ok(Version {
                        entry: Entry {
                            id: r.get(0)?,
                            ts_ms: r.get(1)?,
                            source: r.get(2)?,
                            of_ts_ms: r.get(3)?,
                            changes: vec![],
                            size: toml.len() as i64,
                        },
                        toml,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(mut v) = v else { return Ok(None) };
        let prev: Option<String> = c
            .query_row(
                "SELECT toml FROM config_history WHERE id < ?1 ORDER BY id DESC LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(p) = &prev {
            v.entry.changes = diff_text(p, &v.toml);
        }
        Ok(Some((v, prev)))
    }

    pub fn newest_id(&self) -> Option<i64> {
        self.open().ok().and_then(|c| {
            c.query_row("SELECT MAX(id) FROM config_history", [], |r| r.get(0))
                .ok()
                .flatten()
        })
    }
}

type Row = (i64, i64, String, String);

fn newest_of(c: &Connection) -> Result<Option<Row>, String> {
    c.query_row(
        "SELECT id, ts_ms, source, toml FROM config_history ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn insert(
    c: &Connection,
    ts_ms: i64,
    source: &str,
    of_ts_ms: Option<i64>,
    toml: &str,
    changes: &[String],
) -> Result<i64, String> {
    c.execute(
        "INSERT INTO config_history (ts_ms, source, of_ts_ms, toml, changes) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts_ms, source, of_ts_ms, toml, json!(changes).to_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(c.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(name: &str, text: &str) -> (ConfigHistory, PathBuf) {
        let d = std::env::temp_dir().join(format!("hc-hist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let toml = d.join("office.toml");
        std::fs::write(&toml, text).unwrap();
        (ConfigHistory::new(d.join("config-history.db"), toml), d)
    }

    fn raw(e: &Entry) -> Vec<String> {
        e.changes.iter().map(Change::text).collect()
    }

    fn eq(db: f64) -> String {
        format!("name = \"office\"\n\n[chain.eq]\nenabled = true\nb2_db = {db:?}\n")
    }

    #[test]
    fn the_summary_names_each_key_with_old_and_new() {
        let c = diff_text(&eq(-6.0), &eq(-8.5));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text(), "chain.eq.b2_db: -6.0 → -8.5");
        let c = diff_text("name = \"a\"\n", &eq(-6.0));
        let t: Vec<String> = c.iter().map(Change::text).collect();
        assert!(t.contains(&"[chain]: section added".to_string()), "{t:?}");
        assert!(t.contains(&"[chain.eq]: section added".to_string()));
        assert!(t.contains(&"chain.eq.b2_db: (not set) → -6.0".to_string()));
        assert!(t.contains(&"name: \"a\" → \"office\"".to_string()));
        let t: Vec<String> = diff_text(&eq(-6.0), "name = \"office\"\n")
            .iter()
            .map(Change::text)
            .collect();
        assert!(
            t.contains(&"[chain.eq]: section removed".to_string()),
            "{t:?}"
        );
        assert!(t.contains(&"chain.eq.enabled: true → (not set)".to_string()));
        assert!(diff_text("a = 1 # x\n", "a = 1\n").is_empty());
    }

    #[test]
    fn changes_from_one_source_within_ten_seconds_are_one_version() {
        let (h, d) = setup("coalesce", &eq(0.0));
        let t0 = 1_000_000;
        let mut prev = eq(0.0);
        for (i, v) in [-1.0, -2.0, -3.0].iter().enumerate() {
            let next = eq(*v);
            h.record(t0 + i as i64 * 4_000, "processing: eq", None, &prev, &next)
                .unwrap();
            prev = next;
        }
        let l = h.list().unwrap();
        // The starting point and one version for the drag, which keeps the oldest "before" and the newest "after".
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!(raw(&l[0]), vec!["chain.eq.b2_db: 0.0 → -3.0"]);
        assert_eq!(l[0].ts_ms, t0 + 8_000);
        // 11 s after the last one: a new version. A different source: a new version.
        let n = eq(-4.0);
        h.record(t0 + 8_000 + 11_000, "processing: eq", None, &prev, &n)
            .unwrap();
        let n2 = eq(-5.0);
        h.record(t0 + 8_000 + 12_000, "loudness", None, &n, &n2)
            .unwrap();
        assert_eq!(h.list().unwrap().len(), 4);
        // A second loudness change inside the window merges with the first.
        let n3 = eq(-6.0);
        h.record(t0 + 8_000 + 13_000, "loudness", None, &n2, &n3)
            .unwrap();
        let l = h.list().unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(raw(&l[0]), vec!["chain.eq.b2_db: -4.0 → -6.0"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_drag_that_comes_back_leaves_no_version() {
        let (h, d) = setup("back", &eq(0.0));
        h.record(5_000, "processing: eq", None, &eq(0.0), &eq(-3.0))
            .unwrap();
        h.record(6_000, "processing: eq", None, &eq(-3.0), &eq(0.0))
            .unwrap();
        let l = h.list().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].source, "earlier state");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn restores_are_never_merged() {
        let (h, d) = setup("restore", &eq(0.0));
        h.record(1_000, "processing: eq", None, &eq(0.0), &eq(-1.0))
            .unwrap();
        h.record(2_000, "restore", Some(500), &eq(-1.0), &eq(0.0))
            .unwrap();
        h.record(3_000, "restore", Some(500), &eq(0.0), &eq(-1.0))
            .unwrap();
        let l = h.list().unwrap();
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].of_ts_ms, Some(500));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn only_the_newest_two_hundred_are_kept() {
        let (h, d) = setup("prune", &eq(0.0));
        let mut prev = eq(0.0);
        for i in 1..=230 {
            let next = eq(f64::from(i));
            // 20 s apart, so none merge.
            h.record(i64::from(i) * 20_000, "processing: eq", None, &prev, &next)
                .unwrap();
            prev = next;
        }
        let l = h.list().unwrap();
        assert_eq!(l.len(), KEEP);
        assert_eq!(raw(&l[0]), vec!["chain.eq.b2_db: 229.0 → 230.0"]);
        // The oldest kept has nothing under it to compare with; the one above it shows its change.
        assert!(l[KEEP - 1].changes.is_empty());
        assert_eq!(raw(&l[KEEP - 2]), vec!["chain.eq.b2_db: 31.0 → 32.0"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_hand_edit_is_captured_at_start_and_when_seen_later() {
        let (h, d) = setup("hand", &eq(0.0));
        h.capture_start(1_000).unwrap();
        let l = h.list().unwrap();
        assert_eq!((l.len(), l[0].source.as_str()), (1, "loaded at start"));
        // Nothing changed: no second version.
        h.capture_start(2_000).unwrap();
        assert_eq!(h.list().unwrap().len(), 1);
        // A hand edit while stopped.
        std::fs::write(h.toml_path(), eq(-7.0)).unwrap();
        h.capture_start(3_000).unwrap();
        let l = h.list().unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].source, "loaded at start");
        assert_eq!(raw(&l[0]), vec!["chain.eq.b2_db: 0.0 → -7.0"]);
        // A hand edit while running, then a page change: the page change finds the file different from the
        // newest version and keeps the edit as a version too.
        std::fs::write(h.toml_path(), eq(-8.0)).unwrap();
        h.track("processing: eq", || {
            std::fs::write(h.toml_path(), eq(-9.0)).unwrap()
        });
        let l = h.list().unwrap();
        assert_eq!(l.len(), 4, "{l:?}");
        assert_eq!(l[1].source, "edited outside the page");
        assert_eq!(raw(&l[0]), vec!["chain.eq.b2_db: -8.0 → -9.0"]);
        let _ = std::fs::remove_dir_all(d);
    }
}
