//! The decoder pool (architecture 1.2, 1.4): three normal-priority threads that decode files
//! into voice rings, and decode head preloads into memory. None of this is real-time code; it
//! may allocate, lock and read files. It never touches ALSA and never waits for rt-out.
//!
//! * A **voice job** owns a decoder and the producer end of a voice ring (20 s). Workers pick
//!   the job whose ring is emptiest first (that is the "reprioritize" rule of 1.4), decode until
//!   the ring is nearly full, and put the job back.
//! * A **preload** decodes the first 30 s after the cue-in into memory as soon as an item enters
//!   the queue. A voice opened from a preload starts by draining that memory into its ring, so a
//!   slow disk at segue time cannot make a late start; decoding then carries on from where the
//!   preload stopped, with the same decoder (no seam).
//! * A job whose ring consumer was dropped (the voice was released) is discarded.

use crate::decode::{DecodeError, FileDecoder};
use crate::feed::{new_ring, FeedStatus, VoiceFeed, RING_SECONDS};
use rtrb::Producer;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Seconds decoded ahead by a head preload (architecture 1.4).
pub const PRELOAD_SECONDS: f64 = 30.0;
/// A worker refills a ring once at least this many seconds of it are free.
const REFILL_FREE_S: f64 = 0.5;
/// A voice ring holding less than this many seconds is served before any preload: a queue of
/// preloads (each one decodes 30 s without yielding) must never starve a voice that is on air.
const LOW_WATER_S: f64 = 5.0;
/// The longest a worker keeps one job before letting it go back and re-picking.
const SLICE: Duration = Duration::from_millis(40);

/// Where a voice's audio comes from.
#[allow(clippy::large_enum_variant)]
pub enum VoiceSource {
    /// Open the file, starting `start_s` seconds in.
    File { path: PathBuf, start_s: f64 },
    /// Continue from a finished preload.
    Preload(Preload),
    /// Play audio held in memory (the RAM loop, architecture 1.5), starting `start_s` seconds in.
    Memory { clip: Arc<RamClip>, start_s: f64 },
}

/// Audio held in memory: interleaved stereo, 16-bit, at the pool's rate.
pub struct RamClip {
    pub samples: Vec<i16>,
    pub rate: u32,
}

impl RamClip {
    pub fn frames(&self) -> u64 {
        (self.samples.len() / 2) as u64
    }
}

/// The head of a file, decoded into memory, and the decoder positioned after it.
pub struct Preload {
    decoder: FileDecoder,
    head: Vec<f32>,
    ended: bool,
    status: Arc<FeedStatus>,
    frames: u64,
}

impl Preload {
    /// Frames held in memory.
    pub fn frames(&self) -> u64 {
        self.frames
    }
    pub fn status(&self) -> &Arc<FeedStatus> {
        &self.status
    }
}

/// A preload in progress. Poll [`PreloadHandle::take`].
pub struct PreloadHandle {
    slot: Mutex<Option<Result<Preload, DecodeError>>>,
    ready: AtomicBool,
}

impl PreloadHandle {
    pub fn is_ready(&self) -> bool {
        self.ready.load(Acquire)
    }
    /// The finished preload (or its error), once. `None` while it is still running.
    pub fn take(&self) -> Option<Result<Preload, DecodeError>> {
        if !self.is_ready() {
            return None;
        }
        self.slot.lock().ok().and_then(|mut g| g.take())
    }
}

struct PreloadTask {
    path: PathBuf,
    start_s: f64,
    rate: u32,
    seconds: f64,
    handle: Arc<PreloadHandle>,
}

struct VoiceJob {
    source: Option<VoiceSource>,
    rate: u32,
    prod: Producer<f32>,
    capacity_frames: usize,
    status: Arc<FeedStatus>,
    decoder: Option<FileDecoder>,
    /// Decoded frames not yet in the ring (interleaved stereo), from `pending_pos`.
    pending: Vec<f32>,
    pending_pos: usize,
    decoder_ended: bool,
    pushed: u64,
    /// The job's source has been opened (or its memory clip adopted).
    started: bool,
    /// A memory source: the clip and the next sample index to send.
    mem: Option<(Arc<RamClip>, usize)>,
}

impl VoiceJob {
    fn free_frames(&self) -> usize {
        self.prod.slots() / 2
    }
    fn fill_frames(&self) -> usize {
        self.capacity_frames - self.free_frames()
    }
}

#[derive(Default)]
#[allow(clippy::vec_box)] // jobs move between threads by value; boxes keep that cheap
struct Queue {
    voices: Vec<Box<VoiceJob>>,
    preloads: Vec<PreloadTask>,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
    shutdown: AtomicBool,
}

pub struct DecoderPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    rate: u32,
}

impl DecoderPool {
    /// Start `threads` workers (architecture: 3) decoding to `rate`.
    pub fn new(threads: usize, rate: u32) -> Self {
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue::default()),
            cv: Condvar::new(),
            shutdown: AtomicBool::new(false),
        });
        let threads = (0..threads.max(1))
            .map(|i| {
                let s = shared.clone();
                std::thread::Builder::new()
                    .name(format!("decode-{i}"))
                    .spawn(move || worker(s))
                    .expect("spawn decoder thread")
            })
            .collect();
        Self {
            shared,
            threads,
            rate,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Decode the head of `path` into memory in the background.
    pub fn preload(&self, path: PathBuf, start_s: f64, seconds: f64) -> Arc<PreloadHandle> {
        let handle = Arc::new(PreloadHandle {
            slot: Mutex::new(None),
            ready: AtomicBool::new(false),
        });
        let task = PreloadTask {
            path,
            start_s,
            rate: self.rate,
            seconds,
            handle: handle.clone(),
        };
        if let Ok(mut q) = self.shared.q.lock() {
            q.preloads.push(task);
        }
        self.shared.cv.notify_one();
        handle
    }

    /// Create a voice ring and start decoding into it. Returns at once; the file is opened by a
    /// worker (watch `status.opened`, `status.failed`).
    pub fn open(&self, source: VoiceSource) -> VoiceFeed {
        let capacity_frames = RING_SECONDS * self.rate as usize;
        let (prod, ring) = new_ring(capacity_frames);
        let status = match &source {
            VoiceSource::Preload(p) => p.status.clone(),
            VoiceSource::File { .. } | VoiceSource::Memory { .. } => {
                Arc::new(FeedStatus::default())
            }
        };
        let job = Box::new(VoiceJob {
            source: Some(source),
            rate: self.rate,
            prod,
            capacity_frames,
            status: status.clone(),
            decoder: None,
            pending: Vec::new(),
            pending_pos: 0,
            decoder_ended: false,
            pushed: 0,
            started: false,
            mem: None,
        });
        if let Ok(mut q) = self.shared.q.lock() {
            q.voices.push(job);
        }
        self.shared.cv.notify_one();
        VoiceFeed { ring, status }
    }

    /// Stop the workers and wait for them.
    pub fn shutdown(&mut self) {
        self.shared.shutdown.store(true, Release);
        self.shared.cv.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for DecoderPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum Work {
    Preload(PreloadTask),
    Voice(Box<VoiceJob>),
}

fn worker(shared: Arc<Shared>) {
    loop {
        if shared.shutdown.load(Acquire) {
            return;
        }
        let work = {
            let Ok(mut q) = shared.q.lock() else { return };
            pick(&mut q)
        };
        match work {
            Some(Work::Preload(t)) => run_preload(t),
            Some(Work::Voice(mut job)) => {
                let done = service(&mut job);
                if !done {
                    if let Ok(mut q) = shared.q.lock() {
                        q.voices.push(job);
                    }
                }
            }
            None => {
                let Ok(q) = shared.q.lock() else { return };
                let _ = shared.cv.wait_timeout(q, Duration::from_millis(15));
            }
        }
    }
}

/// The next piece of work: a voice whose ring is under [`LOW_WATER_S`], else the oldest
/// preload still wanted, else the voice whose ring is emptiest among those with work to do.
/// Jobs whose voice was released, and preloads whose handle was dropped, are discarded here.
fn pick(q: &mut Queue) -> Option<Work> {
    q.voices.retain(|j| !j.prod.is_abandoned());
    q.preloads.retain(|t| Arc::strong_count(&t.handle) > 1);
    // The voice whose ring is emptiest, if it has room to refill (or has not started, or has
    // something left to push, or has to publish its end).
    let mut best: Option<usize> = None;
    for (i, j) in q.voices.iter().enumerate() {
        let needs = !j.started && j.source.is_some()
            || (j.pending_pos < j.pending.len() && j.free_frames() > 0)
            || (j.decoder_ended && j.pending_pos >= j.pending.len())
            || (!j.decoder_ended && j.free_frames() as f64 >= REFILL_FREE_S * f64::from(j.rate));
        if needs && best.is_none_or(|b| j.fill_frames() < q.voices[b].fill_frames()) {
            best = Some(i);
        }
    }
    let low = best.is_some_and(|b| {
        let j = &q.voices[b];
        (j.fill_frames() as f64) < LOW_WATER_S * f64::from(j.rate)
    });
    if !low && !q.preloads.is_empty() {
        return Some(Work::Preload(q.preloads.remove(0)));
    }
    best.map(|i| Work::Voice(q.voices.swap_remove(i)))
}

fn run_preload(t: PreloadTask) {
    let result = (|| {
        let mut decoder = FileDecoder::open(&t.path, t.rate, t.start_s)?;
        let status = Arc::new(FeedStatus::default());
        if let Some(e) = decoder.expected_frames() {
            status.expected_frames.store(e, Release);
        }
        status.opened.store(true, Release);
        let want = (t.seconds * f64::from(t.rate)) as usize * 2;
        let mut head = Vec::with_capacity(want + 8192);
        let mut ended = false;
        while head.len() < want {
            if Arc::strong_count(&t.handle) == 1 {
                // Nobody wants it any more; stop decoding (the result is dropped unseen).
                break;
            }
            if !decoder.decode_next(&mut head)? {
                ended = true;
                break;
            }
        }
        let frames = (head.len() / 2) as u64;
        Ok(Preload {
            decoder,
            head,
            ended,
            status,
            frames,
        })
    })();
    if let Ok(mut g) = t.handle.slot.lock() {
        *g = Some(result);
    }
    t.handle.ready.store(true, Release);
}

/// Push what is pending, decode more while there is room and time. True when the job is
/// finished (all frames pushed and `eof` published) or its ring is gone.
fn service(job: &mut VoiceJob) -> bool {
    let t0 = Instant::now();
    if !job.started {
        if !start_job(job) {
            return true;
        }
        job.started = true;
    }
    loop {
        if job.prod.is_abandoned() {
            return true;
        }
        push_pending(job);
        if job.pending_pos < job.pending.len() {
            return false; // ring full
        }
        job.pending.clear();
        job.pending_pos = 0;
        if job.decoder_ended {
            let st = &job.status;
            if let Some(d) = &job.decoder {
                st.glitches.store(d.glitches(), Release);
                if d.truncated() {
                    st.truncated.store(true, Release);
                    if !st.failed.load(Acquire) {
                        st.set_error("the file ends before its declared length".into());
                    }
                }
            }
            st.total_frames.store(job.pushed, Release);
            st.eof.store(true, Release);
            return true;
        }
        if job.free_frames() < 1024 || t0.elapsed() >= SLICE {
            return false;
        }
        if let Some((clip, pos)) = job.mem.as_mut() {
            // A memory source: convert the next few thousand frames.
            let end = (*pos + 8192).min(clip.samples.len());
            job.pending.extend(
                clip.samples[*pos..end]
                    .iter()
                    .map(|&v| f32::from(v) / 32768.0),
            );
            *pos = end;
            if end >= clip.samples.len() {
                job.decoder_ended = true;
            }
            continue;
        }
        let Some(dec) = job.decoder.as_mut() else {
            return true;
        };
        match dec.decode_next(&mut job.pending) {
            Ok(true) => {}
            Ok(false) => job.decoder_ended = true,
            Err(e) => {
                job.status.set_error(e.to_string());
                job.decoder_ended = true;
            }
        }
        job.status.glitches.store(dec.glitches(), Relaxed);
    }
}

/// Open the decoder (or adopt the preload) and queue the preloaded head. False if it failed.
fn start_job(job: &mut VoiceJob) -> bool {
    match job.source.take() {
        Some(VoiceSource::File { path, start_s }) => {
            match FileDecoder::open(&path, job.rate, start_s) {
                Ok(d) => {
                    if let Some(e) = d.expected_frames() {
                        job.status.expected_frames.store(e, Release);
                    }
                    job.decoder = Some(d);
                    job.status.opened.store(true, Release);
                    true
                }
                Err(e) => {
                    job.status.set_error(e.to_string());
                    job.status.opened.store(true, Release);
                    job.status.total_frames.store(0, Release);
                    job.status.eof.store(true, Release);
                    false
                }
            }
        }
        Some(VoiceSource::Memory { clip, start_s }) => {
            let start =
                ((start_s.max(0.0) * f64::from(clip.rate)) as usize * 2).min(clip.samples.len());
            job.status
                .expected_frames
                .store(clip.frames().saturating_sub((start / 2) as u64), Release);
            job.mem = Some((clip, start));
            job.status.opened.store(true, Release);
            true
        }
        Some(VoiceSource::Preload(p)) => {
            job.decoder_ended = p.ended;
            job.pending = p.head;
            job.pending_pos = 0;
            job.decoder = Some(p.decoder);
            true
        }
        None => false,
    }
}

fn push_pending(job: &mut VoiceJob) {
    let left = job.pending.len() - job.pending_pos;
    let room = job.prod.slots() & !1;
    let n = left.min(room) & !1;
    if n == 0 {
        return;
    }
    let chunk = job.prod.write_chunk_uninit(n).expect("checked room");
    let end = job.pending_pos + n;
    chunk.fill_from_iter(job.pending[job.pending_pos..end].iter().copied());
    job.pending_pos = end;
    job.pushed += (n / 2) as u64;
    job.status.pushed_frames.store(job.pushed, Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(capacity_frames: usize, filled: usize) -> (Box<VoiceJob>, rtrb::Consumer<f32>) {
        let (mut prod, cons) = new_ring(capacity_frames);
        if filled > 0 {
            let c = prod.write_chunk_uninit(filled * 2).unwrap();
            c.fill_from_iter(std::iter::repeat_n(0.0f32, filled * 2));
        }
        let j = Box::new(VoiceJob {
            source: None,
            rate: 1000,
            prod,
            capacity_frames,
            status: Arc::new(FeedStatus::default()),
            decoder: None,
            pending: Vec::new(),
            pending_pos: 0,
            decoder_ended: false,
            pushed: 0,
            started: true,
            mem: None,
        });
        (j, cons)
    }

    fn preload_task() -> (PreloadTask, Arc<PreloadHandle>) {
        let handle = Arc::new(PreloadHandle {
            slot: Mutex::new(None),
            ready: AtomicBool::new(false),
        });
        let t = PreloadTask {
            path: PathBuf::from("/nonexistent"),
            start_s: 0.0,
            rate: 1000,
            seconds: 30.0,
            handle: handle.clone(),
        };
        (t, handle)
    }

    #[test]
    fn a_voice_under_low_water_goes_before_preloads() {
        // Rate 1000: low water is 5,000 frames. A 20,000-frame ring holding 2,000 frames.
        let mut q = Queue::default();
        let (low, _c1) = job(20_000, 2_000);
        q.voices.push(low);
        let (t, _h) = preload_task();
        q.preloads.push(t);
        assert!(matches!(pick(&mut q), Some(Work::Voice(_))));
        assert_eq!(q.preloads.len(), 1);
    }

    #[test]
    fn preloads_go_before_a_voice_with_enough_buffered() {
        let mut q = Queue::default();
        let (ok, _c1) = job(20_000, 12_000);
        q.voices.push(ok);
        let (t, _h) = preload_task();
        q.preloads.push(t);
        assert!(matches!(pick(&mut q), Some(Work::Preload(_))));
        assert!(matches!(pick(&mut q), Some(Work::Voice(_))));
    }

    #[test]
    fn dropped_preloads_and_released_voices_are_discarded() {
        let mut q = Queue::default();
        let (gone, c) = job(20_000, 0);
        drop(c);
        q.voices.push(gone);
        let (t, h) = preload_task();
        drop(h);
        q.preloads.push(t);
        assert!(pick(&mut q).is_none());
        assert!(q.voices.is_empty() && q.preloads.is_empty());
    }
}
