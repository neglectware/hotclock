//! Temporary directories for tests, and the processes tests start, cleaned up without fail.
//!
//! Every test directory lives under `$TMPDIR/hotclock-tests/p<pid>/`, one directory per test process:
//!
//! - it is removed when the test process exits (an `atexit` handler: it runs after a passing and after a
//!   failing run, since libtest leaves through `exit`), unless `HOTCLOCK_KEEP_TEST_DIRS=1`;
//! - the first test directory a process asks for sweeps away the directories of test processes that are no
//!   longer running (killed by a signal or a timeout, so their handler never ran), and anything left from the
//!   older layout that is more than ten minutes old;
//! - [`TempDir`] removes its directory as soon as it is dropped, for tests that write a lot.
//!
//! Before this existed each test left its directory behind; `$TMPDIR/hotclock-tests` reached 71 GB and
//! filled the Mac's disk (2026-09-30). [`KillOnDrop`] is the same idea for child processes: a test that
//! panics between starting a `hotclock` process and killing it no longer leaves it running.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Once;
use std::time::{Duration, SystemTime};

/// `$TMPDIR/hotclock-tests`: the parent of every process's directory.
pub fn base() -> PathBuf {
    std::env::temp_dir().join("hotclock-tests")
}

fn keep() -> bool {
    std::env::var_os("HOTCLOCK_KEEP_TEST_DIRS").is_some_and(|v| v == "1")
}

#[allow(unsafe_code)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 sends nothing; it only reports whether the process exists.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

extern "C" fn remove_at_exit() {
    if !keep() {
        let _ = std::fs::remove_dir_all(process_dir_path());
    }
}

fn process_dir_path() -> PathBuf {
    base().join(format!("p{}", std::process::id()))
}

/// Remove what earlier test processes left: the directory of any process that is gone, and old-layout
/// directories more than ten minutes old.
pub fn sweep() {
    let Ok(rd) = std::fs::read_dir(base()) else {
        return;
    };
    let me = std::process::id();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        match name.strip_prefix('p').and_then(|p| p.parse::<u32>().ok()) {
            Some(pid) if pid == me => {}
            Some(pid) => {
                if !pid_alive(pid as i32) {
                    remove(&path);
                }
            }
            None => {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .is_some_and(|age| age > Duration::from_secs(600));
                if old {
                    remove(&path);
                }
            }
        }
    }
}

fn remove(p: &Path) {
    if p.is_dir() {
        let _ = std::fs::remove_dir_all(p);
    } else {
        let _ = std::fs::remove_file(p);
    }
}

/// This process's directory, created on first use (which also sweeps earlier runs and registers the removal
/// at exit).
#[allow(unsafe_code)]
pub fn process_dir() -> PathBuf {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        sweep();
        // SAFETY: registers a plain `extern "C" fn()` with no captured state.
        unsafe {
            libc::atexit(remove_at_exit);
        }
    });
    let d = process_dir_path();
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A fresh, empty directory `name` inside this process's directory. It goes when the process exits.
pub fn fresh(name: &str) -> PathBuf {
    let d = process_dir().join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create a test directory");
    d
}

/// A fresh directory removed when this value is dropped (and at the latest when the process exits).
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        TempDir(fresh(name))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !keep() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A child process that is killed and reaped when dropped, so a failing test cannot leave it running.
#[derive(Debug)]
pub struct KillOnDrop(pub Child);

impl Deref for KillOnDrop {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_dir_goes_when_dropped() {
        let p = {
            let t = TempDir::new("drop-check");
            std::fs::write(t.join("f"), b"x").unwrap();
            t.path().to_path_buf()
        };
        assert!(keep() || !p.exists());
    }

    #[test]
    fn a_dead_process_directory_is_swept() {
        // A pid that cannot be running: above every system's pid limit.
        let dead = base().join("p2147480000");
        std::fs::create_dir_all(dead.join("x")).unwrap();
        let _ = process_dir();
        sweep();
        assert!(!dead.exists());
        assert!(process_dir().exists(), "our own directory stays");
    }

    #[test]
    fn a_child_is_killed_when_dropped() {
        let c = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = c.id() as i32;
        drop(KillOnDrop(c));
        assert!(!pid_alive(pid));
    }
}
