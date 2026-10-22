//! The `inbox/` drop folder (architecture 5.4): inotify plus a 60 s rescan.
//!
//! Layout: files in the bare `inbox/` go to `music`; a first-level subfolder whose name matches a
//! category (case-insensitively) selects it (`inbox/liner/`, `inbox/id/`, `inbox/promo/`,
//! `inbox/ad/`, `inbox/music/`). After a successful import (or a duplicate) the source file is
//! removed from the inbox; a failed file moves to `inbox/.failed/` next to a `.error.txt` with the
//! reason, so it is neither retried forever nor lost. Files modified in the last 2 s are left
//! alone (still being copied).

use crate::db::{Library, Paths};
use crate::import::{import_many, ImportOptions, ImportOutcome};
use crate::Result;
use analysis::census::AUDIO_EXTENSIONS;
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How long a file must be unchanged before it is picked up.
pub const SETTLE: Duration = Duration::from_secs(2);
pub const RESCAN: Duration = Duration::from_secs(60);

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// Candidate files under the inbox with the category their folder selects.
pub fn candidates(lib: &Library, settle: Duration) -> Result<Vec<(PathBuf, Option<String>)>> {
    let mut cats: Vec<String> = vec![];
    {
        let mut st = lib.conn.prepare("SELECT name FROM categories")?;
        for r in st.query_map([], |r| r.get::<_, String>(0))? {
            cats.push(r?);
        }
    }
    let mut out = vec![];
    let now = SystemTime::now();
    let mut stack = vec![lib.paths.inbox.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if is_hidden(&p) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let ok_ext = p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| AUDIO_EXTENSIONS.contains(&x.to_ascii_lowercase().as_str()));
            if !ok_ext {
                continue;
            }
            let settled = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|age| age >= settle);
            if !settled {
                continue;
            }
            let rel = p.strip_prefix(&lib.paths.inbox).unwrap_or(&p);
            let first = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .filter(|_| rel.components().count() > 1);
            let cat = first.and_then(|f| cats.iter().find(|c| c.eq_ignore_ascii_case(f)).cloned());
            out.push((p, cat));
        }
    }
    out.sort();
    Ok(out)
}

/// One pass over the inbox. Returns what happened to each file.
pub fn scan_once(
    paths: &Paths,
    workers: usize,
    settle: Duration,
) -> Result<Vec<(PathBuf, ImportOutcome)>> {
    let lib = Library::open_worker(paths.clone())?;
    let found = candidates(&lib, settle)?;
    if found.is_empty() {
        return Ok(vec![]);
    }
    let jobs: Vec<(PathBuf, ImportOptions)> = found
        .into_iter()
        .map(|(p, c)| {
            (
                p,
                ImportOptions {
                    category: c,
                    source: "inbox".into(),
                },
            )
        })
        .collect();
    let results = import_many(paths, jobs, workers);
    let failed_dir = paths.inbox.join(".failed");
    for (p, outcome) in &results {
        match outcome {
            ImportOutcome::Done { .. } | ImportOutcome::Duplicate { .. } => {
                let _ = std::fs::remove_file(p);
            }
            ImportOutcome::Failed { error } => {
                let _ = std::fs::create_dir_all(&failed_dir);
                if let Some(name) = p.file_name() {
                    let dest = failed_dir.join(name);
                    if std::fs::rename(p, &dest).is_ok() {
                        let mut note = dest.clone().into_os_string();
                        note.push(".error.txt");
                        let _ = std::fs::write(PathBuf::from(note), format!("{error}\n"));
                    }
                }
            }
        }
    }
    let _ = lib.conn.execute("SELECT 1", params![]);
    Ok(results)
}

/// The inbox service loop: a filesystem watcher wakes the scan, and a rescan every 60 s catches
/// anything the watcher missed (network shares, a full event queue). Returns when `stop` is set.
pub fn run(paths: &Paths, workers: usize, stop: Arc<AtomicBool>) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
        if ev.is_ok() {
            let _ = tx.send(());
        }
    })
    .map_err(|e| crate::LibraryError::Invalid(format!("inbox watcher: {e}")))?;
    std::fs::create_dir_all(&paths.inbox)?;
    watcher
        .watch(&paths.inbox, RecursiveMode::Recursive)
        .map_err(|e| crate::LibraryError::Invalid(format!("inbox watcher: {e}")))?;
    // The library service also writes what the stations decided about items (quarantines, releases,
    // playback errors: 1.5), which they queue in their own state.db; every second or so.
    let lib = Library::open_worker(paths.clone())?;
    let outbox = || {
        let n = crate::health::apply_station_outboxes(&lib);
        if n > 0 {
            tracing::info!("applied {n} station write(s) to library.db");
        }
    };
    outbox();
    while !stop.load(Ordering::Relaxed) {
        // Wait for an event or the rescan interval; poll the stop flag every second.
        let mut waited = Duration::ZERO;
        let mut woke = false;
        while waited < RESCAN && !stop.load(Ordering::Relaxed) {
            if rx.recv_timeout(Duration::from_secs(1)).is_ok() {
                woke = true;
                break;
            }
            waited += Duration::from_secs(1);
            outbox();
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if woke {
            // Let a copy finish, and swallow the burst of events it made.
            std::thread::sleep(SETTLE + Duration::from_millis(200));
            while rx.try_recv().is_ok() {}
        }
        match scan_once(paths, workers, SETTLE) {
            Ok(r) => {
                for (p, o) in r {
                    tracing::info!("inbox: {} -> {:?}", p.display(), o);
                }
            }
            Err(e) => tracing::error!("inbox scan failed: {e}"),
        }
    }
    Ok(())
}
