//! The program of `--stream-test` (architecture 1.3a, the drift soak): a 1 kHz tone at -20 dBFS RMS with
//! a **timing marker** and a **counter**, so a receiver can measure how far its clock and the box's
//! have drifted apart, and see any gap or repeat.
//!
//! * A 60 ms burst of 3 kHz on every whole second of the box's `CLOCK_REALTIME`. The burst starts at the
//!   frame where the whole second falls inside the block being rendered (sample-accurate to the extent the
//!   render time is steady, well under a millisecond on an idle box).
//! * Every 10 s (at a second divisible by 10) a 24-bit counter, the number of 10 s periods since the
//!   epoch, sent as 24 bursts of 50 ms starting 100 ms after that second's marker: a 1 is 2 kHz, a 0 is
//!   4 kHz, most significant bit first. A repeat or a gap of any size shows as a counter that does not
//!   advance by one.
//!
//! The decoder is `tools/stream-drift.py`. The source renders the station's real mix underneath it
//! (`inner`) only so that the mixer's state keeps moving; its output is replaced.

use engine::source::{rms_dbfs_to_peak, Source};
use engine::CommandKind;
use std::f64::consts::TAU;

pub const MARKER_HZ: f64 = 3000.0;
pub const MARKER_MS: u32 = 60;
pub const BIT_ONE_HZ: f64 = 2000.0;
pub const BIT_ZERO_HZ: f64 = 4000.0;
pub const BIT_MS: u32 = 50;
pub const BITS: u32 = 24;
/// The first bit starts this long after the marker second.
pub const BITS_OFFSET_MS: u32 = 100;
pub const TONE_HZ: f64 = 1000.0;

pub struct TimingSource {
    inner: Option<Box<dyn Source>>,
    rate: f64,
    frame: u64,
    now: Box<dyn FnMut() -> f64 + Send>,
    tone_amp: f32,
    burst_amp: f32,
    next_second: Option<i64>,
    marker_start: Option<u64>,
    counter: Option<(u64, u32)>,
}

fn realtime_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl TimingSource {
    pub fn new(rate: u32, inner: Option<Box<dyn Source>>) -> Self {
        Self::with_clock(rate, inner, Box::new(realtime_seconds))
    }

    /// `now` returns the box's `CLOCK_REALTIME` in seconds when called (tests pass a fake).
    pub fn with_clock(
        rate: u32,
        inner: Option<Box<dyn Source>>,
        now: Box<dyn FnMut() -> f64 + Send>,
    ) -> Self {
        Self {
            inner,
            rate: f64::from(rate),
            frame: 0,
            now,
            tone_amp: rms_dbfs_to_peak(-20.0),
            burst_amp: 0.1,
            next_second: None,
            marker_start: None,
            counter: None,
        }
    }

    fn ms(&self, ms: u32) -> u64 {
        (self.rate * f64::from(ms) / 1000.0).round() as u64
    }

    /// A burst's envelope at `k` frames into a burst of `len` frames: 2 ms raised-cosine edges.
    fn env(&self, k: u64, len: u64) -> f32 {
        let edge = self.ms(2).max(1);
        let a = if k < edge {
            0.5 - 0.5 * (std::f64::consts::PI * k as f64 / edge as f64).cos()
        } else {
            1.0
        };
        let r = len - 1 - k.min(len - 1);
        let b = if r < edge {
            0.5 - 0.5 * (std::f64::consts::PI * r as f64 / edge as f64).cos()
        } else {
            1.0
        };
        (a * b) as f32
    }
}

impl Source for TimingSource {
    fn render(&mut self, out: &mut [f32]) {
        if let Some(i) = self.inner.as_mut() {
            i.render(out);
        }
        let n = (out.len() / 2) as u64;
        let t = (self.now)();
        let block_s = n as f64 / self.rate;
        // Every whole second that falls inside (or before the end of) this block starts a marker.
        let mut sec = self.next_second.unwrap_or(t.ceil() as i64);
        while (sec as f64) < t + block_s {
            let off = ((sec as f64 - t) * self.rate)
                .round()
                .clamp(0.0, (n - 1) as f64) as u64;
            let start = self.frame + off;
            self.marker_start = Some(start);
            if sec % 10 == 0 {
                self.counter = Some((start, ((sec / 10) as u32) & 0x00FF_FFFF));
            }
            sec += 1;
        }
        self.next_second = Some(sec);

        let marker_len = self.ms(MARKER_MS);
        let bit_len = self.ms(BIT_MS);
        let bits_from = self.ms(BITS_OFFSET_MS);
        let bits_len = bit_len * u64::from(BITS);
        for (i, fr) in out.chunks_exact_mut(2).enumerate() {
            let f = self.frame + i as u64;
            let mut s = self.tone_amp * (TAU * TONE_HZ * f as f64 / self.rate).sin() as f32;
            if let Some(m) = self.marker_start {
                let k = f.wrapping_sub(m);
                if f >= m && k < marker_len {
                    s += self.burst_amp
                        * self.env(k, marker_len)
                        * (TAU * MARKER_HZ * k as f64 / self.rate).sin() as f32;
                }
            }
            if let Some((c, value)) = self.counter {
                if f >= c + bits_from && f < c + bits_from + bits_len {
                    let k = f - c - bits_from;
                    let bit = (k / bit_len) as u32;
                    let within = k % bit_len;
                    let one = (value >> (BITS - 1 - bit)) & 1 == 1;
                    let hz = if one { BIT_ONE_HZ } else { BIT_ZERO_HZ };
                    s += self.burst_amp
                        * self.env(within, bit_len)
                        * (TAU * hz * within as f64 / self.rate).sin() as f32;
                }
            }
            fr[0] = s;
            fr[1] = s;
        }
        self.frame += n;
    }

    fn command(&mut self, kind: &CommandKind) {
        if let Some(i) = self.inner.as_mut() {
            i.command(kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock that advances by exactly the audio rendered (an ideal pacer).
    fn ideal(start: f64, rate: f64, block: usize) -> Box<dyn FnMut() -> f64 + Send> {
        let mut k = 0u64;
        Box::new(move || {
            let t = start + (k * block as u64) as f64 / rate;
            k += 1;
            t
        })
    }

    fn goertzel(x: &[f32], hz: f64, rate: f64) -> f64 {
        let w = TAU * hz / rate;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            re += f64::from(*v) * (w * i as f64).cos();
            im += f64::from(*v) * (w * i as f64).sin();
        }
        (re * re + im * im).sqrt() / x.len() as f64 * 2.0
    }

    #[test]
    fn markers_land_on_whole_seconds_and_the_counter_decodes() {
        let rate = 48_000.0;
        let block = 1024;
        // Start 0.5 s before second 1000 (divisible by 10): counter 100.
        let mut s = TimingSource::with_clock(48_000, None, ideal(999.5, rate, block));
        let mut all: Vec<f32> = vec![];
        let mut buf = vec![0f32; block * 2];
        for _ in 0..(5.0 * rate / block as f64) as usize {
            s.render(&mut buf);
            all.extend(buf.iter().step_by(2));
        }
        // Marker for second 1000 is 0.5 s into the render (24,000 frames), within a frame or two.
        let at = 24_000usize;
        let win = &all[at + 200..at + 2400];
        assert!(goertzel(win, 3000.0, rate) > 0.08, "no marker");
        let before = &all[at - 2600..at - 200];
        assert!(goertzel(before, 3000.0, rate) < 0.001);
        // The next marker is exactly 48,000 frames later.
        let at2 = at + 48_000;
        assert!(goertzel(&all[at2 + 200..at2 + 2400], 3000.0, rate) > 0.08);
        // The counter: 100 = 0b000000000000000001100100.
        let bit_len = 2400usize;
        let from = at + 4800;
        let mut v = 0u32;
        for b in 0..24 {
            let w = &all[from + b * bit_len + 300..from + (b + 1) * bit_len - 300];
            let one = goertzel(w, 2000.0, rate);
            let zero = goertzel(w, 4000.0, rate);
            assert!(one > 0.05 || zero > 0.05, "bit {b}: {one} {zero}");
            v = v << 1 | u32::from(one > zero);
        }
        assert_eq!(v, 100);
        // The tone is -20 dBFS RMS between bursts.
        let quiet = &all[at + 70_000..at + 74_800];
        let rms =
            (quiet.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / quiet.len() as f64).sqrt();
        let db = 20.0 * rms.log10();
        assert!((db + 20.0).abs() < 0.05, "{db}");
    }
}
