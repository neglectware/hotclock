//! Pre-flight of a queued item (architecture 1.5): before an item is played, its file is opened, its
//! first and last 64 KB are read and its size is compared with the size the library recorded. It runs
//! on its own thread (a failing disk must never stall the conductor's step), and again at commit.
//!
//! The verdicts map to the failure table: a file that is gone is `item_missing`, one whose size differs
//! `item_truncated` (smaller) or a hash mismatch (larger), and a read that fails with an I/O error is
//! evidence about the media store, not about the item (`media_store_failing`).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// How much of each end of the file is read.
pub const EDGE_BYTES: u64 = 64 * 1024;

/// A test hook (never set in production): make reads fail as the media store would (12: "with reads
/// failing on every file (a test hook)").
#[derive(Default)]
pub struct MediaFaults {
    /// Every read fails with an I/O error.
    pub fail_all: AtomicBool,
    /// Reads of these files fail with an I/O error.
    pub fail_paths: Mutex<HashSet<PathBuf>>,
}

impl MediaFaults {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn injects(&self, path: &Path) -> bool {
        self.fail_all.load(Acquire) || self.fail_paths.lock().is_ok_and(|g| g.contains(path))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Ok,
    Missing(String),
    /// Smaller than the library says.
    Truncated {
        actual: u64,
        expected: u64,
    },
    /// Larger than the library says (a different file under the same name).
    Mismatch {
        actual: u64,
        expected: u64,
    },
    /// An I/O error other than "not found": the media store, not the item.
    Unreadable(String),
}

impl Verdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, Verdict::Ok)
    }
}

/// A file that is not there is the item's fault only if the store around it is: when the media directory
/// itself is gone or empty (a disk that did not mount, a network share that dropped), every file is "not
/// found", and quarantining them one by one would empty the library. The store is `media/ab/<hash>.<ext>`.
fn not_found(path: &Path) -> Verdict {
    let root = path.parent().and_then(Path::parent);
    let store_there = root
        .and_then(|r| std::fs::read_dir(r).ok())
        .is_some_and(|mut d| d.next().is_some());
    if store_there {
        Verdict::Missing(format!("{}: not found", path.display()))
    } else {
        Verdict::Unreadable(format!(
            "{}: not found, and the media store around it is gone or empty",
            path.display()
        ))
    }
}

/// Check one file now, on the calling thread.
pub fn check(path: &Path, expected_bytes: u64, faults: &MediaFaults) -> Verdict {
    if faults.injects(path) {
        return Verdict::Unreadable("read failed (injected)".into());
    }
    let md = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return not_found(path),
        Err(e) => return Verdict::Unreadable(format!("{}: {e}", path.display())),
    };
    let actual = md.len();
    if expected_bytes > 0 {
        if actual < expected_bytes {
            return Verdict::Truncated {
                actual,
                expected: expected_bytes,
            };
        }
        if actual > expected_bytes {
            return Verdict::Mismatch {
                actual,
                expected: expected_bytes,
            };
        }
    }
    let read_edges = || -> std::io::Result<()> {
        let mut f = std::fs::File::open(path)?;
        let n = actual.min(EDGE_BYTES) as usize;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf)?;
        if actual > EDGE_BYTES {
            f.seek(SeekFrom::Start(actual - n as u64))?;
            f.read_exact(&mut buf)?;
        }
        Ok(())
    };
    match read_edges() {
        Ok(()) => Verdict::Ok,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => not_found(path),
        Err(e) => Verdict::Unreadable(format!("{}: {e}", path.display())),
    }
}

struct Request {
    item_id: i64,
    path: PathBuf,
    expected: u64,
}

pub struct Response {
    pub item_id: i64,
    pub verdict: Verdict,
}

/// The pre-flight worker and what it has found.
pub struct Preflight {
    tx: Sender<Request>,
    rx: Receiver<Response>,
    /// Answers `settle` has taken off the channel, for the next `poll`.
    early: Vec<Response>,
    pub faults: Arc<MediaFaults>,
    pending: HashMap<i64, i64>,
    /// The last verdict per item and the station wall time it arrived at.
    pub results: HashMap<i64, (Verdict, i64)>,
}

/// A verdict this recent is trusted at commit (ms).
pub const FRESH_MS: i64 = 5_000;
/// A request unanswered for this long counts as an unreadable store (a hung read).
pub const TIMEOUT_MS: i64 = 4_000;

impl Preflight {
    pub fn new(faults: Arc<MediaFaults>) -> Self {
        let (tx, worker_rx) = channel::<Request>();
        let (worker_tx, rx) = channel::<Response>();
        let f = faults.clone();
        std::thread::Builder::new()
            .name("preflight".into())
            .spawn(move || {
                for r in worker_rx {
                    let verdict = check(&r.path, r.expected, &f);
                    if worker_tx
                        .send(Response {
                            item_id: r.item_id,
                            verdict,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn preflight thread");
        Self {
            tx,
            rx,
            faults,
            early: Vec::new(),
            pending: HashMap::new(),
            results: HashMap::new(),
        }
    }

    /// Ask for a check of `item_id` unless one is on its way.
    pub fn request(&mut self, item_id: i64, path: PathBuf, expected: u64, now_ms: i64) {
        if self.pending.contains_key(&item_id) {
            return;
        }
        self.pending.insert(item_id, now_ms);
        let _ = self.tx.send(Request {
            item_id,
            path,
            expected,
        });
    }

    /// Collect the verdicts that arrived; requests that have waited past [`TIMEOUT_MS`] are answered
    /// `Unreadable` (the read is hung). Returns `(item, verdict)` of every new result.
    pub fn poll(&mut self, now_ms: i64) -> Vec<(i64, Verdict)> {
        let mut out = Vec::new();
        let early = std::mem::take(&mut self.early);
        for r in early {
            self.pending.remove(&r.item_id);
            self.results.insert(r.item_id, (r.verdict.clone(), now_ms));
            out.push((r.item_id, r.verdict));
        }
        while let Ok(r) = self.rx.try_recv() {
            self.pending.remove(&r.item_id);
            self.results.insert(r.item_id, (r.verdict.clone(), now_ms));
            out.push((r.item_id, r.verdict));
        }
        let stuck: Vec<i64> = self
            .pending
            .iter()
            .filter(|(_, t)| now_ms - **t > TIMEOUT_MS)
            .map(|(i, _)| *i)
            .collect();
        for i in stuck {
            self.pending.remove(&i);
            let v = Verdict::Unreadable("the read did not return".into());
            self.results.insert(i, (v.clone(), now_ms));
            out.push((i, v));
        }
        out
    }

    /// The verdict for `item_id` if it is younger than [`FRESH_MS`].
    pub fn fresh(&self, item_id: i64, now_ms: i64) -> Option<&Verdict> {
        self.results
            .get(&item_id)
            .filter(|(_, t)| now_ms - *t <= FRESH_MS)
            .map(|(v, _)| v)
    }

    /// Wait, up to `max`, for the answer to every request sent so far, and hold the answers for the next
    /// `poll`. For offline renders, whose virtual clock runs far faster than the worker thread: a request
    /// times out on the station's clock, not on the wall's.
    pub fn settle(&mut self, max: std::time::Duration) {
        let t = std::time::Instant::now();
        while self.early.len() < self.pending.len() && t.elapsed() < max {
            if let Ok(r) = self.rx.recv_timeout(std::time::Duration::from_millis(1)) {
                self.early.push(r);
            }
        }
    }

    pub fn is_pending(&self, item_id: i64) -> bool {
        self.pending.contains_key(&item_id)
    }

    pub fn forget(&mut self, item_id: i64) {
        self.results.remove(&item_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = fixtures::testdir::process_dir().join(format!("pf-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn verdicts_for_each_kind_of_failure() {
        let d = tmp("v");
        let faults = MediaFaults::new();
        let f = d.join("a.bin");
        std::fs::write(&f, vec![7u8; 300_000]).unwrap();
        assert_eq!(check(&f, 300_000, &faults), Verdict::Ok);
        assert!(matches!(
            check(&f, 400_000, &faults),
            Verdict::Truncated {
                actual: 300_000,
                expected: 400_000
            }
        ));
        assert!(matches!(check(&f, 100, &faults), Verdict::Mismatch { .. }));
        assert!(matches!(
            check(&d.join("gone"), 1, &faults),
            Verdict::Missing(_)
        ));
        // A store that is not there at all (an empty mount point): every file is "not found", and that is
        // the store's failure, not each item's.
        let media = d.join("media");
        std::fs::create_dir_all(&media).unwrap();
        assert!(matches!(
            check(&media.join("ab").join("abcd.flac"), 1, &faults),
            Verdict::Unreadable(_)
        ));
        std::fs::create_dir_all(media.join("cd")).unwrap();
        assert!(matches!(
            check(&media.join("ab").join("abcd.flac"), 1, &faults),
            Verdict::Missing(_)
        ));
        faults.fail_paths.lock().unwrap().insert(f.clone());
        assert!(matches!(
            check(&f, 300_000, &faults),
            Verdict::Unreadable(_)
        ));
        faults.fail_paths.lock().unwrap().clear();
        faults.fail_all.store(true, Release);
        assert!(matches!(
            check(&f, 300_000, &faults),
            Verdict::Unreadable(_)
        ));
    }

    #[test]
    fn the_worker_answers_and_results_go_stale() {
        let d = tmp("w");
        let f = d.join("b.bin");
        std::fs::write(&f, vec![1u8; 1000]).unwrap();
        let mut p = Preflight::new(MediaFaults::new());
        p.request(1, f.clone(), 1000, 0);
        p.request(2, d.join("nope"), 5, 0);
        let mut got = vec![];
        let t = std::time::Instant::now();
        while got.len() < 2 && t.elapsed().as_secs() < 5 {
            got.extend(p.poll(10));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        got.sort_by_key(|g| g.0);
        assert_eq!(got[0].1, Verdict::Ok);
        assert!(matches!(got[1].1, Verdict::Missing(_)));
        assert!(p.fresh(1, 1_000).is_some());
        assert!(p.fresh(1, 20_000).is_none());
    }
}
