//! Real music through the whole import pipeline, only when `HOTCLOCK_TESTDATA` points at the
//! read-only sets (never committed). Each set is imported into its own library and the three are
//! never mixed. The media store lives under the system temp directory, never in the repository.
//!
//! `import_rate_mp3` (acceptance 7 on this machine) is heavy and runs only with
//! `HOTCLOCK_IMPORT_RATE=1`.

use analysis::census::collect_files;
use library::import::import_many;
use library::{ImportOptions, ImportOutcome, Library, Paths};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn root() -> Option<PathBuf> {
    match std::env::var("HOTCLOCK_TESTDATA") {
        Ok(r) => Some(PathBuf::from(r)),
        Err(_) => {
            eprintln!("SKIPPED: HOTCLOCK_TESTDATA is not set");
            None
        }
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = fixtures::testdir::process_dir().join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn jobs(files: &[PathBuf]) -> Vec<(PathBuf, ImportOptions)> {
    files
        .iter()
        .map(|f| {
            (
                f.clone(),
                ImportOptions {
                    category: None,
                    source: "test".into(),
                },
            )
        })
        .collect()
}

#[test]
fn real_sets_import_and_analyze() {
    let Some(root) = root() else { return };
    for set in ["office", "preshow", "elevator"] {
        let dir = root.join(set);
        if !dir.is_dir() {
            continue;
        }
        let files = collect_files(&dir).unwrap();
        let data = tmp(set);
        let paths = Paths::from_data_dir(&data.join("data"));
        let lib = Library::open(paths.clone()).unwrap();
        let t0 = Instant::now();
        let res = import_many(&paths, jobs(&files), 2);
        let secs = t0.elapsed().as_secs_f64();
        let failed: Vec<_> = res
            .iter()
            .filter(|(_, o)| matches!(o, ImportOutcome::Failed { .. }))
            .collect();
        assert!(failed.is_empty(), "{set}: {failed:?}");
        let audio_s: f64 = lib
            .conn()
            .query_row("SELECT SUM(file_ms) / 1000.0 FROM items", [], |r| r.get(0))
            .unwrap();
        let n: i64 = lib
            .conn()
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap();
        let dups: usize = res
            .iter()
            .filter(|(_, o)| matches!(o, ImportOutcome::Duplicate { .. }))
            .count();
        let count_flag = |f: &str| -> i64 {
            lib.conn()
                .query_row(
                    "SELECT COUNT(*) FROM item_effective WHERE flags LIKE '%\"' || ?1 || '\"%'",
                    [f],
                    |r| r.get(0),
                )
                .unwrap()
        };
        println!(
            "{set}: {} files -> {n} items ({dups} duplicates), {:.1} min of audio in {secs:.1} s wall ({:.0}x real time, {:.2} files/s, 2 workers); flags: cold_start {} cold_end {} long_intro {} long_tail {} soft_start {} segue_suspect {} ambient_suspect {} clipped {} gapless_unknown {}",
            files.len(),
            audio_s / 60.0,
            audio_s / secs,
            files.len() as f64 / secs,
            count_flag("cold_start"),
            count_flag("cold_end"),
            count_flag("long_intro"),
            count_flag("long_tail"),
            count_flag("soft_start"),
            count_flag("segue_suspect"),
            count_flag("ambient_suspect"),
            count_flag("clipped"),
            count_flag("gapless_unknown"),
        );
        // Every item's markers satisfy the invariants and lie inside the file.
        let bad: i64 = lib
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM item_effective e JOIN items i ON i.id=e.item_id
                 WHERE NOT (e.cue_in >= 0 AND e.cue_in <= e.attack AND e.attack < e.segue AND e.segue <= e.cue_out AND e.cue_out <= i.file_ms AND e.cue_out - e.cue_in >= 250)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad, 0, "{set}");
        assert_eq!(n as usize + dups, files.len());
        // Artwork and tags were read where present.
        let with_art: i64 = lib
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE art_sha IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let with_album: i64 = lib
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM items WHERE album_key <> ''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        println!("{set}: {with_art} items with artwork, {with_album} with an album_key");
        let _ = std::fs::remove_dir_all(&data);
    }
}

fn ffmpeg_mp3(src: &Path, dst: &Path, kbps: u32) -> bool {
    Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-vn",
            "-map_metadata",
            "-1",
            "-c:a",
            "libmp3lame",
            "-b:a",
            &format!("{kbps}k"),
        ])
        .arg(dst)
        .status()
        .is_ok_and(|s| s.success())
}

/// Acceptance 7 on this machine: 500 real music files as MP3 through 2 import workers. The
/// station-playing-at-the-same-time half needs the target box (see docs/hardware-tests.md).
#[test]
fn import_rate_mp3() {
    if std::env::var("HOTCLOCK_IMPORT_RATE").is_err() {
        eprintln!("SKIPPED: set HOTCLOCK_IMPORT_RATE=1 to run the 500-file import-rate test");
        return;
    }
    let Some(root) = root() else { return };
    // 500 MP3s made from the elevator set (real library music): each track at 96, 128 and 192
    // kbit/s (480 files) plus the first 20 at 64 kbit/s. One set only.
    let src = collect_files(&root.join("elevator")).unwrap();
    let work = tmp("rate-src");
    let mut todo: Vec<(PathBuf, PathBuf, u32)> = vec![];
    for (i, f) in src.iter().enumerate() {
        for kbps in [96u32, 128, 192] {
            todo.push((f.clone(), work.join(format!("{i:03}-{kbps}.mp3")), kbps));
        }
        if i < 20 {
            todo.push((f.clone(), work.join(format!("{i:03}-64.mp3")), 64));
        }
    }
    let t0 = Instant::now();
    let todo = std::sync::Arc::new(std::sync::Mutex::new(todo));
    let made = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let (todo, made) = (todo.clone(), made.clone());
            std::thread::spawn(move || loop {
                let next = todo.lock().unwrap().pop();
                let Some((s, d, k)) = next else { break };
                if ffmpeg_mp3(&s, &d, k) {
                    made.lock().unwrap().push(d);
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let mut files = made.lock().unwrap().clone();
    files.sort();
    println!(
        "made {} MP3 files in {:.1} s",
        files.len(),
        t0.elapsed().as_secs_f64()
    );
    assert!(files.len() >= 495);
    let audio_s: f64 = 0.0;
    let _ = audio_s;
    let data = tmp("rate-lib");
    let paths = Paths::from_data_dir(&data.join("data"));
    let lib = Library::open(paths.clone()).unwrap();
    let t1 = Instant::now();
    let res = import_many(&paths, jobs(&files), 2);
    let secs = t1.elapsed().as_secs_f64();
    let failed = res
        .iter()
        .filter(|(_, o)| matches!(o, ImportOutcome::Failed { .. }))
        .count();
    let dups = res
        .iter()
        .filter(|(_, o)| matches!(o, ImportOutcome::Duplicate { .. }))
        .count();
    let minutes: f64 = lib
        .conn()
        .query_row("SELECT SUM(file_ms) / 60000.0 FROM items", [], |r| r.get(0))
        .unwrap();
    let rate = files.len() as f64 / secs;
    println!(
        "import rate: {} MP3 files ({minutes:.0} min of audio, {failed} failed, {dups} duplicates) in {secs:.1} s with 2 workers = {rate:.2} files/s = {:.2} files/s per worker (limit 3.0); {:.0}x real time",
        files.len(),
        rate / 2.0,
        minutes * 60.0 / secs
    );
    assert_eq!(failed, 0);
    assert!(rate / 2.0 >= 3.0, "{:.2} files/s per worker", rate / 2.0);
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&work);
}
