//! The content-addressed media store (architecture 5.1): `media/ab/<sha256>.<ext>`, written once,
//! `0444`, never modified. Every write goes through `upload-tmp/`: write, fsync, rename. A crash
//! at any point therefore leaves either the finished file or nothing under `media/`.

use crate::Result;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Remove everything in `upload-tmp/` (the remains of interrupted copies). Called on start.
pub fn clean_upload_tmp(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    Ok(())
}

/// Relative store path of an audio file: `ab/abcdef....ext`.
pub fn store_rel(sha: &str, ext: &str) -> String {
    format!("{}/{sha}.{ext}", &sha[..2])
}

pub fn art_rel(sha: &str, size: u32) -> String {
    format!("art/{}/{sha}-{size}.jpg", &sha[..2])
}

/// A unique name inside `upload-tmp/`.
pub fn tmp_name(dir: &Path, tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{}-{n}-{tag}.part", std::process::id()))
}

/// Optional pause between copy chunks, for the crash test (`HOTCLOCK_TEST_COPY_DELAY_MS`).
fn test_delay() -> Option<Duration> {
    std::env::var("HOTCLOCK_TEST_COPY_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
}

/// Copy `src` to `tmp` (fsync'd). The chunk size is small enough that the delay hook can hold a
/// partial file open.
pub fn copy_to_tmp(src: &Path, tmp: &Path) -> Result<()> {
    let mut input = std::fs::File::open(src)?;
    let mut out = std::fs::File::create(tmp)?;
    let delay = test_delay();
    let mut buf = vec![0u8; if delay.is_some() { 64 * 1024 } else { 1 << 20 }];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
    }
    out.sync_all()?;
    Ok(())
}

/// Atomically publish a finished temp file at `dest`, mode `0444`. If `dest` already exists
/// (same hash, so the same bytes) the temp file is simply dropped. Returns whether this call
/// created the file.
pub fn publish(tmp: &Path, dest: &Path) -> Result<bool> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        let _ = std::fs::remove_file(tmp);
        return Ok(false);
    }
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o444))?;
    std::fs::rename(tmp, dest)?;
    if let Some(parent) = dest.parent() {
        if let Ok(d) = std::fs::File::open(parent) {
            let _ = d.sync_all();
        }
    }
    Ok(true)
}

/// Every regular file below `dir`, for tests and the verify job.
pub fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_files(&p, out);
        } else {
            out.push(p);
        }
    }
}
