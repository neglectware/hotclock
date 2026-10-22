//! The link between a decoder and a voice: a lock-free ring of interleaved stereo `f32` at the
//! device rate, and a status block of atomics (architecture 1.4).
//!
//! The decoder pool owns the producer end and writes the status; rt-out owns the consumer end
//! (inside a voice) and reads the status with atomic loads only. Rings are allocated on the
//! control side, before the voice is loaded, never on rt-out.

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Arc, Mutex};

/// Seconds of audio a voice ring holds (architecture 1.4).
pub const RING_SECONDS: usize = 20;

/// Status of one decode job. Written by the pool (and, for `consumed_frames`, by rt-out).
#[derive(Default)]
pub struct FeedStatus {
    /// The file has been opened and its facts (`expected_frames`) are known, or opening failed.
    pub opened: AtomicBool,
    /// The last frame is in the ring (set with `Release` after the data is pushed, so a reader
    /// that sees it with `Acquire` and then looks at the ring sees every frame).
    pub eof: AtomicBool,
    /// Decoding failed (open error, fatal decode error, too many glitches) or the file was
    /// truncated. Audio decoded before the failure is still in the ring; `eof` is also set.
    pub failed: AtomicBool,
    /// The file ended well short of its declared length.
    pub truncated: AtomicBool,
    /// The exact number of frames the decoder produced. Valid once `eof` is set.
    pub total_frames: AtomicU64,
    /// Frames pushed into the ring so far.
    pub pushed_frames: AtomicU64,
    /// Frames rt-out has taken from the ring (the voice's position in its file, from its start
    /// offset).
    pub consumed_frames: AtomicU64,
    /// Recoverable decode errors so far (each was padded with silence).
    pub glitches: AtomicU32,
    /// The length in output frames the container declares, or 0 when unknown. Exact for FLAC,
    /// ALAC, WAV and AIFF; a hint for MP3 and AAC (use `total_frames` once `eof` is set).
    pub expected_frames: AtomicU64,
    /// Human-readable reason when `failed`; read by the control side only.
    pub error: Mutex<Option<String>>,
}

impl FeedStatus {
    pub fn expected(&self) -> Option<u64> {
        match self.expected_frames.load(Acquire) {
            0 => None,
            n => Some(n),
        }
    }

    pub fn is_eof(&self) -> bool {
        self.eof.load(Acquire)
    }

    /// The exact length, once the decoder has finished.
    pub fn total(&self) -> Option<u64> {
        self.is_eof().then(|| self.total_frames.load(Acquire))
    }

    /// Frames decoded into the ring and not yet taken by rt-out. The conductor raises
    /// `decode_slow` when this stays under 3 s for a playing voice (architecture 1.4).
    pub fn buffered_frames(&self) -> u64 {
        self.pushed_frames
            .load(Acquire)
            .saturating_sub(self.consumed_frames.load(Acquire))
    }

    pub fn error_message(&self) -> Option<String> {
        self.error.lock().ok().and_then(|g| g.clone())
    }

    pub fn set_error(&self, msg: String) {
        if let Ok(mut g) = self.error.lock() {
            *g = Some(msg);
        }
        self.failed.store(true, Release);
    }
}

/// The consumer end of a voice ring and its status: what the control side hands to
/// [`crate::mixer::MixerControl::load`].
pub struct VoiceFeed {
    pub ring: Consumer<f32>,
    pub status: Arc<FeedStatus>,
}

/// A ring of `frames` stereo frames and its two ends.
pub fn new_ring(frames: usize) -> (Producer<f32>, Consumer<f32>) {
    RingBuffer::new(frames.max(1) * 2)
}

impl VoiceFeed {
    /// A feed that is already complete: the given interleaved stereo samples, in memory. Used
    /// for tests, and for the emergency RAM loop.
    pub fn from_samples(samples: &[f32]) -> VoiceFeed {
        assert_eq!(samples.len() % 2, 0, "interleaved stereo");
        let frames = samples.len() / 2;
        let (mut prod, ring) = new_ring(frames);
        if !samples.is_empty() {
            let chunk = prod
                .write_chunk_uninit(samples.len())
                .expect("ring sized to fit");
            chunk.fill_from_iter(samples.iter().copied());
        }
        let status = Arc::new(FeedStatus::default());
        status.pushed_frames.store(frames as u64, Relaxed);
        status.total_frames.store(frames as u64, Relaxed);
        status.expected_frames.store(frames as u64, Relaxed);
        status.opened.store(true, Relaxed);
        status.eof.store(true, Release);
        VoiceFeed { ring, status }
    }
}
