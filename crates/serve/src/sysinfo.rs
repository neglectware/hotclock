//! What the top bar of the box's screen shows besides the clock (`split`, 11.2b): the whole box's CPU use from
//! `/proc/stat`, whether the box has a way out to the network, and the version in short. Each is a small file read
//! on request; nothing runs in the background.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The CPU use is the average over about this long, so the number does not flicker.
pub const CPU_WINDOW: Duration = Duration::from_secs(5);
/// Samples kept at most, however often the bar is asked for (several screens open at once).
const CPU_SAMPLES: usize = 64;

/// The all-CPU line of `/proc/stat`, in clock ticks since boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    /// Ticks spent on anything but idle and waiting for I/O.
    pub busy: u64,
    /// All ticks.
    pub total: u64,
}

/// The first `cpu ` line of `/proc/stat`: user nice system idle iowait irq softirq steal (guest and guest_nice are
/// already counted in user and nice, so they are left out). None when the line is missing or short.
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|f| f.parse().ok())
        .collect::<Option<_>>()?;
    if v.len() < 4 {
        return None;
    }
    let total: u64 = v.iter().fold(0u64, |a, &x| a.saturating_add(x));
    let idle = v[3].saturating_add(v.get(4).copied().unwrap_or(0));
    Some(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
    })
}

/// Whole-box CPU use between two samples, a whole percent. None when no tick passed, or when a counter went back
/// (a wrap, a reset, a CPU taken offline): there is no honest number for that interval.
pub fn cpu_percent(prev: CpuTimes, now: CpuTimes) -> Option<u8> {
    let dt = now.total.checked_sub(prev.total)?;
    let db = now.busy.checked_sub(prev.busy)?;
    if dt == 0 || db > dt {
        return None;
    }
    Some(((db as f64 * 100.0 / dt as f64).round() as u8).min(100))
}

/// The CPU use averaged over [`CPU_WINDOW`]: each request adds a sample and compares it with the oldest one still
/// needed. Primed when made, so the first request already has a number.
pub struct CpuSampler {
    path: PathBuf,
    samples: Mutex<VecDeque<(Instant, CpuTimes)>>,
}

impl CpuSampler {
    pub fn new(path: impl Into<PathBuf>) -> CpuSampler {
        let s = CpuSampler {
            path: path.into(),
            samples: Mutex::new(VecDeque::new()),
        };
        s.add(Instant::now(), s.read());
        s
    }

    fn read(&self) -> Option<CpuTimes> {
        parse_proc_stat(&std::fs::read_to_string(&self.path).ok()?)
    }

    /// The average now, or None without `/proc/stat` (or before a usable second sample).
    pub fn percent(&self) -> Option<u8> {
        self.add(Instant::now(), self.read())
    }

    /// Add the sample `now` taken at `t`, and answer the use since the oldest sample of the window.
    pub fn add(&self, t: Instant, now: Option<CpuTimes>) -> Option<u8> {
        let now = now?;
        let mut q = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back((t, now));
        // keep one sample at least as old as the window as the base; drop the ones before it
        while q.len() > 2 && t.saturating_duration_since(q[1].0) >= CPU_WINDOW {
            q.pop_front();
        }
        while q.len() > CPU_SAMPLES {
            q.pop_front();
        }
        if q.len() < 2 {
            return None;
        }
        let r = cpu_percent(q[0].1, now);
        if r.is_none() && q[0].1 != now {
            // a counter went back: start again from this sample
            q.clear();
            q.push_back((t, now));
        }
        r
    }
}

/// Whether the box has a way out: a default route (IPv4 `/proc/net/route` or IPv6 `/proc/net/ipv6_route`) that is
/// up, through an interface that is not down (`/sys/class/net/<if>/operstate`). None when neither route table can
/// be read.
pub fn network_up(proc_net: &Path, sys_class_net: &Path) -> Option<bool> {
    let v4 = std::fs::read_to_string(proc_net.join("route")).ok();
    let v6 = std::fs::read_to_string(proc_net.join("ipv6_route")).ok();
    if v4.is_none() && v6.is_none() {
        return None;
    }
    let mut ifaces = default_route_ifaces(v4.as_deref().unwrap_or(""), v6.as_deref().unwrap_or(""));
    ifaces.dedup();
    Some(ifaces.iter().any(|i| {
        std::fs::read_to_string(sys_class_net.join(i).join("operstate"))
            .map_or(true, |s| s.trim() != "down")
    }))
}

/// The interfaces of the default routes that are up, from the two route tables' text.
pub fn default_route_ifaces(v4: &str, v6: &str) -> Vec<String> {
    const RTF_UP: u32 = 0x1;
    let mut out = vec![];
    // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
    for l in v4.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 8
            && f[1] == "00000000"
            && f[7] == "00000000"
            && u32::from_str_radix(f[3], 16).is_ok_and(|x| x & RTF_UP != 0)
        {
            out.push(f[0].to_string());
        }
    }
    // dest prefix src prefix nexthop metric refcnt use flags iface
    for l in v6.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 10
            && f[0].bytes().all(|b| b == b'0')
            && f[1] == "00"
            && f[9] != "lo"
            && u32::from_str_radix(f[8], 16).is_ok_and(|x| x & RTF_UP != 0)
        {
            out.push(f[9].to_string());
        }
    }
    out
}

/// The version for the screen: the crate version, with the package revision when the build has one.
/// `0.1.0+build.25.g9641eb2` is `0.1.0-25` (the Debian version); `0.1.0+build.dev` is `0.1.0`.
pub fn short_version(v: &str) -> String {
    let (base, build) = v.split_once("+build.").unwrap_or((v, ""));
    let rev = build.split('.').next().unwrap_or("");
    if !rev.is_empty() && rev.bytes().all(|b| b.is_ascii_digit()) {
        format!("{base}-{rev}")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(user: u64, system: u64, idle: u64, iowait: u64) -> String {
        format!("cpu  {user} 0 {system} {idle} {iowait} 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\nintr 1 2\n")
    }

    #[test]
    fn the_cpu_line_is_parsed_without_guest_time() {
        let t = parse_proc_stat("cpu  100 20 30 800 50 5 5 0 40 0\ncpu0 1 1 1 1 1 1 1 1 1 1\n")
            .unwrap();
        // guest (40) is already part of user: not counted twice
        assert_eq!(
            t,
            CpuTimes {
                busy: 160,
                total: 1010
            }
        );
        // an old kernel with four fields
        assert_eq!(
            parse_proc_stat("cpu 1 2 3 4\n"),
            Some(CpuTimes { busy: 6, total: 10 })
        );
        assert_eq!(parse_proc_stat("cpu0 1 2 3 4\n"), None);
        assert_eq!(parse_proc_stat("cpu 1 2\n"), None);
        assert_eq!(parse_proc_stat("cpu 1 x 3 4\n"), None);
        assert_eq!(parse_proc_stat(""), None);
    }

    #[test]
    fn two_snapshots_give_the_whole_box_percent() {
        let a = parse_proc_stat(&stat(1000, 500, 10_000, 200)).unwrap();
        // 30 + 10 busy, 155 idle, 5 iowait: 40 of 200 ticks
        let b = parse_proc_stat(&stat(1030, 510, 10_155, 205)).unwrap();
        assert_eq!(cpu_percent(a, b), Some(20));
        // rounding: 1 of 3 ticks is 33 %, 2 of 3 is 67 %
        let c = parse_proc_stat(&stat(1001, 500, 10_002, 200)).unwrap();
        assert_eq!(cpu_percent(a, c), Some(33));
        let d = parse_proc_stat(&stat(1002, 500, 10_001, 200)).unwrap();
        assert_eq!(cpu_percent(a, d), Some(67));
        // all busy, all idle
        let e = parse_proc_stat(&stat(1100, 500, 10_000, 200)).unwrap();
        assert_eq!(cpu_percent(a, e), Some(100));
        let f = parse_proc_stat(&stat(1000, 500, 10_100, 200)).unwrap();
        assert_eq!(cpu_percent(a, f), Some(0));
    }

    #[test]
    fn no_change_and_a_counter_going_back_give_no_number() {
        let a = parse_proc_stat(&stat(1000, 500, 10_000, 200)).unwrap();
        assert_eq!(cpu_percent(a, a), None);
        // a wrap or reset: the counters are smaller than before
        let w = parse_proc_stat(&stat(3, 1, 50, 0)).unwrap();
        assert_eq!(cpu_percent(a, w), None);
        // only idle went back (a CPU taken offline): busy grew more than the total
        let o = parse_proc_stat(&stat(1050, 500, 9_990, 200)).unwrap();
        assert_eq!(cpu_percent(a, o), None);
        // near the top of u64: no overflow in the sum
        assert!(parse_proc_stat(&format!("cpu {} {} 1 1\n", u64::MAX, u64::MAX)).is_some());
    }

    #[test]
    fn the_sampler_averages_over_the_window_and_starts_again_after_a_wrap() {
        let s = CpuSampler {
            path: PathBuf::from("/nonexistent/stat"),
            samples: Mutex::new(VecDeque::new()),
        };
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let ct = |busy, total| Some(CpuTimes { busy, total });
        assert_eq!(s.add(at(0), ct(0, 0)), None);
        // 1 s later: 50 % since the start
        assert_eq!(s.add(at(1), ct(50, 100)), Some(50));
        assert_eq!(s.add(at(2), ct(50, 200)), Some(25));
        assert_eq!(s.add(at(5), ct(50, 500)), Some(10));
        // at 7 s the base is the sample of 2 s (the newest one at least 5 s old): 250 busy of 500 since then
        assert_eq!(s.add(at(7), ct(300, 700)), Some(50));
        // no tick since the last sample: still the average since the base
        assert_eq!(s.add(at(7), ct(300, 700)), Some(50));
        // the counters went back: no number now, and the next sample counts from there
        assert_eq!(s.add(at(8), ct(10, 20)), None);
        assert_eq!(s.add(at(9), ct(30, 120)), Some(20));
        // no /proc/stat: no number, and the samples are kept
        assert_eq!(s.add(at(10), None), None);
        assert_eq!(s.add(at(11), ct(40, 220)), Some(15));
        // the real file is absent here, so a sampler on it answers None
        assert_eq!(s.percent(), None);
    }

    #[test]
    fn a_sampler_on_the_real_proc_stat_answers_a_percent() {
        if !Path::new("/proc/stat").exists() {
            return;
        }
        let s = CpuSampler::new("/proc/stat");
        std::thread::sleep(Duration::from_millis(50));
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        assert!(x > 0);
        if let Some(p) = s.percent() {
            assert!(p <= 100);
        }
    }

    #[test]
    fn the_default_routes_are_found_in_both_tables() {
        let v4 =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                  eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                  eth0\t0002A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                  wlan0\t00000000\t0102A8C0\t0002\t0\t0\t600\t00000000\t0\t0\t0\n";
        assert_eq!(default_route_ifaces(v4, ""), ["eth0"]);
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003 eth1\n\
                  00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
                  fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth1\n";
        assert_eq!(default_route_ifaces("", v6), ["eth1"]);
        assert!(default_route_ifaces("Iface\tDestination\n", "").is_empty());
    }

    #[test]
    fn the_network_is_up_with_a_default_route_on_an_interface_that_is_not_down() {
        let d = std::env::temp_dir().join(format!("hotclock-net-{}", std::process::id()));
        let (proc_net, sys) = (d.join("net"), d.join("class"));
        std::fs::create_dir_all(sys.join("eth0")).unwrap();
        std::fs::create_dir_all(&proc_net).unwrap();
        // no route table at all: unknown
        assert_eq!(network_up(&proc_net, &sys), None);
        let route = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
                     eth0\t00000000\t0102A8C0\t0003\t0\t0\t100\t00000000\n";
        std::fs::write(proc_net.join("route"), route).unwrap();
        std::fs::write(sys.join("eth0/operstate"), "up\n").unwrap();
        assert_eq!(network_up(&proc_net, &sys), Some(true));
        std::fs::write(sys.join("eth0/operstate"), "down\n").unwrap();
        assert_eq!(network_up(&proc_net, &sys), Some(false));
        // no default route
        std::fs::write(proc_net.join("route"), "Iface\tDestination\n").unwrap();
        assert_eq!(network_up(&proc_net, &sys), Some(false));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_short_version_carries_the_package_revision() {
        assert_eq!(short_version("0.1.0+build.25.g9641eb2"), "0.1.0-25");
        assert_eq!(short_version("0.1.0+build.dev"), "0.1.0");
        assert_eq!(short_version("0.1.0"), "0.1.0");
        assert_eq!(short_version("1.2.3+build.x7.gabc"), "1.2.3");
    }
}
