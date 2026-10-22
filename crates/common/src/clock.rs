//! The `Clock` trait. Every part of the scheduler and every paced output takes time from a
//! `Clock`; production uses [`RealClock`], tests and the accelerated soak use [`VirtualClock`]
//! (architecture 1.6).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A source of time and a way to wait for it.
pub trait Clock: Send + Sync {
    /// Monotonic time since an arbitrary origin, in nanoseconds. Never goes backwards.
    fn monotonic_ns(&self) -> u64;
    /// Wall-clock time as nanoseconds since the Unix epoch (`CLOCK_REALTIME`).
    fn realtime_unix_ns(&self) -> i128;
    /// Block until `monotonic_ns() >= target_ns`. A virtual clock jumps there instead.
    fn sleep_until_ns(&self, target_ns: u64);

    fn sleep_ns(&self, ns: u64) {
        self.sleep_until_ns(self.monotonic_ns().saturating_add(ns));
    }
}

/// The real clocks: `Instant` for monotonic time, `SystemTime` for wall time.
pub struct RealClock {
    origin: Instant,
}

impl RealClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn monotonic_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }

    fn realtime_unix_ns(&self) -> i128 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        }
    }

    fn sleep_until_ns(&self, target_ns: u64) {
        loop {
            let now = self.monotonic_ns();
            if now >= target_ns {
                return;
            }
            std::thread::sleep(Duration::from_nanos(target_ns - now));
        }
    }
}

/// A clock that only moves when told to, or when something sleeps on it. Sleeping advances time
/// to the target immediately, so a paced output driven by a virtual clock runs as fast as the CPU
/// allows. Intended for a single driving thread (the offline renderer).
pub struct VirtualClock {
    now_ns: AtomicU64,
    unix_origin_ns: i128,
}

impl VirtualClock {
    /// Start at monotonic 0, with wall time `unix_origin_ns` at that instant.
    pub fn new(unix_origin_ns: i128) -> Self {
        Self {
            now_ns: AtomicU64::new(0),
            unix_origin_ns,
        }
    }

    pub fn advance_ns(&self, ns: u64) {
        self.now_ns.fetch_add(ns, Ordering::SeqCst);
    }
}

impl Clock for VirtualClock {
    fn monotonic_ns(&self) -> u64 {
        self.now_ns.load(Ordering::SeqCst)
    }

    fn realtime_unix_ns(&self) -> i128 {
        self.unix_origin_ns + self.monotonic_ns() as i128
    }

    fn sleep_until_ns(&self, target_ns: u64) {
        self.now_ns.fetch_max(target_ns, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_jumps_on_sleep() {
        let c = VirtualClock::new(1_000);
        assert_eq!(c.monotonic_ns(), 0);
        c.sleep_ns(5_000);
        assert_eq!(c.monotonic_ns(), 5_000);
        c.sleep_until_ns(2_000); // in the past: no effect
        assert_eq!(c.monotonic_ns(), 5_000);
        assert_eq!(c.realtime_unix_ns(), 6_000);
    }

    #[test]
    fn real_clock_is_monotonic_and_sleeps() {
        let c = RealClock::new();
        let a = c.monotonic_ns();
        c.sleep_ns(2_000_000);
        let b = c.monotonic_ns();
        assert!(b - a >= 2_000_000, "slept {} ns", b - a);
    }
}
