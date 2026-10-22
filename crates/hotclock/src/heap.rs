//! Giving memory back after the nightly snapshot (soak 2, 2026-10-07).
//!
//! The library service stepped from 2.2 MB to 8.3 MB of anonymous memory at its second nightly snapshot, a step
//! the size of the database it copies (6.4 MB), and stayed there. `VACUUM INTO` reads the whole database through
//! SQLite's page cache and writes it through another, in the snapshot thread's own malloc arena; glibc keeps
//! those freed pages for the next call. [`trim`] hands them back (`malloc_trim(0)`: every arena, and the free
//! pages inside the heap, not only the top). This is the one place besides the soaks' counting allocator where
//! the package uses `unsafe`.

/// Ask glibc to return free heap pages to the system. True when it released something. Does nothing (false)
/// where the C library is not glibc, because `malloc_trim` is glibc's.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
pub fn trim() -> bool {
    // SAFETY: `malloc_trim` takes one integer, touches only the allocator's own free lists, and is safe to call
    // from any thread at any time.
    unsafe { libc::malloc_trim(0) != 0 }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn trim() -> bool {
    false
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;
    use std::path::Path;

    /// `RssAnon` of this process, kB.
    fn anon_kb() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("RssAnon:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap()
    }

    /// A database of about `mb` MB. `analysis` is the library's shape (soak 2's `library.db`: 6.1 of its 6.4 MB
    /// is the `analysis` table, 53 rows of about 115 kB each, which SQLite reads and writes one overflow chain
    /// at a time); `small` is many 320-byte rows with an index.
    fn make_db(path: &Path, mb: usize, shape: &str) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t(id INTEGER PRIMARY KEY, k TEXT, v BLOB); CREATE INDEX tk ON t(k);",
        )
        .unwrap();
        let (blob_len, rows) = if shape == "small" {
            (300usize, mb * 1024 * 1024 / 320)
        } else {
            (115_000usize, mb * 1024 * 1024 / 115_000)
        };
        let tx = c.unchecked_transaction().unwrap();
        {
            let mut ins = tx.prepare("INSERT INTO t(k, v) VALUES (?1, ?2)").unwrap();
            for i in 0..rows {
                // Not compressible away and not identical: varying bytes per row.
                let blob: Vec<u8> = (0..blob_len as u32)
                    .map(|j| {
                        (i as u32)
                            .wrapping_mul(2654435761)
                            .wrapping_add(j.wrapping_mul(40503)) as u8
                    })
                    .collect();
                ins.execute(rusqlite::params![
                    format!("k{:08x}", i.wrapping_mul(2654435761)),
                    blob
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }

    /// The child of [`snapshot_memory_with_and_without_trim`]: one variant, in a process of its own (the
    /// allocator's state would carry from one variant to the next in a shared process). It runs the snapshots
    /// on one long-lived thread, as the library service does, and prints one `RESULT` line.
    #[test]
    #[ignore = "run by snapshot_memory_with_and_without_trim"]
    fn snapshot_memory_child() {
        let variant = std::env::var("HC_HEAP_VARIANT").unwrap_or_default();
        let mb: usize = std::env::var("HC_HEAP_DB_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7);
        let dir = fixtures::testdir::process_dir().join(format!(
            "heap-{}-{variant}-{shape0}",
            std::process::id(),
            shape0 = std::env::var("HC_HEAP_SHAPE").unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.db");
        let shape = std::env::var("HC_HEAP_SHAPE").unwrap_or_default();
        if let Some(real) = std::env::var_os("HC_HEAP_DB") {
            // A real database to copy from (never in CI): the soak's `library.db`.
            std::fs::copy(real, &db).unwrap();
        } else {
            make_db(&db, mb, &shape);
        }
        let db_bytes = std::fs::metadata(&db).unwrap().len();
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let (d2, bk) = (db.clone(), dir.join("backup"));
        let trim_after = variant == "trim";
        let th = std::thread::spawn(move || {
            while let Ok(n) = rx.recv() {
                let date = format!("2026100{n}");
                let i = library::backup::snapshot(&d2, &bk, "library", &date, 0).unwrap();
                assert!(i.ok(), "{i:?}");
                if trim_after {
                    trim();
                }
                done_tx.send(()).unwrap();
            }
        });
        let before = anon_kb();
        let mut after = vec![];
        for n in 1..=5 {
            tx.send(n).unwrap();
            done_rx.recv().unwrap();
            after.push(anon_kb());
        }
        drop(tx);
        th.join().unwrap();
        println!(
            "RESULT variant={variant} shape={shape} db_bytes={db_bytes} before_kb={before} after_each_kb={}",
            after
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    /// Several snapshots of a multi-MB database, with and without `malloc_trim(0)` after each, each variant in a
    /// process of its own with the library service's arena setting (`MALLOC_ARENA_MAX=2`). The numbers are printed
    /// (run with `--nocapture`); the check is that trimming ends no higher than not trimming, and well under the
    /// size of the database above where it began.
    #[test]
    fn snapshot_memory_with_and_without_trim() {
        let exe = std::env::current_exe().unwrap();
        let run = |variant: &str, shape: &str| -> (u64, Vec<u64>, u64) {
            let out = std::process::Command::new(&exe)
                .args([
                    "--ignored",
                    "--exact",
                    "heap::tests::snapshot_memory_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("HC_HEAP_VARIANT", variant)
                .env("HC_HEAP_SHAPE", shape)
                .env("MALLOC_ARENA_MAX", "2")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let line = text
                .lines()
                .find_map(|l| l.find("RESULT ").map(|i| &l[i..]))
                .unwrap_or_else(|| {
                    panic!(
                        "no RESULT: {text}\n{}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                });
            println!("{line}");
            let get = |k: &str| {
                line.split_whitespace()
                    .find_map(|w| w.strip_prefix(&format!("{k}=")))
                    .unwrap()
                    .to_string()
            };
            (
                get("before_kb").parse().unwrap(),
                get("after_each_kb")
                    .split(',')
                    .map(|v| v.parse().unwrap())
                    .collect(),
                get("db_bytes").parse().unwrap(),
            )
        };
        for shape in ["analysis", "small"] {
            let (b0, plain, db_bytes) = run("plain", shape);
            let (b1, trimmed, _) = run("trim", shape);
            let kb = |v: &[u64]| *v.last().unwrap();
            println!(
                "{shape}: database {:.1} MB; RssAnon before the first snapshot: plain {} kB, trim {} kB; after five snapshots: plain {} kB (+{} kB), trim {} kB (+{} kB)",
                db_bytes as f64 / 1048576.0,
                b0,
                b1,
                kb(&plain),
                kb(&plain) as i64 - b0 as i64,
                kb(&trimmed),
                kb(&trimmed) as i64 - b1 as i64
            );
            assert!(
                kb(&trimmed) <= kb(&plain),
                "{shape}: trimming must not leave more memory held than not trimming"
            );
        }
    }
}
