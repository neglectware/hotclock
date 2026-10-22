//! Memory, threads, file descriptors and CPU time of a process, for the soaks (M1.8). Linux reads `/proc`;
//! macOS asks `ps` and `lsof` (the numbers the design's soak needs are the same on both, so the accelerated
//! soak run on the Mac and the hardware soak on the target box report the same columns).

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProcSample {
    pub rss_kb: u64,
    /// The memory the process really holds: `RssAnon` on Linux, the physical footprint on macOS. (macOS's
    /// RSS counts pages the allocator has freed but not returned, which read as growth and are not.)
    pub mem_kb: u64,
    pub threads: u32,
    pub fds: u32,
    /// User plus system CPU seconds since the process started.
    pub cpu_s: f64,
}

/// Bytes allocated and not yet freed by this process, when it was built with the `alloc-count` feature.
pub fn live_heap_bytes() -> Option<u64> {
    #[cfg(feature = "alloc-count")]
    {
        Some(counting::LIVE.load(std::sync::atomic::Ordering::Relaxed) as u64)
    }
    #[cfg(not(feature = "alloc-count"))]
    {
        None
    }
}

/// Bytes SQLite holds (its own allocator, not Rust's), with the `alloc-count` feature.
pub fn sqlite_bytes() -> Option<u64> {
    #[cfg(feature = "alloc-count")]
    {
        Some(counting::sqlite_used())
    }
    #[cfg(not(feature = "alloc-count"))]
    {
        None
    }
}

/// With `alloc-count` and `HOTCLOCK_LIVE_HEAP_LOG=<file>`, append `unix_ms pid live_heap_kb sqlite_kb` to the file every
/// 30 s (the soaks' services: a leak shows here and not in the allocator's resident pages).
pub fn log_live_heap_from_env() {
    #[cfg(feature = "alloc-count")]
    if let Some(path) = std::env::var_os("HOTCLOCK_LIVE_HEAP_LOG") {
        let _ = std::thread::Builder::new()
            .name("live-heap".into())
            .spawn(move || loop {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis());
                    let _ = writeln!(
                        f,
                        "{ms} {} {} {}",
                        std::process::id(),
                        live_heap_bytes().unwrap_or(0) / 1024,
                        sqlite_bytes().unwrap_or(0) / 1024
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(30));
            });
    }
}

#[cfg(feature = "alloc-count")]
#[allow(unsafe_code)]
pub mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub static LIVE: AtomicUsize = AtomicUsize::new(0);

    /// `sqlite3_memory_used()`: SQLite's own allocations (its page caches, statements), which Rust's allocator
    /// never sees.
    pub fn sqlite_used() -> u64 {
        // SAFETY: a read of SQLite's global memory statistic; no arguments, no state.
        let n = unsafe { rusqlite::ffi::sqlite3_memory_used() };
        n.max(0) as u64
    }

    pub struct Counting;

    // SAFETY: every call is forwarded to `System` unchanged; only the byte count is kept.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                LIVE.fetch_add(l.size(), Relaxed);
            }
            p
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc_zeroed(l) };
            if !p.is_null() {
                LIVE.fetch_add(l.size(), Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            LIVE.fetch_sub(l.size(), Relaxed);
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            let q = unsafe { System.realloc(p, l, n) };
            if !q.is_null() {
                LIVE.fetch_sub(l.size(), Relaxed);
                LIVE.fetch_add(n, Relaxed);
            }
            q
        }
    }
}

pub fn sample(pid: u32) -> Option<ProcSample> {
    #[cfg(target_os = "linux")]
    {
        linux(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        other(pid)
    }
}

#[cfg(target_os = "linux")]
fn linux(pid: u32) -> Option<ProcSample> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |k: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
    };
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised name: utime is the 12th and stime the 13th after `)`.
    let after = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks: f64 = f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?;
    let rss_kb = field("VmRSS:")?;
    Some(ProcSample {
        rss_kb,
        mem_kb: field("RssAnon:").unwrap_or(rss_kb),
        threads: field("Threads:")? as u32,
        fds: std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count() as u32,
        cpu_s: ticks / 100.0,
    })
}

#[cfg(not(target_os = "linux"))]
fn other(pid: u32) -> Option<ProcSample> {
    use std::process::Command;
    let out = Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut it = text.split_whitespace();
    let rss_kb: u64 = it.next()?.parse().ok()?;
    let cpu_s = parse_cpu_time(it.next()?)?;
    let threads = Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .count()
                .saturating_sub(1) as u32
        })
        .unwrap_or(0);
    let fds = if pid == std::process::id() {
        std::fs::read_dir("/dev/fd")
            .map(|d| d.count() as u32)
            .unwrap_or(0)
    } else {
        Command::new("lsof")
            .args(["-p", &pid.to_string()])
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .count()
                    .saturating_sub(1) as u32
            })
            .unwrap_or(0)
    };
    let mem_kb = Command::new("footprint")
        .args([
            "-p",
            &pid.to_string(),
            "--format",
            "bytes",
            "--noCategories",
        ])
        .output()
        .ok()
        .and_then(|o| {
            let t = String::from_utf8_lossy(&o.stdout).into_owned();
            let after = t.split("Footprint:").nth(1)?;
            after
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|b| b / 1024)
        })
        .unwrap_or(rss_kb);
    Some(ProcSample {
        rss_kb,
        mem_kb,
        threads,
        fds,
        cpu_s,
    })
}

/// `ps` CPU time, `[[dd-]hh:]mm:ss[.cc]` or `mm:ss.cc` (the macOS path reads it; Linux reads `/proc`).
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn parse_cpu_time(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut total = 0.0;
    for part in rest.split(':') {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + total)
}

/// Total size in bytes of the files under `dir` (recursive; unreadable entries count as zero).
pub fn dir_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => total += dir_bytes(&e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_times_parse() {
        assert_eq!(parse_cpu_time("0:01.50"), Some(1.5));
        assert_eq!(parse_cpu_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_cpu_time("2-00:00:10"), Some(172_810.0));
        assert_eq!(parse_cpu_time("x"), None);
    }

    #[test]
    fn the_own_process_has_numbers() {
        let s = sample(std::process::id()).unwrap();
        assert!(s.rss_kb > 1000 && s.threads >= 1 && s.fds >= 3, "{s:?}");
    }
}
