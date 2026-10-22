//! The safety limiter (architecture 2.4): always on, lookahead 2 ms, ceiling -1.0 dBFS sample
//! peak, release 50 ms.
//!
//! Method. For each input frame the gain that would bring it to the ceiling is
//! `need = min(1, ceiling / peak)` (peak of the two channels, so the image does not shift). Let
//! `L` be the lookahead in frames. The sliding minimum `m[i]` of `need` over the last `L + 1`
//! frames is averaged over the last `L + 1` values of `m`. The output frame `s` is the input
//! frame `s` (delayed by `L`) times the average of `m[s ..= s + L]`; each of those `m` is a
//! minimum over a window that contains `s`, so the average is at most `need[s]`: **the ceiling
//! holds by construction** for every sample, whatever the signal. The averaging makes the attack
//! a smooth ramp of `L` frames. Release is limited on top: the gain may rise toward 1 no faster
//! than a one-pole with the release time constant, and the `min` with the averaged gain keeps
//! the guarantee.
//!
//! Everything is allocated in [`Limiter::new`]. `process` allocates nothing.

use std::collections::VecDeque;

pub const CEILING_DBFS: f64 = -1.0;
pub const LOOKAHEAD_S: f64 = 0.002;
pub const RELEASE_S: f64 = 0.050;

pub struct Limiter {
    la: usize,
    ceiling: f32,
    release_coef: f64,
    /// Delay line, `la` stereo frames.
    delay: Vec<f32>,
    delay_pos: usize,
    /// Monotonic deque for the sliding minimum: (frame index, need).
    dq: VecDeque<(u64, f32)>,
    /// The last `la + 1` values of `m`, and their running sum.
    mring: Vec<f64>,
    mpos: usize,
    msum: f64,
    /// Consecutive `m == 1` values, to reset the running sum exactly (no drift in 24/7 use).
    ones: usize,
    index: u64,
    gain: f64,
    /// Frames with any gain reduction, and with more than 1 dB of it (`/metrics`).
    pub active_frames: u64,
    pub over_1db_frames: u64,
    pub min_gain: f32,
    /// The smallest gain since [`take_window_min`](Self::take_window_min) (for the meter frame).
    window_min: f32,
}

impl Limiter {
    pub fn new(rate: u32) -> Self {
        let la = ((LOOKAHEAD_S * f64::from(rate)).round() as usize).max(1);
        Self {
            la,
            ceiling: 10f64.powf(CEILING_DBFS / 20.0) as f32,
            release_coef: 1.0 - (-1.0 / (RELEASE_S * f64::from(rate))).exp(),
            delay: vec![0.0; la * 2],
            delay_pos: 0,
            dq: VecDeque::with_capacity(la + 2),
            mring: vec![1.0; la + 1],
            mpos: 0,
            msum: (la + 1) as f64,
            ones: la + 1,
            index: 0,
            gain: 1.0,
            active_frames: 0,
            over_1db_frames: 0,
            min_gain: 1.0,
            window_min: 1.0,
        }
    }

    /// The smallest gain applied since the last call (1.0 when it did nothing), then reset.
    pub fn take_window_min(&mut self) -> f32 {
        std::mem::replace(&mut self.window_min, 1.0)
    }

    /// The delay the limiter adds, in frames.
    pub fn latency_frames(&self) -> usize {
        self.la
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    /// Move the ceiling (dBFS sample peak). The loudness controller raises it above -1.0 when its
    /// true-peak ceiling is set higher, so that the setting has an effect (6.1).
    pub fn set_ceiling_db(&mut self, db: f32) {
        self.ceiling = 10f32.powf(db / 20.0);
    }

    /// Process interleaved stereo frames: `input` to `output` (equal lengths).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        let la1 = self.la + 1;
        let over_1db = 10f64.powf(-1.0 / 20.0);
        for (x, y) in input.chunks_exact(2).zip(output.chunks_exact_mut(2)) {
            // Non-finite input is silenced here so that it cannot poison the gain or the output.
            let l = if x[0].is_finite() { x[0] } else { 0.0 };
            let r = if x[1].is_finite() { x[1] } else { 0.0 };
            let peak = l.abs().max(r.abs());
            let need = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };

            // Sliding minimum of `need` over the last la + 1 frames.
            while let Some(&(_, v)) = self.dq.back() {
                if v >= need {
                    self.dq.pop_back();
                } else {
                    break;
                }
            }
            self.dq.push_back((self.index, need));
            while let Some(&(i, _)) = self.dq.front() {
                if self.index - i >= la1 as u64 {
                    self.dq.pop_front();
                } else {
                    break;
                }
            }
            let m = self.dq.front().map_or(1.0, |&(_, v)| v);

            // Running average of the last la + 1 values of m.
            self.msum += f64::from(m) - self.mring[self.mpos];
            self.mring[self.mpos] = f64::from(m);
            self.mpos = (self.mpos + 1) % la1;
            if m == 1.0 {
                self.ones += 1;
                if self.ones >= la1 {
                    self.msum = la1 as f64;
                }
            } else {
                self.ones = 0;
            }
            let avg = (self.msum / la1 as f64).min(1.0);

            // Release-limited rise toward 1, never above the averaged gain.
            let mut g = self.gain + (1.0 - self.gain) * self.release_coef;
            if 1.0 - g < 1e-9 {
                g = 1.0;
            }
            let g = avg.min(g);
            self.gain = g;

            // Output frame: the input from `la` frames ago.
            let d = self.delay_pos * 2;
            let (ol, or) = (self.delay[d], self.delay[d + 1]);
            self.delay[d] = l;
            self.delay[d + 1] = r;
            self.delay_pos += 1;
            if self.delay_pos == self.la {
                self.delay_pos = 0;
            }
            let gf = g as f32;
            y[0] = ol * gf;
            y[1] = or * gf;

            if g < 1.0 {
                self.active_frames += 1;
                if g < over_1db {
                    self.over_1db_frames += 1;
                }
                if gf < self.min_gain {
                    self.min_gain = gf;
                }
                if gf < self.window_min {
                    self.window_min = gf;
                }
            }
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(l: &mut Limiter, x: &[f32]) -> Vec<f32> {
        let mut y = vec![0.0; x.len()];
        l.process(x, &mut y);
        y
    }

    #[test]
    fn below_the_ceiling_it_is_a_bit_exact_delay() {
        let mut l = Limiter::new(48_000);
        let d = l.latency_frames();
        let x: Vec<f32> = (0..2 * 4000)
            .map(|i| ((i as f32) * 0.01).sin() * 0.8)
            .collect();
        let y = run(&mut l, &x);
        assert!(y[..2 * d].iter().all(|&v| v == 0.0));
        assert_eq!(&y[2 * d..], &x[..x.len() - 2 * d]);
        assert_eq!(l.active_frames, 0);
    }

    #[test]
    fn ceiling_holds_for_hot_and_random_input() {
        let mut l = Limiter::new(48_000);
        let ceil = l.ceiling();
        let mut seed = 12345u64;
        let mut x = vec![0.0f32; 2 * 200_000];
        for (i, v) in x.iter_mut().enumerate() {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = ((seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
            // bursts of hot noise between quiet passages
            let burst = (i / 20_000) % 2 == 0;
            *v = if burst { r * 6.0 } else { r * 0.1 };
        }
        let y = run(&mut l, &x);
        let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= ceil * 1.000_001, "{peak} > {ceil}");
        assert!(l.active_frames > 0);
    }

    #[test]
    fn non_finite_input_is_silenced() {
        let mut l = Limiter::new(48_000);
        let mut x = vec![0.5f32; 2 * 1000];
        x[10] = f32::NAN;
        x[11] = f32::INFINITY;
        let y = run(&mut l, &x);
        assert!(y.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn recovers_to_unity_after_release() {
        let mut l = Limiter::new(48_000);
        let mut x = vec![0.0f32; 2 * 48_000 * 2];
        for v in x[..2 * 4800].iter_mut() {
            *v = 3.0;
        }
        for v in x[2 * 4800..].iter_mut() {
            *v = 0.5;
        }
        let y = run(&mut l, &x);
        let tail = &y[y.len() - 2 * 1000..];
        assert!(tail.iter().all(|&v| v == 0.5), "gain back at exactly 1");
    }
}
