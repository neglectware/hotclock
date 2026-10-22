//! The stream tap (architecture 1.2 and 1.3a): a wait-free ring from the rt-out thread to the
//! stream encoder.
//!
//! rt-out pushes, once per period, the same buffer the device receives (after the chain and the
//! safety limiter, after any fade-in), tagged with the station frame the period starts at. The ring
//! is `rtrb` over `u32` words: the `f32` samples travel as their bit patterns, so a period is one
//! contiguous reservation (three header words, then the samples) and a reader that sees the header
//! also sees the whole period. A full ring drops the period and counts it. rt-out never waits for
//! the encoder, and never allocates, locks or logs here.
//!
//! Frames that never reach the ring (dropped periods) and the silence rt-out writes after an xrun
//! are visible to the encoder as a jump in `start_frame`; it fills the jump with silence so the
//! stream's timeline stays exact.

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// Header words before the samples: start frame (low, high), frames in the period.
const HEADER: usize = 3;

/// Counters shared by the two ends.
#[derive(Default)]
pub struct TapStats {
    /// Frames pushed into the ring.
    pub pushed_frames: AtomicU64,
    /// Periods dropped because the ring was full.
    pub dropped_periods: AtomicU64,
    /// Frames in those periods.
    pub dropped_frames: AtomicU64,
}

pub struct TapProducer {
    ring: Producer<u32>,
    stats: Arc<TapStats>,
}

pub struct TapConsumer {
    ring: Consumer<u32>,
    stats: Arc<TapStats>,
}

/// One period read from the ring: where it starts on the station's frame clock, and how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapPeriod {
    pub start_frame: u64,
    pub frames: usize,
}

/// A tap that holds up to `capacity_frames` frames of interleaved stereo (plus headers for up to
/// `max_periods` periods).
pub fn stream_tap(capacity_frames: usize, max_periods: usize) -> (TapProducer, TapConsumer) {
    let (ring_tx, ring_rx) = RingBuffer::<u32>::new(capacity_frames * 2 + HEADER * max_periods);
    let stats = Arc::new(TapStats::default());
    (
        TapProducer {
            ring: ring_tx,
            stats: stats.clone(),
        },
        TapConsumer {
            ring: ring_rx,
            stats,
        },
    )
}

impl TapProducer {
    /// Push one period (`samples` interleaved stereo) that starts at station frame `start_frame`.
    /// Wait-free. Returns false if the ring was full (the period is dropped and counted).
    #[inline]
    pub fn push(&mut self, start_frame: u64, samples: &[f32]) -> bool {
        let frames = (samples.len() / 2) as u64;
        if frames == 0 {
            return true;
        }
        match self.ring.write_chunk_uninit(HEADER + samples.len()) {
            Ok(chunk) => {
                let header = [
                    start_frame as u32,
                    (start_frame >> 32) as u32,
                    frames as u32,
                ];
                chunk.fill_from_iter(
                    header
                        .into_iter()
                        .chain(samples.iter().map(|s| s.to_bits())),
                );
                self.stats.pushed_frames.fetch_add(frames, Relaxed);
                true
            }
            Err(_) => {
                self.stats.dropped_periods.fetch_add(1, Relaxed);
                self.stats.dropped_frames.fetch_add(frames, Relaxed);
                false
            }
        }
    }

    pub fn stats(&self) -> &Arc<TapStats> {
        &self.stats
    }
}

impl TapConsumer {
    /// Take the next period, appending its samples to `out` (which is cleared first).
    pub fn pop_into(&mut self, out: &mut Vec<f32>) -> Option<TapPeriod> {
        out.clear();
        if self.ring.slots() < HEADER {
            return None;
        }
        let mut header = [0u32; HEADER];
        {
            let chunk = self.ring.read_chunk(HEADER).ok()?;
            for (h, w) in header.iter_mut().zip(chunk) {
                *h = w;
            }
        }
        let frames = header[2] as usize;
        // The producer commits a period in one step, so its samples are all here.
        let chunk = self.ring.read_chunk(frames * 2).ok()?;
        out.extend(chunk.into_iter().map(f32::from_bits));
        Some(TapPeriod {
            start_frame: u64::from(header[0]) | (u64::from(header[1]) << 32),
            frames,
        })
    }

    /// Frames waiting in the ring (headers ignored).
    pub fn queued_frames(&self) -> usize {
        self.ring.slots() / 2
    }

    pub fn stats(&self) -> &Arc<TapStats> {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_round_trip_and_a_full_ring_drops_and_counts() {
        let (mut tx, mut rx) = stream_tap(2048, 8);
        let a: Vec<f32> = (0..2048).map(|i| i as f32 * 0.001 - 1.0).collect();
        assert!(tx.push(1 << 33, &a));
        assert!(tx.push(1024 + (1 << 33), &a));
        // 2 x 2 x 1024 words queued, the ring holds 4096 + 24: a third period does not fit.
        assert!(!tx.push(2048 + (1 << 33), &a));
        assert_eq!(tx.stats().dropped_periods.load(Relaxed), 1);
        assert_eq!(tx.stats().dropped_frames.load(Relaxed), 1024);
        let mut buf = vec![];
        let p = rx.pop_into(&mut buf).unwrap();
        assert_eq!(
            p,
            TapPeriod {
                start_frame: 1 << 33,
                frames: 1024
            }
        );
        assert_eq!(buf, a);
        let p = rx.pop_into(&mut buf).unwrap();
        assert_eq!(p.start_frame, 1024 + (1 << 33));
        assert!(rx.pop_into(&mut buf).is_none());
        // Room again.
        assert!(tx.push(5, &a));
    }

    #[test]
    fn bit_patterns_survive() {
        let (mut tx, mut rx) = stream_tap(16, 4);
        let s = [f32::NAN, -0.0, f32::INFINITY, 1e-40];
        tx.push(0, &s);
        let mut b = vec![];
        rx.pop_into(&mut b).unwrap();
        for (x, y) in s.iter().zip(&b) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
    }
}
