//! The `rt-out` thread (architecture 1.2 and 1.5).
//!
//! **Rules for the code in this file that runs on the rt-out thread** (`rt_main` and what it calls):
//! no allocation or freeing, no locks, no logging, no file or database access. It communicates
//! through `rtrb` rings and atomics. Everything it needs (the render block, the fade scratch block,
//! the silence block, the pending-command list) is allocated by [`RtHandle::start`] before the
//! thread runs and moved in.
//!
//! Faults never duplicate or skip source audio: frames the device took before a failing write
//! (`OutputError::frames_done`) are not sent again, and the rest of the block is. After the
//! silence that follows an xrun or a suspend, audio fades in over `xrun_fade_ms`; after a device
//! loss, over `resume_fade_ms`. Fades are applied as a block is written, never to the rendered
//! block, so a block sent again is not faded twice.
//! The engine tests run it under an allocator that aborts on any allocation from this thread
//! (`tests/rt_no_alloc.rs`).
//!
//! Device loss is the one place the thread does something unusual. The `Output` is moved, by value
//! through a ring, to a **recovery thread** that retries `reopen` every 2 s (this is the only
//! code that may allocate or make syscalls while the device is absent). The rt thread waits for the
//! output to come back by polling the ring with short sleeps. The station clock (`frames_written`)
//! stands still while the device is gone.

use crate::fault::Fault;
use crate::mixer::MixerCmd;
use crate::output::Output;
use crate::source::Source;
use crate::sys::{self, Priority};
use common::Clock;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{
    fence, AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering::*,
};
use std::sync::Arc;
use std::thread::JoinHandle;

pub const COMMAND_RING: usize = 256;
pub const EVENT_RING: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommandKind {
    /// Linear gain of the source (ramped across one block).
    SetGain(f32),
    SetToneHz(f32),
    /// Test hook: sleep the rt-out thread for this long, to provoke an xrun (milestone M1.1
    /// acceptance 4). Never sent in production.
    Stall {
        ms: u32,
    },
    /// A mixer command (M1.2). Not held until `at_frame`: the mixer applies it as soon as it
    /// is popped and keeps the absolute frames inside the command, so the timing is exact.
    Mixer(MixerCmd),
}

/// A command with an **absolute start frame**. The M1.1 kinds execute at the first block
/// boundary at or after `at_frame` (block-granular); mixer commands carry their own frames and
/// are sample-exact (`CommandKind::Mixer`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Command {
    pub at_frame: u64,
    pub kind: CommandKind,
}

impl Command {
    pub fn now(kind: CommandKind) -> Self {
        Self { at_frame: 0, kind }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    Started,
    Stopped,
    /// An underrun was recovered (prepare, one buffer of silence, then the audio fades in).
    XrunRecovered,
    /// The device suspended and was resumed.
    Resumed,
    /// The device disappeared; the station clock is paused.
    DeviceLost,
    /// The device came back after this many milliseconds.
    DeviceRestored {
        outage_ms: u32,
    },
    /// A fault that could not be recovered in place was handled by treating the device as lost.
    Fault(Fault),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// `frames_written` when the event was posted.
    pub frame: u64,
    pub kind: EventKind,
}

/// Lifecycle of the thread, for `Stats::state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtState {
    Starting,
    Playing,
    DeviceLost,
    Stopped,
}

impl RtState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => RtState::Starting,
            1 => RtState::Playing,
            2 => RtState::DeviceLost,
            _ => RtState::Stopped,
        }
    }
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    state: AtomicU8,
    /// Seqlock over the position fields below (odd while the rt thread is writing them), so a
    /// reader gets `frames_written`, `delay_frames` and `position_ns` from the same block. Read
    /// separately they can come from different blocks, which moves the audible frame by a period.
    pos_seq: AtomicU64,
    frames_written: AtomicU64,
    source_frames: AtomicU64,
    delay_frames: AtomicI64,
    position_ns: AtomicU64,
    xruns: AtomicU32,
    suspends: AtomicU32,
    device_lost: AtomicU32,
    device_restored: AtomicU32,
    event_drops: AtomicU32,
    command_drops: AtomicU32,
    outage_ms_total: AtomicU64,
    max_write_ns: AtomicU64,
}

/// A copy of the counters at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub state: RtState,
    /// Frames handed to the device since start, silence included. Leads the audible position by
    /// the device delay.
    pub frames_written: u64,
    /// Frames the source has rendered. Differs from `frames_written` by the silence written after
    /// xruns; a source's position never advances during a gap.
    pub source_frames: u64,
    /// Frames queued in the device, not yet audible.
    pub delay_frames: i64,
    /// `Clock::monotonic_ns` when `delay_frames` was measured (just after the write of the
    /// block that brought `frames_written` to its value).
    pub position_ns: u64,
    pub xruns: u32,
    pub suspends: u32,
    pub device_lost: u32,
    pub device_restored: u32,
    pub event_drops: u32,
    pub command_drops: u32,
    pub outage_ms_total: u64,
    /// The longest single `write` call, in nanoseconds (a block-time watchdog).
    pub max_write_ns: u64,
}

impl Stats {
    /// The audible frame at `position_ns`: `frames_written - delay` (architecture 1.3). The
    /// station clock of 1.6 anchors to this pair, `(audible_frames, position_ns)`.
    pub fn audible_frames(&self) -> i64 {
        self.frames_written as i64 - self.delay_frames
    }

    /// The audible frame extrapolated to `now_ns` at `rate` (valid while `Playing`: the published
    /// value is up to one period old when read).
    pub fn audible_frames_at(&self, now_ns: u64, rate: u32) -> i64 {
        let dt = now_ns.saturating_sub(self.position_ns) as i128;
        self.audible_frames() + (dt * i128::from(rate) / 1_000_000_000) as i64
    }
}

pub struct RtOptions {
    /// Stop by itself after this many source frames, writing a short final block if needed.
    pub max_frames: Option<u64>,
    /// Called first thing on the rt thread. Tests use it to mark the thread for the allocator.
    /// An `Arc<dyn Fn>` rather than a `Box<dyn FnOnce>` so that calling it frees nothing.
    pub on_start: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Called last on the rt thread, before it returns its resources to the joiner.
    pub on_exit: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Ask for `SCHED_FIFO` (Linux). Off for offline rendering and most tests.
    pub realtime_priority: Option<u8>,
    /// Retry interval for reopening a lost device.
    pub recovery_retry_ns: u64,
    /// Fade-in after the device comes back (a click guard).
    pub resume_fade_ms: u32,
    /// Fade-in of the audio that follows the silence after an xrun or a resume from suspend
    /// (a declick: the silence ends in a step otherwise).
    pub xrun_fade_ms: u32,
    pub thread_name: String,
    /// The stream tap (1.3a): every buffer written to the device is also pushed here, wait-free.
    pub stream_tap: Option<crate::tap::TapProducer>,
}

impl Default for RtOptions {
    fn default() -> Self {
        Self {
            max_frames: None,
            on_start: None,
            on_exit: None,
            realtime_priority: None,
            recovery_retry_ns: 2_000_000_000,
            resume_fade_ms: 150,
            xrun_fade_ms: 10,
            thread_name: "rt-out".into(),
            stream_tap: None,
        }
    }
}

struct Recovered {
    out: Box<dyn Output>,
    reopened: bool,
}

pub struct RtHandle {
    shared: Arc<Shared>,
    commands: Producer<Command>,
    events: Consumer<Event>,
    join: Option<JoinHandle<RtContext>>,
    recovery_join: Option<JoinHandle<()>>,
    priority: Arc<std::sync::Mutex<Option<Priority>>>,
}

impl RtHandle {
    /// Start the recovery thread and the rt-out thread. Returns once the rt thread is running.
    pub fn start(
        out: Box<dyn Output>,
        source: Box<dyn Source>,
        clock: Arc<dyn Clock>,
        opts: RtOptions,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let granted = out.granted().clone();
        let period = granted.period_frames as usize;
        let channels = granted.channels as usize;
        assert_eq!(channels, 2, "the engine is stereo");

        // Everything the rt thread will ever need, allocated here.
        let block = vec![0.0f32; period * channels];
        let ramped = vec![0.0f32; period * channels];
        let silence = vec![0.0f32; period * channels];
        let pending: Vec<Command> = Vec::with_capacity(COMMAND_RING);
        let (cmd_tx, cmd_rx) = RingBuffer::<Command>::new(COMMAND_RING);
        let (ev_tx, ev_rx) = RingBuffer::<Event>::new(EVENT_RING);
        let (to_rec_tx, to_rec_rx) = RingBuffer::<Box<dyn Output>>::new(1);
        let (from_rec_tx, from_rec_rx) = RingBuffer::<Recovered>::new(1);
        let priority_slot = Arc::new(std::sync::Mutex::new(None));

        let rec_shared = shared.clone();
        let rec_clock = clock.clone();
        let retry_ns = opts.recovery_retry_ns;
        let recovery_join = std::thread::Builder::new()
            .name("device-recovery".into())
            .spawn(move || {
                recovery_thread(rec_clock, retry_ns, rec_shared, to_rec_rx, from_rec_tx)
            })?;
        let rec_thread = recovery_join.thread().clone();

        let rt_shared = shared.clone();
        let slot = priority_slot.clone();
        let name = opts.thread_name.clone();
        let join = std::thread::Builder::new().name(name).spawn(move || {
            let ctx = RtContext {
                out,
                source,
                clock,
                shared: rt_shared,
                cmds: cmd_rx,
                events: ev_tx,
                to_rec: to_rec_tx,
                from_rec: from_rec_rx,
                rec_thread,
                block,
                ramped,
                silence,
                pending,
                opts,
                priority_slot: slot,
            };
            rt_main(ctx)
        })?;

        // Wait for the thread to come up (bounded).
        let t0 = std::time::Instant::now();
        while RtState::from_u8(shared.state.load(Acquire)) == RtState::Starting
            && t0.elapsed() < std::time::Duration::from_secs(5)
            && !join.is_finished()
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        Ok(Self {
            shared,
            commands: cmd_tx,
            events: ev_rx,
            join: Some(join),
            recovery_join: Some(recovery_join),
            priority: priority_slot,
        })
    }

    /// Queue a command. Returns false if the ring is full.
    pub fn send(&mut self, cmd: Command) -> bool {
        self.commands.push(cmd).is_ok()
    }

    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop().ok()
    }

    pub fn stats(&self) -> Stats {
        let s = &self.shared;
        // Seqlock read: retry while the rt thread is between its two sequence stores. It never
        // waits for us.
        let (frames_written, source_frames, delay_frames, position_ns) = loop {
            let s1 = s.pos_seq.load(Acquire);
            if s1 & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let v = (
                s.frames_written.load(Relaxed),
                s.source_frames.load(Relaxed),
                s.delay_frames.load(Relaxed),
                s.position_ns.load(Relaxed),
            );
            fence(Acquire);
            if s.pos_seq.load(Relaxed) == s1 {
                break v;
            }
        };
        Stats {
            state: RtState::from_u8(s.state.load(Acquire)),
            frames_written,
            source_frames,
            delay_frames,
            position_ns,
            xruns: s.xruns.load(Acquire),
            suspends: s.suspends.load(Acquire),
            device_lost: s.device_lost.load(Acquire),
            device_restored: s.device_restored.load(Acquire),
            event_drops: s.event_drops.load(Acquire),
            command_drops: s.command_drops.load(Acquire),
            outage_ms_total: s.outage_ms_total.load(Acquire),
            max_write_ns: s.max_write_ns.load(Acquire),
        }
    }

    /// The scheduling priority the rt thread obtained (known once it has started).
    pub fn priority(&self) -> Option<Priority> {
        self.priority.lock().ok().and_then(|g| g.clone())
    }

    pub fn request_stop(&self) {
        self.shared.stop.store(true, Release);
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(|j| j.is_finished())
    }

    /// Stop, wait for the threads and return the output (the caller calls `finish` on it).
    pub fn stop(mut self) -> Box<dyn Output> {
        self.request_stop();
        self.join_inner()
    }

    /// Wait for a thread that stops by itself (`max_frames`).
    pub fn wait(mut self) -> Box<dyn Output> {
        self.join_inner()
    }

    fn join_inner(&mut self) -> Box<dyn Output> {
        let ctx = self
            .join
            .take()
            .expect("already joined")
            .join()
            .expect("rt-out thread panicked");
        let RtContext { out, .. } = ctx;
        self.shared.state.store(3, Release);
        if let Some(r) = self.recovery_join.take() {
            r.thread().unpark();
            let _ = r.join();
        }
        out
    }
}

impl Drop for RtHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.request_stop();
            let _ = self.join_inner();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The recovery thread (not real-time; allowed to allocate and log)

fn recovery_thread(
    clock: Arc<dyn Clock>,
    retry_ns: u64,
    shared: Arc<Shared>,
    mut from_rt: Consumer<Box<dyn Output>>,
    mut to_rt: Producer<Recovered>,
) {
    const SLICE_NS: u64 = 50_000_000;
    loop {
        match from_rt.pop() {
            Ok(mut out) => {
                let mut reopened = false;
                'retry: loop {
                    if shared.stop.load(Acquire) {
                        break;
                    }
                    match out.reopen() {
                        Ok(()) => {
                            reopened = true;
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("reopening the output failed: {e}; retrying");
                            let mut waited = 0;
                            while waited < retry_ns {
                                if shared.stop.load(Acquire) {
                                    break 'retry;
                                }
                                let step = SLICE_NS.min(retry_ns - waited);
                                clock.sleep_ns(step);
                                waited += step;
                            }
                        }
                    }
                }
                let mut item = Recovered { out, reopened };
                // The ring has room: the rt thread only sends one output at a time.
                while let Err(rtrb::PushError::Full(back)) = to_rt.push(item) {
                    item = back;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            Err(_) => {
                if shared.state.load(Acquire) == 3 {
                    return;
                }
                std::thread::park_timeout(std::time::Duration::from_millis(50));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The rt-out thread

pub struct RtContext {
    out: Box<dyn Output>,
    source: Box<dyn Source>,
    clock: Arc<dyn Clock>,
    shared: Arc<Shared>,
    cmds: Consumer<Command>,
    events: Producer<Event>,
    to_rec: Producer<Box<dyn Output>>,
    from_rec: Consumer<Recovered>,
    rec_thread: std::thread::Thread,
    /// The source's audio for the current block, as rendered (never modified in place).
    block: Vec<f32>,
    /// Scratch for the block with a fade-in applied.
    ramped: Vec<f32>,
    silence: Vec<f32>,
    pending: Vec<Command>,
    opts: RtOptions,
    priority_slot: Arc<std::sync::Mutex<Option<Priority>>>,
}

#[inline]
fn post(events: &mut Producer<Event>, shared: &Shared, frame: u64, kind: EventKind) {
    if events.push(Event { frame, kind }).is_err() {
        shared.event_drops.fetch_add(1, Relaxed);
    }
}

/// Publish the position (seqlock writer; the rt thread is the only writer, and never waits).
#[inline]
fn publish(shared: &Shared, written: u64, source_frames: u64, delay: i64, now_ns: u64) {
    let seq = shared.pos_seq.load(Relaxed);
    shared.pos_seq.store(seq.wrapping_add(1), Relaxed);
    fence(Release);
    shared.frames_written.store(written, Relaxed);
    shared.source_frames.store(source_frames, Relaxed);
    shared.delay_frames.store(delay, Relaxed);
    shared.position_ns.store(now_ns, Relaxed);
    shared.pos_seq.store(seq.wrapping_add(2), Release);
}

/// A linear fade-in applied when a block is written, not to the rendered block itself, so a block
/// that is sent again after a fault is faded once, from where the fade actually stands.
#[derive(Clone, Copy)]
struct Ramp {
    /// Frames already faded in (the next frame to reach the device has gain `(pos + 1) / len`).
    pos: f32,
    len: f32,
}

impl Ramp {
    fn idle() -> Self {
        Self { pos: 1.0, len: 1.0 }
    }
    fn start(len: f32) -> Self {
        Self { pos: 0.0, len }
    }
    fn active(&self) -> bool {
        self.pos < self.len
    }
    fn advance(&mut self, frames: usize) {
        self.pos = (self.pos + frames as f32).min(self.len);
    }
}

fn rt_main(mut c: RtContext) -> RtContext {
    if let Some(f) = &c.opts.on_start {
        f();
    }
    // Priority is requested once, at start, before the loop (the setter may allocate an error
    // string on failure; that is startup, not the audio path). It has to run on this thread:
    // `pthread_setschedparam` is applied to the calling thread.
    if let Some(p) = c.opts.realtime_priority {
        let got = sys::request_realtime(p);
        if let Ok(mut g) = c.priority_slot.lock() {
            *g = Some(got);
        }
    }

    let granted = c.out.granted().clone();
    let period = granted.period_frames as u64;
    let periods = granted.periods;
    let ms_to_frames = |ms: u32| (u64::from(ms) * u64::from(granted.rate) / 1000).max(1) as f32;
    let resume_fade = ms_to_frames(c.opts.resume_fade_ms);
    let declick = ms_to_frames(c.opts.xrun_fade_ms);
    // Taken out of the options so the loop can borrow it; put back before the context goes to the joiner.
    let mut tap = c.opts.stream_tap.take();
    let mut ramp = Ramp::idle();
    let mut source_frames: u64 = 0;
    let mut written: u64 = 0;
    let mut delay: i64 = 0;
    // The current block: rendered but not yet (completely) taken by the device.
    let mut have_block = false;
    let mut block_frames: usize = 0;
    // Frames at the front of the current block the device has already taken (a partial write
    // before a fault). They are never sent twice.
    let mut sent: usize = 0;

    c.shared.state.store(1, Release);
    post(&mut c.events, &c.shared, 0, EventKind::Started);

    'outer: loop {
        if c.shared.stop.load(Acquire) {
            break;
        }

        // Commands: take new ones, run the due ones in arrival order, keep the rest.
        while let Ok(cmd) = c.cmds.pop() {
            if let CommandKind::Mixer(_) = cmd.kind {
                c.source.command(&cmd.kind);
                continue;
            }
            if c.pending.len() < c.pending.capacity() {
                c.pending.push(cmd);
            } else {
                c.shared.command_drops.fetch_add(1, Relaxed);
            }
        }
        if !c.pending.is_empty() {
            let mut keep = 0;
            for i in 0..c.pending.len() {
                let cmd = c.pending[i];
                if cmd.at_frame <= written {
                    match cmd.kind {
                        CommandKind::Stall { ms } => c.clock.sleep_ns(u64::from(ms) * 1_000_000),
                        _ => c.source.command(&cmd.kind),
                    }
                } else {
                    c.pending[keep] = cmd;
                    keep += 1;
                }
            }
            c.pending.truncate(keep);
        }

        if !have_block {
            let n = match c.opts.max_frames {
                Some(m) => period.min(m.saturating_sub(source_frames)),
                None => period,
            } as usize;
            if n == 0 {
                break;
            }
            c.source.render(&mut c.block[..n * 2]);
            source_frames += n as u64;
            block_frames = n;
            sent = 0;
            have_block = true;
        }

        // What is left of the block, faded in if a ramp is running.
        let rest = &c.block[sent * 2..block_frames * 2];
        let buf: &[f32] = if ramp.active() {
            let out = &mut c.ramped[..rest.len()];
            for (i, (o, s)) in out
                .chunks_exact_mut(2)
                .zip(rest.chunks_exact(2))
                .enumerate()
            {
                let g = ((ramp.pos + 1.0 + i as f32) / ramp.len).min(1.0);
                o[0] = s[0] * g;
                o[1] = s[1] * g;
            }
            out
        } else {
            rest
        };

        let t0 = c.clock.monotonic_ns();
        let res = c.out.write(buf);
        let took = c.clock.monotonic_ns().saturating_sub(t0);
        if took > c.shared.max_write_ns.load(Relaxed) {
            c.shared.max_write_ns.store(took, Relaxed);
        }

        let e = match res {
            Ok(()) => {
                let n = block_frames - sent;
                if let Some(t) = tap.as_mut() {
                    t.push(written, buf);
                }
                written += n as u64;
                ramp.advance(n);
                have_block = false;
                delay = c.out.delay_frames().unwrap_or(delay);
                publish(
                    &c.shared,
                    written,
                    source_frames,
                    delay,
                    c.clock.monotonic_ns(),
                );
                continue;
            }
            Err(e) => e,
        };

        // Whatever the device took before failing counts as written and is not sent again.
        let done = (e.frames_done as usize).min(block_frames - sent);
        if let Some(t) = tap.as_mut() {
            t.push(written, &buf[..done * 2]);
        }
        sent += done;
        written += done as u64;
        ramp.advance(done);
        if sent == block_frames {
            have_block = false;
        }

        let mut lost = matches!(e.fault, Fault::Lost);
        match e.fault {
            Fault::Retry => continue 'outer,
            Fault::Xrun | Fault::Suspended | Fault::Other => {
                if c.out.recover(e.fault).is_err() {
                    lost = true;
                } else {
                    // Silence for one buffer, then the rest of the block, faded in: the source
                    // has not advanced past what was never played, so no audio is skipped.
                    let mut ok = true;
                    for _ in 0..periods {
                        match c.out.write(&c.silence) {
                            Ok(()) => written += period,
                            Err(se) => {
                                written += u64::from(se.frames_done).min(period);
                                ok = false;
                                break;
                            }
                        }
                    }
                    delay = c.out.delay_frames().unwrap_or(delay);
                    publish(
                        &c.shared,
                        written,
                        source_frames,
                        delay,
                        c.clock.monotonic_ns(),
                    );
                    ramp = Ramp::start(declick);
                    if ok {
                        let kind = if e.fault == Fault::Suspended {
                            c.shared.suspends.fetch_add(1, Relaxed);
                            EventKind::Resumed
                        } else {
                            c.shared.xruns.fetch_add(1, Relaxed);
                            EventKind::XrunRecovered
                        };
                        post(&mut c.events, &c.shared, written, kind);
                    } else {
                        lost = true;
                    }
                }
            }
            Fault::Lost => {}
        }
        if !lost {
            continue;
        }
        if e.fault == Fault::Other {
            post(
                &mut c.events,
                &c.shared,
                written,
                EventKind::Fault(Fault::Other),
            );
        }

        // ---- device lost: hand the output to the recovery thread ----
        c.shared.device_lost.fetch_add(1, Relaxed);
        c.shared.state.store(2, Release);
        post(&mut c.events, &c.shared, written, EventKind::DeviceLost);
        let t_lost = c.clock.monotonic_ns();
        // Move the output out, leaving a zero-sized stand-in (boxing a ZST does not allocate,
        // and dropping one does not free).
        let out = std::mem::replace(&mut c.out, Box::new(Gone));
        if let Err(rtrb::PushError::Full(out)) = c.to_rec.push(out) {
            // Cannot happen (capacity 1, and the recovery thread has taken the previous one
            // before it returns it), but never drop the output here: put it back and stop.
            c.out = out;
            break 'outer;
        }
        c.rec_thread.unpark();
        let back = loop {
            if let Ok(r) = c.from_rec.pop() {
                break r;
            }
            c.clock.sleep_ns(5_000_000);
        };
        c.out = back.out;
        if !back.reopened {
            break 'outer; // stop requested while the device was absent
        }
        let outage_ms = (c.clock.monotonic_ns().saturating_sub(t_lost) / 1_000_000)
            .min(u64::from(u32::MAX)) as u32;
        c.shared.device_restored.fetch_add(1, Relaxed);
        c.shared
            .outage_ms_total
            .fetch_add(u64::from(outage_ms), Relaxed);
        c.shared.state.store(1, Release);
        post(
            &mut c.events,
            &c.shared,
            written,
            EventKind::DeviceRestored { outage_ms },
        );
        // The rest of the block that failed is written next, then new blocks, all under the
        // 150 ms fade-in (architecture 1.5).
        ramp = Ramp::start(resume_fade);
    }

    c.opts.stream_tap = tap;
    post(&mut c.events, &c.shared, written, EventKind::Stopped);
    c.shared.state.store(3, Release);
    if let Some(f) = &c.opts.on_exit {
        f();
    }
    // The whole context goes back to the joiner, so that its buffers, rings and Arcs are freed
    // there and never by the rt thread.
    c
}

/// A stand-in that lives in `RtContext::out` while the real output is with the recovery thread.
/// Zero-sized, so boxing it does not allocate.
struct Gone;

impl Output for Gone {
    fn name(&self) -> &str {
        "gone"
    }
    fn granted(&self) -> &crate::negotiate::Granted {
        unreachable!("output is with the recovery thread")
    }
    fn write(&mut self, _: &[f32]) -> Result<(), crate::output::OutputError> {
        unreachable!()
    }
    fn delay_frames(&mut self) -> Option<i64> {
        None
    }
    fn recover(&mut self, _: Fault) -> Result<(), crate::output::OutputError> {
        unreachable!()
    }
    fn reopen(&mut self) -> Result<(), String> {
        unreachable!()
    }
}
