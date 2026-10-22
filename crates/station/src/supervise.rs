//! What keeps the process itself honest (architecture 1.5, 10): the systemd watchdog (`sd_notify`), the
//! engine-stall detector that decides when to stop feeding it, and the start counter that puts a
//! crash-looping station into safe mode. No systemd library: the protocol is one datagram to the socket
//! named in `NOTIFY_SOCKET`.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

/// `sd_notify` (`man sd_notify`): `READY=1`, `WATCHDOG=1`, `STATUS=...`. A no-op when `NOTIFY_SOCKET` is
/// not set (a station run by hand).
pub struct Notifier {
    sock: Option<UnixDatagram>,
    target: Option<String>,
    /// `WATCHDOG_USEC`: the unit's `WatchdogSec` in microseconds, when the watchdog is on.
    pub watchdog_usec: Option<u64>,
}

impl Notifier {
    pub fn from_env() -> Self {
        let target = std::env::var("NOTIFY_SOCKET")
            .ok()
            .filter(|s| !s.is_empty());
        let watchdog_usec = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0);
        let sock = target.as_ref().and_then(|_| UnixDatagram::unbound().ok());
        Self {
            sock,
            target,
            watchdog_usec,
        }
    }

    fn send(&self, msg: &str) -> io::Result<()> {
        let (Some(sock), Some(target)) = (&self.sock, &self.target) else {
            return Ok(());
        };
        if let Some(name) = target.strip_prefix('@') {
            #[cfg(target_os = "linux")]
            {
                use std::os::linux::net::SocketAddrExt;
                let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
                sock.send_to_addr(msg.as_bytes(), &addr)?;
                return Ok(());
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = name;
                return Ok(());
            }
        }
        sock.send_to(msg.as_bytes(), Path::new(target))?;
        Ok(())
    }

    pub fn ready(&self) {
        let _ = self.send("READY=1\n");
    }

    pub fn watchdog(&self) {
        let _ = self.send("WATCHDOG=1\n");
    }

    /// `WATCHDOG=trigger` (systemd 243 and later): "the service detected an internal error that should be
    /// handled by the configured watchdog options", at once, without waiting out `WatchdogSec`. Sent when
    /// the engine stall is detected (3 s); the feed stopping (1 s after the stall) is the backstop for a
    /// systemd that ignores it, and for a process too stuck to send anything.
    pub fn trigger(&self) {
        let _ = self.send("WATCHDOG=trigger\n");
    }

    pub fn status(&self, s: &str) {
        let _ = self.send(&format!("STATUS={s}\n"));
    }

    /// How often to feed the watchdog: a third of its period (systemd suggests half; a third leaves room
    /// for one late loop).
    pub fn feed_interval(&self) -> Option<Duration> {
        self.watchdog_usec.map(|u| Duration::from_micros(u / 3))
    }
}

/// Engine-stall detection (1.5): `frames_written` not advancing for [`StallWatch::AFTER_MS`] while the
/// device is open means rt-out is stuck. Then the process stops feeding the watchdog and systemd restarts
/// it. A device that is gone is not a stall (the station is healthy, the hardware is not).
#[derive(Debug, Default)]
pub struct StallWatch {
    frames: u64,
    since_ms: i64,
    started: bool,
}

impl StallWatch {
    pub const AFTER_MS: i64 = 3_000;
    /// The watchdog is fed only while the engine advanced within this long (or the card is away). The
    /// alert waits 3 s and then asks systemd to act at once (`WATCHDOG=trigger`): the process is killed about
    /// 3 s after the stall begins and, with the unit's `RestartSec=2`, running again at about 5 s. If the
    /// trigger is not honoured, the feed has stopped at 1 s: the last feed is at most a second after the stall
    /// begins, `WatchdogSec` 10 s ends it by 11 s, and `RestartSec` 2 s has the new process up at 13 s plus its
    /// start. That backstop alone is at the edge of acceptance 5's 13 s; the trigger gives it 8 s of margin.
    pub const FEED_GRACE_MS: i64 = 1_000;

    /// Feed one observation; returns true while the engine is stalled.
    pub fn observe(&mut self, frames_written: u64, device_open: bool, now_ms: i64) -> bool {
        if !self.started || frames_written != self.frames || !device_open {
            self.started = true;
            self.frames = frames_written;
            self.since_ms = now_ms;
            return false;
        }
        now_ms - self.since_ms >= Self::AFTER_MS
    }

    /// May the watchdog be fed at `now_ms`? Yes while the engine advanced within [`Self::FEED_GRACE_MS`],
    /// and always while the card is away (the process is healthy, the hardware is not).
    pub fn feed_ok(&self, device_open: bool, now_ms: i64) -> bool {
        !device_open || !self.started || now_ms - self.since_ms < Self::FEED_GRACE_MS
    }
}

/// The start counter: `/run/hotclock/<name>.starts` holds the unix-ms time of each recent start.
pub mod crashloop {
    use std::path::Path;

    pub const WINDOW_MS: i64 = 120_000;
    /// This many starts within the window put the station in safe mode (1.5).
    pub const LIMIT: usize = 5;

    /// Record a start and return how many starts (this one included) fall in the last [`WINDOW_MS`].
    /// Errors (a `/run` that cannot be written) count as no history: 1.
    pub fn record_start(path: &Path, now_ms: i64) -> usize {
        let mut times: Vec<i64> = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|t| now_ms - t <= WINDOW_MS && *t <= now_ms)
            .collect();
        times.push(now_ms);
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let body: String = times.iter().map(|t| format!("{t}\n")).collect();
        let _ = std::fs::write(path, body);
        times.len()
    }

    /// Forget the history (an operator acknowledged safe mode).
    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_needs_three_quiet_seconds_and_an_open_device() {
        let mut w = StallWatch::default();
        assert!(!w.observe(1000, true, 0));
        assert!(!w.observe(2000, true, 1000));
        assert!(!w.observe(2000, true, 3900)); // 2.9 s
        assert!(w.observe(2000, true, 4000)); // 3.0 s
        assert!(w.observe(2000, true, 9000));
        assert!(!w.observe(3000, true, 9500)); // it moved
                                               // A device that is gone is not a stall, however long.
        assert!(!w.observe(3000, false, 20_000));
        assert!(!w.observe(3000, false, 60_000));
        assert!(w.feed_ok(false, 60_000), "fed while the card is away");
        // Feeding stops a second after the engine stops, before the alert.
        let mut w = StallWatch::default();
        w.observe(10, true, 0);
        w.observe(20, true, 500);
        assert!(w.feed_ok(true, 1_400));
        assert!(!w.feed_ok(true, 1_600));
        assert!(!w.observe(20, true, 1_600), "not yet an alert");
    }

    #[test]
    fn five_starts_in_two_minutes_is_a_crash_loop() {
        let d = fixtures::testdir::process_dir().join(format!("starts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("office.starts");
        let t0 = 1_800_000_000_000;
        let counts: Vec<usize> = (0..6)
            .map(|i| crashloop::record_start(&f, t0 + i * 10_000))
            .collect();
        assert_eq!(counts, vec![1, 2, 3, 4, 5, 6]);
        assert!(counts[4] >= crashloop::LIMIT);
        // Two minutes after the last of them the window has emptied.
        assert_eq!(crashloop::record_start(&f, t0 + 50_000 + 121_000), 1);
        crashloop::clear(&f);
        assert_eq!(crashloop::record_start(&f, t0), 1);
    }

    #[test]
    fn notifier_delivers_to_a_socket() {
        let d = fixtures::testdir::process_dir().join(format!("notify-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let sock_path = d.join("n.sock");
        let _ = std::fs::remove_file(&sock_path);
        let srv = UnixDatagram::bind(&sock_path).unwrap();
        srv.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let n = Notifier {
            sock: Some(UnixDatagram::unbound().unwrap()),
            target: Some(sock_path.display().to_string()),
            watchdog_usec: Some(10_000_000),
        };
        n.ready();
        n.watchdog();
        n.trigger();
        let mut buf = [0u8; 64];
        let k = srv.recv(&mut buf).unwrap();
        assert_eq!(&buf[..k], b"READY=1\n");
        let k = srv.recv(&mut buf).unwrap();
        assert_eq!(&buf[..k], b"WATCHDOG=1\n");
        let k = srv.recv(&mut buf).unwrap();
        assert_eq!(&buf[..k], b"WATCHDOG=trigger\n");
        assert_eq!(n.feed_interval(), Some(Duration::from_micros(3_333_333)));
    }
}
