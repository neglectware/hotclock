//! The mount: finished Ogg pages in a ring, and the listeners that follow it (architecture 1.3a).
//!
//! The encoder thread pushes pages; each listener (a thread owned by the HTTP server) holds a cursor
//! into the ring and takes pages in order. Nothing here waits for a listener: a page push takes the
//! lock for a few microseconds, a listener that falls too far behind is disconnected, and the ring
//! simply overwrites its oldest pages. With no listener, pages are discarded as the ring turns over.

use std::collections::{BTreeMap, VecDeque};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::*};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct MountConfig {
    /// The mount name: served at `/stream/<name>.ogg`.
    pub name: String,
    /// The station's display name, sent as `icy-name` and in the stream's comment.
    pub display_name: String,
    pub rate: u32,
    pub codec: &'static str,
    pub burst_ms: u32,
    pub max_listeners: u32,
    pub listener_lag_s: u32,
    pub expect_listener: bool,
}

impl MountConfig {
    /// Pages are kept this long: past the lag limit, so a lagging listener is seen to lag and is
    /// disconnected as `slow`, rather than silently finding its pages gone.
    fn retention_frames(&self) -> u64 {
        (u64::from(self.listener_lag_s.max(5)) + 3) * u64::from(self.rate)
    }
}

/// One Ogg page of the audio stream.
pub struct Page {
    pub seq: u64,
    pub bytes: Vec<u8>,
    /// The page's span on the stream's timeline, in frames (the granule position before and after).
    pub start_frame: u64,
    pub end_frame: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Fell more than `listener_lag_s` behind, or stopped reading for that long.
    Slow,
    /// The client closed the connection.
    Gone,
    /// An operator disconnected it (`POST /api/v1/stream/disconnect`).
    Kicked,
    /// The encoder started a new logical stream (new headers), so every listener reconnects. An ordinary
    /// encoder restart continues the stream and closes nobody.
    EncoderRestart,
    /// The station is stopping.
    Shutdown,
}

impl CloseReason {
    pub fn name(self) -> &'static str {
        match self {
            CloseReason::Slow => "slow",
            CloseReason::Gone => "gone",
            CloseReason::Kicked => "kicked",
            CloseReason::EncoderRestart => "encoder_restart",
            CloseReason::Shutdown => "shutdown",
        }
    }
}

/// Encoder lifecycle, for `/health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderState {
    Starting,
    Running,
    /// It failed and is starting over.
    Restarting,
    /// The station stopped it.
    Stopped,
    /// It cannot run: it could not start or restart (the thread has ended), or it is failing over and
    /// over. The stream is down.
    Failed,
}

impl EncoderState {
    pub fn name(self) -> &'static str {
        match self {
            EncoderState::Starting => "starting",
            EncoderState::Running => "running",
            EncoderState::Restarting => "restarting",
            EncoderState::Stopped => "stopped",
            EncoderState::Failed => "failed",
        }
    }
}

#[derive(Default)]
pub struct MountStats {
    pub connects: AtomicU64,
    pub rejected: AtomicU64,
    pub peak_listeners: AtomicU64,
    pub closed_slow: AtomicU64,
    pub closed_gone: AtomicU64,
    pub closed_kicked: AtomicU64,
    pub closed_restart: AtomicU64,
    pub bytes_sent: AtomicU64,
    /// Audio queued in the tap ring, in milliseconds (the encoder's lag behind rt-out).
    pub encoder_lag_ms: AtomicU64,
    pub encoder_lag_max_ms: AtomicU64,
    pub encoder_restarts: AtomicU64,
    /// Gaps the encoder filled with silence, and their total length in frames.
    pub gaps: AtomicU64,
    pub gap_frames: AtomicU64,
    /// Frames the encoder has put on the stream's timeline (audio and gap fill).
    pub frames_encoded: AtomicU64,
    pub pages: AtomicU64,
    pub bytes_encoded: AtomicU64,
    /// Time the encoder thread spent encoding, nanoseconds of thread CPU (wall time inside `encode`).
    pub encode_ns: AtomicU64,
    pub max_encode_ns: AtomicU64,
}

struct Entry {
    remote: String,
    since: Instant,
    bytes: u64,
    lag_ms: u64,
    kick: Arc<AtomicBool>,
    sock: Option<TcpStream>,
}

struct State {
    generation: u64,
    headers: Arc<Vec<u8>>,
    pages: VecDeque<Arc<Page>>,
    next_seq: u64,
    listeners: BTreeMap<u64, Entry>,
    next_listener: u64,
    stopped: bool,
}

pub struct Mount {
    pub cfg: MountConfig,
    pub stats: MountStats,
    state: Mutex<State>,
    cv: Condvar,
    started: Instant,
    /// Milliseconds since `started` at which a listener was last connected (or the mount began).
    last_listener_ms: AtomicU64,
    encoder_state: AtomicU8,
    /// Test hook: the encoder sleeps until this many ms after `started`.
    stall_until_ms: AtomicU64,
    /// Test hook: the encoder starts over.
    restart_requested: AtomicBool,
    /// Test hook: the encoder panics in the middle of encoding.
    panic_requested: AtomicBool,
    /// The tap ring's counters, attached when the encoder starts.
    tap: std::sync::OnceLock<Arc<engine::tap::TapStats>>,
}

/// A copy of the mount's live state, for the API.
#[derive(Debug, Clone)]
pub struct ListenerInfo {
    pub id: u64,
    pub remote: String,
    pub connected_s: f64,
    pub bytes: u64,
    pub lag_ms: u64,
}

impl Mount {
    pub fn new(cfg: MountConfig) -> Arc<Mount> {
        Arc::new(Mount {
            cfg,
            stats: MountStats::default(),
            state: Mutex::new(State {
                generation: 0,
                headers: Arc::new(Vec::new()),
                pages: VecDeque::new(),
                next_seq: 0,
                listeners: BTreeMap::new(),
                next_listener: 1,
                stopped: false,
            }),
            cv: Condvar::new(),
            started: Instant::now(),
            last_listener_ms: AtomicU64::new(0),
            encoder_state: AtomicU8::new(0),
            stall_until_ms: AtomicU64::new(0),
            restart_requested: AtomicBool::new(false),
            panic_requested: AtomicBool::new(false),
            tap: std::sync::OnceLock::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    // ---- the encoder's side ------------------------------------------------------------------------------

    pub fn attach_tap(&self, stats: Arc<engine::tap::TapStats>) {
        let _ = self.tap.set(stats);
    }

    pub fn tap_stats(&self) -> Option<&Arc<engine::tap::TapStats>> {
        self.tap.get()
    }

    pub fn uptime_s(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// A new logical stream: the header pages, and every listener is closed (it reconnects and gets
    /// the new headers).
    pub fn reset(&self, headers: Vec<u8>) {
        let mut s = self.lock();
        s.generation += 1;
        s.headers = Arc::new(headers);
        s.pages.clear();
        drop(s);
        self.cv.notify_all();
    }

    pub fn push_page(&self, bytes: Vec<u8>, start_frame: u64, end_frame: u64) {
        self.stats.pages.fetch_add(1, Relaxed);
        self.stats
            .bytes_encoded
            .fetch_add(bytes.len() as u64, Relaxed);
        let mut s = self.lock();
        let seq = s.next_seq;
        s.next_seq += 1;
        s.pages.push_back(Arc::new(Page {
            seq,
            bytes,
            start_frame,
            end_frame,
        }));
        let keep = self.cfg.retention_frames();
        while s.pages.len() > 1
            && s.pages
                .front()
                .is_some_and(|f| end_frame.saturating_sub(f.start_frame) > keep)
        {
            s.pages.pop_front();
        }
        drop(s);
        self.cv.notify_all();
    }

    pub fn set_encoder_state(&self, st: EncoderState) {
        self.encoder_state.store(st as u8, Release);
    }

    pub fn encoder_state(&self) -> EncoderState {
        match self.encoder_state.load(Acquire) {
            0 => EncoderState::Starting,
            1 => EncoderState::Running,
            2 => EncoderState::Restarting,
            3 => EncoderState::Stopped,
            _ => EncoderState::Failed,
        }
    }

    /// The station is stopping: close every listener and refuse new ones.
    pub fn shutdown(&self) {
        self.lock().stopped = true;
        self.encoder_state
            .store(EncoderState::Stopped as u8, Release);
        self.cv.notify_all();
    }

    // ---- test hooks --------------------------------------------------------------------------------------

    /// Make the encoder thread sleep for `ms` (acceptance 4a).
    pub fn hook_stall(&self, ms: u64) {
        self.stall_until_ms.store(self.now_ms() + ms, Release);
    }

    pub fn stalled(&self) -> bool {
        self.now_ms() < self.stall_until_ms.load(Acquire)
    }

    /// Make the encoder start over, as after a failure (acceptance 4d/5): the stream continues with a short
    /// gap of silence, listeners stay connected.
    pub fn hook_restart(&self) {
        self.restart_requested.store(true, Release);
    }

    pub fn take_restart_request(&self) -> bool {
        self.restart_requested.swap(false, AcqRel)
    }

    /// Make the encoder panic while it encodes (a real panic, caught by its thread).
    pub fn hook_panic(&self) {
        self.panic_requested.store(true, Release);
    }

    pub fn take_panic_request(&self) -> bool {
        self.panic_requested.load(Relaxed) && self.panic_requested.swap(false, AcqRel)
    }

    // ---- the listeners' side -----------------------------------------------------------------------------

    /// Admit a listener, or `None` when `max_listeners` are connected (or the mount is stopping). It
    /// starts `burst_ms` behind the newest page, at a page boundary.
    pub fn join(self: &Arc<Self>, remote: &str, sock: Option<TcpStream>) -> Option<Listener> {
        let mut s = self.lock();
        if s.stopped || s.listeners.len() >= self.cfg.max_listeners as usize {
            drop(s);
            self.stats.rejected.fetch_add(1, Relaxed);
            return None;
        }
        let id = s.next_listener;
        s.next_listener += 1;
        let kick = Arc::new(AtomicBool::new(false));
        s.listeners.insert(
            id,
            Entry {
                remote: remote.to_string(),
                since: Instant::now(),
                bytes: 0,
                lag_ms: 0,
                kick: kick.clone(),
                sock,
            },
        );
        let burst = u64::from(self.cfg.burst_ms) * u64::from(self.cfg.rate) / 1000;
        let cursor = match s.pages.back() {
            None => s.next_seq,
            Some(newest) => {
                let from = newest.end_frame.saturating_sub(burst);
                s.pages
                    .iter()
                    .find(|p| p.start_frame >= from)
                    .map_or(newest.seq, |p| p.seq)
            }
        };
        let n = s.listeners.len() as u64;
        let generation = s.generation;
        let headers = s.headers.clone();
        drop(s);
        self.stats.connects.fetch_add(1, Relaxed);
        self.stats.peak_listeners.fetch_max(n, Relaxed);
        self.last_listener_ms.store(self.now_ms(), Relaxed);
        Some(Listener {
            mount: self.clone(),
            id,
            cursor,
            generation,
            headers,
            kick,
            reason: None,
        })
    }

    /// Hand the listener's socket to the mount so a disconnect can shut it down.
    pub fn attach_socket(&self, id: u64, sock: TcpStream) {
        if let Some(e) = self.lock().listeners.get_mut(&id) {
            e.sock = Some(sock);
        }
    }

    pub fn listener_count(&self) -> usize {
        self.lock().listeners.len()
    }

    pub fn listeners(&self) -> Vec<ListenerInfo> {
        self.lock()
            .listeners
            .iter()
            .map(|(id, e)| ListenerInfo {
                id: *id,
                remote: e.remote.clone(),
                connected_s: e.since.elapsed().as_secs_f64(),
                bytes: e.bytes,
                lag_ms: e.lag_ms,
            })
            .collect()
    }

    /// Disconnect one listener (`Some(id)`) or all of them. Returns how many were asked to go.
    pub fn disconnect(&self, id: Option<u64>) -> usize {
        let s = self.lock();
        let mut n = 0;
        for (lid, e) in &s.listeners {
            if id.is_none_or(|i| i == *lid) {
                e.kick.store(true, Release);
                if let Some(sock) = &e.sock {
                    // A listener blocked in a write to a client that stopped reading returns at once.
                    let _ = sock.shutdown(Shutdown::Both);
                }
                n += 1;
            }
        }
        drop(s);
        self.cv.notify_all();
        n
    }

    /// Seconds with nobody listening (0 while anybody is).
    pub fn no_listener_s(&self) -> f64 {
        if self.listener_count() > 0 {
            self.last_listener_ms.store(self.now_ms(), Relaxed);
            return 0.0;
        }
        (self
            .now_ms()
            .saturating_sub(self.last_listener_ms.load(Relaxed))) as f64
            / 1000.0
    }

    /// Audio the newest page is ahead of the oldest still kept, for the API.
    pub fn buffered_ms(&self) -> u64 {
        let s = self.lock();
        match (s.pages.front(), s.pages.back()) {
            (Some(a), Some(b)) => (b.end_frame - a.start_frame) * 1000 / u64::from(self.cfg.rate),
            _ => 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.lock().generation
    }
}

/// What a listener's `next` found.
pub enum Next {
    Page(Arc<Page>),
    /// Nothing new within the wait.
    Idle,
    Closed(CloseReason),
}

/// One connected listener's cursor. Dropping it removes it from the mount.
pub struct Listener {
    mount: Arc<Mount>,
    pub id: u64,
    cursor: u64,
    generation: u64,
    headers: Arc<Vec<u8>>,
    kick: Arc<AtomicBool>,
    reason: Option<CloseReason>,
}

impl Listener {
    /// The header pages, to send first.
    pub fn headers(&self) -> Arc<Vec<u8>> {
        self.headers.clone()
    }

    /// The next page, waiting up to `wait` for one.
    pub fn next(&mut self, wait: Duration) -> Next {
        let m = self.mount.clone();
        let deadline = Instant::now() + wait;
        let mut s = m.lock();
        loop {
            if self.kick.load(Acquire) {
                return self.close(CloseReason::Kicked);
            }
            if s.stopped {
                return self.close(CloseReason::Shutdown);
            }
            if s.generation != self.generation {
                return self.close(CloseReason::EncoderRestart);
            }
            if let Some(first) = s.pages.front() {
                if self.cursor < first.seq {
                    // The ring turned over past this listener.
                    return self.close(CloseReason::Slow);
                }
            }
            let idx = s.pages.front().map(|f| self.cursor.saturating_sub(f.seq));
            if let (Some(i), Some(newest)) = (idx, s.pages.back().cloned()) {
                if let Some(p) = s.pages.get(i as usize).cloned() {
                    if p.seq == self.cursor {
                        let lag = newest.end_frame.saturating_sub(p.end_frame);
                        let lag_ms = lag * 1000 / u64::from(m.cfg.rate);
                        if let Some(e) = s.listeners.get_mut(&self.id) {
                            e.lag_ms = lag_ms;
                        }
                        if lag_ms > u64::from(m.cfg.listener_lag_s) * 1000 {
                            return self.close(CloseReason::Slow);
                        }
                        self.cursor += 1;
                        return Next::Page(p);
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Next::Idle;
            }
            s =
                m.cv.wait_timeout(s, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
        }
    }

    fn close(&mut self, why: CloseReason) -> Next {
        self.reason = Some(why);
        Next::Closed(why)
    }

    /// Account bytes written to this client.
    pub fn sent(&self, n: usize) {
        self.mount.stats.bytes_sent.fetch_add(n as u64, Relaxed);
        if let Some(e) = self.mount.lock().listeners.get_mut(&self.id) {
            e.bytes += n as u64;
        }
    }

    /// Record why the connection ended when it was not `next` that said so (a failed write).
    pub fn end(&mut self, why: CloseReason) {
        self.reason.get_or_insert(why);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let reason = self.reason.unwrap_or(CloseReason::Gone);
        let mut s = self.mount.lock();
        s.listeners.remove(&self.id);
        let empty = s.listeners.is_empty();
        drop(s);
        let st = &self.mount.stats;
        match reason {
            CloseReason::Slow => st.closed_slow.fetch_add(1, Relaxed),
            CloseReason::Gone | CloseReason::Shutdown => st.closed_gone.fetch_add(1, Relaxed),
            CloseReason::Kicked => st.closed_kicked.fetch_add(1, Relaxed),
            CloseReason::EncoderRestart => st.closed_restart.fetch_add(1, Relaxed),
        };
        if empty {
            self.mount
                .last_listener_ms
                .store(self.mount.now_ms(), Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MountConfig {
        MountConfig {
            name: "program".into(),
            display_name: "Test".into(),
            rate: 48_000,
            codec: "flac",
            burst_ms: 1000,
            max_listeners: 2,
            listener_lag_s: 5,
            expect_listener: true,
        }
    }

    fn feed(m: &Mount, n: u64) {
        // 4,096-frame pages (85 ms).
        let base = m.stats.pages.load(Relaxed) * 4096;
        for i in 0..n {
            m.push_page(vec![0; 10], base + i * 4096, base + (i + 1) * 4096);
        }
    }

    #[test]
    fn a_listener_starts_about_a_burst_behind_and_follows() {
        let m = Mount::new(cfg());
        m.reset(vec![1, 2, 3]);
        feed(&m, 40); // 3.4 s
        let mut l = m.join("a", None).unwrap();
        let mut got = 0;
        while let Next::Page(_) = l.next(Duration::ZERO) {
            got += 1;
        }
        // 1 s is 11.7 pages: 12 pages from the first page that starts within the burst window.
        assert!((11..=13).contains(&got), "{got}");
        feed(&m, 1);
        assert!(matches!(l.next(Duration::ZERO), Next::Page(_)));
        assert!(matches!(l.next(Duration::ZERO), Next::Idle));
    }

    #[test]
    fn the_third_listener_is_refused_and_counted() {
        let m = Mount::new(cfg());
        m.reset(vec![]);
        let _a = m.join("a", None).unwrap();
        let _b = m.join("b", None).unwrap();
        assert!(m.join("c", None).is_none());
        assert_eq!(m.stats.rejected.load(Relaxed), 1);
        assert_eq!(m.listener_count(), 2);
        drop(_a);
        assert!(m.join("c", None).is_some());
    }

    #[test]
    fn a_listener_that_lags_past_the_limit_is_closed_slow() {
        let m = Mount::new(cfg());
        m.reset(vec![]);
        feed(&m, 5);
        let mut l = m.join("a", None).unwrap();
        feed(&m, 80); // 6.8 s of audio while it reads nothing
        match l.next(Duration::ZERO) {
            Next::Closed(CloseReason::Slow) => {}
            _ => panic!("expected slow"),
        }
        drop(l);
        assert_eq!(m.stats.closed_slow.load(Relaxed), 1);
        assert_eq!(m.listener_count(), 0);
    }

    #[test]
    fn a_restart_closes_listeners_and_a_kick_closes_one() {
        let m = Mount::new(cfg());
        m.reset(vec![]);
        feed(&m, 3);
        let mut a = m.join("a", None).unwrap();
        let mut b = m.join("b", None).unwrap();
        assert_eq!(m.disconnect(Some(b.id)), 1);
        assert!(matches!(
            b.next(Duration::ZERO),
            Next::Closed(CloseReason::Kicked)
        ));
        m.reset(vec![9]);
        assert!(matches!(
            a.next(Duration::ZERO),
            Next::Closed(CloseReason::EncoderRestart)
        ));
        drop(b);
        let c = m.join("c", None).unwrap();
        assert_eq!(*c.headers(), vec![9]);
    }

    #[test]
    fn a_ring_that_turns_past_a_listener_closes_it_slow_and_a_new_stream_starts_its_cursor_right() {
        let m = Mount::new(cfg());
        m.reset(vec![]);
        feed(&m, 2);
        let mut l = m.join("a", None).unwrap();
        // 17 s of pages: the ring (8 s for listener_lag_s = 5) drops the listener's next page.
        feed(&m, 200);
        assert!(matches!(
            l.next(Duration::ZERO),
            Next::Closed(CloseReason::Slow)
        ));
        drop(l);
        // A new logical stream: the ring empties but sequence numbers go on; a listener that joins before
        // its first page gets that page first, and nothing older.
        m.reset(vec![7]);
        let mut n = m.join("b", None).unwrap();
        assert!(matches!(n.next(Duration::ZERO), Next::Idle));
        m.push_page(vec![1], 0, 4096);
        match n.next(Duration::ZERO) {
            Next::Page(p) => assert_eq!((p.start_frame, p.bytes[0]), (0, 1)),
            _ => panic!("expected the first page of the new stream"),
        }
        assert!(matches!(n.next(Duration::ZERO), Next::Idle));
    }

    #[test]
    fn no_listener_time_counts_from_the_last_disconnect() {
        let m = Mount::new(cfg());
        std::thread::sleep(Duration::from_millis(30));
        assert!(m.no_listener_s() > 0.02);
        let l = m.join("a", None).unwrap();
        assert_eq!(m.no_listener_s(), 0.0);
        drop(l);
        assert!(m.no_listener_s() < 0.02);
    }
}
