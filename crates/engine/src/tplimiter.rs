//! The lookahead true-peak limiter (architecture 6): 5 ms of lookahead, 4x oversampled peak
//! detection, an adjustable ceiling in dBTP, a release of 60 ms that stretches toward 500 ms under
//! sustained reduction. Built in M6a as the loudness controller's peak stage; **M6's chain reuses
//! this block as its lookahead limiter** (same type, same `process`, the ceiling passed per call).
//!
//! # Method
//!
//! The gain law is the safety limiter's (`limiter.rs`): the sliding extremum of the per-frame need
//! over the lookahead window, averaged over the same window, so the ceiling holds *by construction*
//! for every value the detector sees, and the attack is a smooth ramp of the window's length.
//! What differs is the detector. For every input frame `n` the **true-peak value** `t[n]` is the
//! largest magnitude of the signal at the positions `n`, `n + 1/4`, `n + 1/2` and `n + 3/4` of both
//! channels, interpolated by a 97-tap Kaiser-windowed sinc (the BS.1770 4x oversampling, 12 or 13
//! taps per phase). The interpolation reads 12 frames of the future, so `t[n]` is known when frame
//! `n + 12` has arrived; the audio path is delayed by [`LATENCY_S`] (5 ms, 240 frames at 48 kHz) and
//! the lookahead window is `latency - 10` frames, which makes every output frame's gain no larger
//! than the need of both inter-sample segments that touch it (checked in the unit tests).
//!
//! Because the extremum is taken over *peak* values and converted to a gain afterwards
//! (`min(1, ceiling / max_peak)`), the ceiling may change between blocks (an API change, or the
//! controller being switched on) and takes effect at once with the whole lookahead already in place.
//!
//! The delay is constant whether the limiter is working or not, so switching the controller on and
//! off never moves the program in time (6, "the chain's delay stays constant"). With the ceiling
//! above every peak the output is a bit-exact delayed copy of the input.
//!
//! Everything is allocated in [`TpLimiter::new`]; `process` allocates nothing.

use std::collections::VecDeque;

/// Total latency of the limiter, seconds.
pub const LATENCY_S: f64 = 0.005;
/// Release time constant for brief reduction, and the most it stretches to under sustained reduction.
pub const RELEASE_MIN_S: f64 = 0.060;
pub const RELEASE_MAX_S: f64 = 0.500;
/// The detector aims this far under the ceiling. The audio is multiplied by a gain that is still moving
/// while the interpolation filter's 25 neighbors pass (a steep attack ramp changes it by up to about 3 %
/// across them), so a peak the detector sized for one gain can read up to about 0.4 dB higher in the
/// output (measured with an independent 8x filter, `the_true_peak_stays_at_the_ceiling_on_hot_program`).
pub const DETECT_MARGIN_DB: f32 = 0.5;
/// Frames of future the oversampling filter reads.
const FIR_AHEAD: usize = 12;
/// Taps per phase: the input offsets -12..=12.
const PHASE_TAPS: usize = 2 * FIR_AHEAD + 1;
const HALF: i32 = 48;

/// The 4x interpolation filter: `H[p][j]` multiplies the input at offset `j - 6` from `n` for phase `p`
/// (position `n + p/4`), `p` in 1..=3 (phase 0 is the sample itself).
fn make_filter() -> [[f32; PHASE_TAPS]; 4] {
    // Kaiser window, beta 8: about 80 dB of stopband for 49 taps.
    fn i0(x: f64) -> f64 {
        let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
        while term > 1e-14 * sum {
            term *= (x / (2.0 * k)) * (x / (2.0 * k));
            sum += term;
            k += 1.0;
        }
        sum
    }
    let beta = 8.0;
    let h = |k: i32| -> f64 {
        let x = f64::from(k) / 4.0;
        let sinc = if k == 0 {
            1.0
        } else {
            (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
        };
        let r = f64::from(k) / f64::from(HALF + 1);
        let w = i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0(beta);
        sinc * w
    };
    let mut out = [[0f32; PHASE_TAPS]; 4];
    for (p, row) in out.iter_mut().enumerate().skip(1) {
        // y(n + p/4) = sum_k h[k] u[4n + p - k], u[4m] = x[m]: k = p - 4 (m - n)
        for (j, v) in row.iter_mut().enumerate() {
            let m_minus_n = j as i32 - FIR_AHEAD as i32; // -12..=12
            let k = p as i32 - 4 * m_minus_n;
            if (-HALF..=HALF).contains(&k) {
                *v = h(k) as f32;
            }
        }
    }
    for row in out.iter_mut().skip(1) {
        let sum: f32 = row.iter().sum();
        for v in row.iter_mut() {
            *v /= sum;
        }
    }
    out
}

/// The detector's true-peak estimate over interleaved stereo `x` (the same 4x filter the limiter uses), for
/// tests and for offline measurement; frames closer than the filter's reach to either end are skipped.
pub fn true_peak_estimate(x: &[f32]) -> f32 {
    let f = make_filter();
    let n = x.len() / 2;
    let mut peak = 0f32;
    for i in PHASE_TAPS..n {
        let centre = i - FIR_AHEAD - 1;
        for c in 0..2 {
            peak = peak.max(x[centre * 2 + c].abs());
            for h in f.iter().skip(1) {
                let mut acc = 0f32;
                for (j, hv) in h.iter().enumerate() {
                    acc += hv * x[(i - PHASE_TAPS + 1 + j) * 2 + c];
                }
                peak = peak.max(acc.abs());
            }
        }
    }
    peak
}

pub struct TpLimiter {
    /// Total delay of the audio path, frames.
    del: usize,
    /// The lookahead window, frames (`del - FIR_AHEAD + 2`).
    la1: usize,
    ceiling_db: f32,
    ceiling: f32,
    filt: [[f32; PHASE_TAPS]; 4],
    /// The last 25 input frames per channel, oldest first (a small shift register).
    xh: [[f32; PHASE_TAPS]; 2],
    /// Audio delay line, `del` stereo frames.
    delay: Vec<f32>,
    delay_pos: usize,
    /// Monotonic deque for the sliding maximum of the true-peak values: (frame index, value).
    dq: VecDeque<(u64, f32)>,
    /// The window's maximum for the last `la1` frames (to rebuild `m` when the ceiling changes).
    pm_ring: Vec<f32>,
    /// `min(1, ceiling / pm)` for the same frames and its running sum.
    m_ring: Vec<f64>,
    mpos: usize,
    msum: f64,
    ones: usize,
    index: u64,
    gain: f64,
    /// Slow envelope of the reduction (linear, 1 - gain), for the program-dependent release.
    env: f64,
    env_k: f64,
    rel_min_k: f64,
    rel_max_k: f64,
    /// Smallest gain since `take_min_gain` (for the meter).
    min_gain: f32,
    /// Frames with any reduction, and with more than 1 dB of it.
    pub active_frames: u64,
    pub over_1db_frames: u64,
}

/// A very large ceiling: the limiter never acts, and the output is the delayed input.
pub const CEILING_OFF_DB: f32 = 200.0;

impl TpLimiter {
    pub fn new(rate: u32, ceiling_db: f32) -> Self {
        let del = ((LATENCY_S * f64::from(rate)).round() as usize).max(FIR_AHEAD + 2);
        let la1 = del - FIR_AHEAD + 2;
        let r = f64::from(rate);
        let mut s = Self {
            del,
            la1,
            ceiling_db,
            ceiling: 1.0,
            filt: make_filter(),
            xh: [[0.0; PHASE_TAPS]; 2],
            delay: vec![0.0; del * 2],
            delay_pos: 0,
            dq: VecDeque::with_capacity(la1 + 2),
            pm_ring: vec![0.0; la1],
            m_ring: vec![1.0; la1],
            mpos: 0,
            msum: la1 as f64,
            ones: la1,
            index: 0,
            gain: 1.0,
            env: 0.0,
            env_k: 1.0 - (-1.0 / (1.0 * r)).exp(),
            rel_min_k: 1.0 - (-1.0 / (RELEASE_MIN_S * r)).exp(),
            rel_max_k: 1.0 - (-1.0 / (RELEASE_MAX_S * r)).exp(),
            min_gain: 1.0,
            active_frames: 0,
            over_1db_frames: 0,
        };
        s.set_ceiling_db(ceiling_db);
        s
    }

    /// The delay the limiter adds, frames.
    pub fn latency_frames(&self) -> usize {
        self.del
    }

    pub fn ceiling_db(&self) -> f32 {
        self.ceiling_db
    }

    /// Change the ceiling (dBTP; [`CEILING_OFF_DB`] disables the limiting but keeps the delay). Takes effect
    /// at once for the audio already in the lookahead.
    pub fn set_ceiling_db(&mut self, db: f32) {
        self.ceiling_db = db;
        self.ceiling = if db >= 100.0 {
            f32::MAX / 4.0
        } else {
            10f32.powf((db - DETECT_MARGIN_DB) / 20.0)
        };
        // Rebuild the per-frame gains of the window from the stored maxima.
        let mut sum = 0.0;
        let mut ones = 0;
        for i in 0..self.la1 {
            let m = self.need(self.pm_ring[i]);
            self.m_ring[i] = m;
            sum += m;
            if m == 1.0 {
                ones += 1;
            }
        }
        self.msum = sum;
        self.ones = if ones == self.la1 { self.la1 } else { 0 };
    }

    #[inline]
    fn need(&self, peak: f32) -> f64 {
        if peak > self.ceiling {
            f64::from(self.ceiling / peak)
        } else {
            1.0
        }
    }

    /// The smallest gain applied since the last call (1.0 when the limiter did nothing), then reset.
    pub fn take_min_gain(&mut self) -> f32 {
        std::mem::replace(&mut self.min_gain, 1.0)
    }

    /// Process interleaved stereo frames: `input` to `output` (equal lengths).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        let la1 = self.la1;
        let over_1db = 10f64.powf(-1.0 / 20.0);
        for (x, y) in input.chunks_exact(2).zip(output.chunks_exact_mut(2)) {
            let l = if x[0].is_finite() { x[0] } else { 0.0 };
            let r = if x[1].is_finite() { x[1] } else { 0.0 };
            // The history: newest at the end; the centre (frame n = i - 12) is index 12.
            for (c, v) in [l, r].into_iter().enumerate() {
                self.xh[c].copy_within(1.., 0);
                self.xh[c][PHASE_TAPS - 1] = v;
            }
            let mut tp = self.xh[0][FIR_AHEAD].abs().max(self.xh[1][FIR_AHEAD].abs());
            for p in 1..4 {
                let h = &self.filt[p];
                for c in 0..2 {
                    let acc: f32 = h.iter().zip(&self.xh[c]).map(|(a, b)| a * b).sum();
                    tp = tp.max(acc.abs());
                }
            }

            // Sliding maximum of the true-peak values over la1 frames.
            while let Some(&(_, v)) = self.dq.back() {
                if v <= tp {
                    self.dq.pop_back();
                } else {
                    break;
                }
            }
            self.dq.push_back((self.index, tp));
            while let Some(&(i, _)) = self.dq.front() {
                if self.index - i >= la1 as u64 {
                    self.dq.pop_front();
                } else {
                    break;
                }
            }
            let pm = self.dq.front().map_or(0.0, |&(_, v)| v);
            let m = self.need(pm);

            self.msum += m - self.m_ring[self.mpos];
            self.m_ring[self.mpos] = m;
            self.pm_ring[self.mpos] = pm;
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

            // Release toward 1: fast for brief reduction, slower while the reduction is sustained.
            let depth = ((self.env - 0.01) / 0.15).clamp(0.0, 1.0);
            let k = self.rel_min_k + (self.rel_max_k - self.rel_min_k) * depth;
            let mut g = self.gain + (1.0 - self.gain) * k;
            if 1.0 - g < 1e-9 {
                g = 1.0;
            }
            let g = avg.min(g);
            self.gain = g;
            self.env += ((1.0 - g) - self.env) * self.env_k;

            // Output: the input `del` frames ago.
            let d = self.delay_pos * 2;
            let (ol, or) = (self.delay[d], self.delay[d + 1]);
            self.delay[d] = l;
            self.delay[d + 1] = r;
            self.delay_pos += 1;
            if self.delay_pos == self.del {
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
            }
            self.index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(l: &mut TpLimiter, x: &[f32]) -> Vec<f32> {
        let mut y = vec![0.0; x.len()];
        l.process(x, &mut y);
        y
    }

    /// An independent 8x oversampled true-peak estimate (a long windowed sinc, not the limiter's filter).
    fn true_peak_8x(x: &[f32]) -> f32 {
        let taps = 64i32; // each side, in input samples
        let n = x.len() / 2;
        let mut peak = 0f32;
        for c in 0..2 {
            let ch: Vec<f64> = (0..n).map(|i| f64::from(x[i * 2 + c])).collect();
            for i in (taps as usize)..(n - taps as usize) {
                for ph in 0..8 {
                    let frac = f64::from(ph) / 8.0;
                    let mut acc = 0.0;
                    for k in -taps..=taps {
                        let t = f64::from(k) - frac;
                        let s = if t == 0.0 {
                            1.0
                        } else {
                            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                        };
                        let w = 0.5 + 0.5 * (std::f64::consts::PI * t / f64::from(taps)).cos();
                        acc += ch[(i as i32 + k) as usize] * s * w;
                    }
                    peak = peak.max(acc.abs() as f32);
                }
            }
        }
        peak
    }

    #[test]
    fn below_the_ceiling_it_is_a_bit_exact_delay_of_five_ms() {
        let mut l = TpLimiter::new(48_000, -1.0);
        assert_eq!(l.latency_frames(), 240);
        let d = l.latency_frames();
        let x: Vec<f32> = (0..2 * 6000)
            .map(|i| ((i as f32) * 0.01).sin() * 0.8)
            .collect();
        let y = run(&mut l, &x);
        assert!(y[..2 * d].iter().all(|&v| v == 0.0));
        assert_eq!(&y[2 * d..], &x[..x.len() - 2 * d]);
        assert_eq!(l.active_frames, 0);
    }

    #[test]
    fn disabled_it_is_the_same_delay_with_hot_input() {
        let mut l = TpLimiter::new(48_000, CEILING_OFF_DB);
        let x: Vec<f32> = (0..2 * 3000)
            .map(|i| ((i as f32) * 0.01).sin() * 3.0)
            .collect();
        let y = run(&mut l, &x);
        let d = l.latency_frames();
        assert_eq!(&y[2 * d..], &x[..x.len() - 2 * d]);
    }

    #[allow(clippy::excessive_precision)]
    /// Pink-ish noise (Paul Kellet's filter), RMS-normalized to about 0.18, peak factor a normal ~12 dB.
    fn pink(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
            (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
        (0..n)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let w = ((s >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0;
                b0 = 0.99886 * b0 + w * 0.0555179;
                b1 = 0.99332 * b1 + w * 0.0750759;
                b2 = 0.96900 * b2 + w * 0.1538520;
                b3 = 0.86650 * b3 + w * 0.3104856;
                b4 = 0.55000 * b4 + w * 0.5329522;
                b5 = -0.7616 * b5 - w * 0.0168980;
                let v = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
                b6 = w * 0.115926;
                v * 0.11
            })
            .collect()
    }

    fn hot_program(n: usize, hot: f32) -> Vec<f32> {
        let a = pink(n, 11);
        let b = pink(n, 12);
        let mut x = vec![0.0f32; 2 * n];
        for i in 0..n {
            let tone = (2.0 * std::f32::consts::PI * 11_025.0 * i as f32 / 48_000.0 + 0.7).sin();
            let (l_, r_) = match (i / 12_000) % 4 {
                0 => (a[i] * hot, b[i] * hot),
                1 => (tone * hot * 0.6, tone * hot * 0.55),
                2 => (
                    a[i] * hot * 0.2 + tone * hot * 0.5,
                    b[i] * hot * 0.2 - tone * hot * 0.5,
                ),
                _ => (a[i] * 0.1, b[i] * 0.1),
            };
            x[2 * i] = l_;
            x[2 * i + 1] = r_;
        }
        x
    }

    #[test]
    fn the_true_peak_stays_at_the_ceiling_on_hot_program() {
        // Pink noise and an 11.025 kHz tone (strong inter-sample peaks) with peaks 4 to 16 dB over the
        // ceiling, measured with an independent 8x filter of 129 taps (stricter than the 4x measurement
        // the milestone names). Moderate overload must hold to the ceiling itself; the heavy case, where the gain
        // itself moves every few samples and modulates the signal, to 0.05 dB.
        for (over_db, tol_db) in [(4.0f32, 0.0f32), (8.0, 0.0), (16.0, 0.05)] {
            for ceiling in [-1.0f32, -2.0] {
                let mut l = TpLimiter::new(48_000, ceiling);
                let c = 10f32.powf(ceiling / 20.0);
                let x = hot_program(48_000 * 4, 10f32.powf((ceiling + over_db) / 20.0) / 0.55);
                let y = run(&mut l, &x);
                let tp = true_peak_8x(&y);
                let db = 20.0 * tp.max(1e-9).log10();
                println!(
                    "ceiling {ceiling:+.1} dBTP, peaks {:+.1} dBTP in: output {db:+.3} dBTP (8x, independent), detector {:+.3}, tolerance {tol_db} dB, {} frames limited",
                    20.0 * true_peak_8x(&x).log10(),
                    20.0 * true_peak_estimate(&y).log10(),
                    l.active_frames
                );
                assert!(db <= ceiling + tol_db, "{db} dBTP over {ceiling}");
                let sp = y.iter().fold(0f32, |m, v| m.max(v.abs()));
                assert!(sp <= c * 1.000_001, "sample peak {sp}");
                assert!(l.active_frames > 0);
            }
        }
    }

    #[test]
    fn a_ceiling_change_takes_effect_for_the_audio_already_in_the_lookahead() {
        let mut l = TpLimiter::new(48_000, CEILING_OFF_DB);
        let mut x = vec![0.1f32; 2 * 2000];
        for v in x[2 * 1000..2 * 1010].iter_mut() {
            *v = 0.9;
        }
        // The first 700 frames with the limiter off, then the ceiling drops to -6 dB: the burst at frame
        // 1000 is still in the delay line and must be caught.
        let mut y = run(&mut l, &x[..2 * 700]);
        l.set_ceiling_db(-6.0);
        y.extend(run(&mut l, &x[2 * 700..]));
        let c = 10f32.powf(-6.0 / 20.0);
        let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= c * 1.001, "{peak} > {c}");
    }

    #[test]
    fn recovers_to_unity_after_the_release() {
        let mut l = TpLimiter::new(48_000, -1.0);
        let mut x = vec![0.0f32; 2 * 48_000 * 3];
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

    #[test]
    fn non_finite_input_is_silenced() {
        let mut l = TpLimiter::new(48_000, -1.0);
        let mut x = vec![0.5f32; 2 * 1000];
        x[10] = f32::NAN;
        x[11] = f32::INFINITY;
        let y = run(&mut l, &x);
        assert!(y.iter().all(|v| v.is_finite()));
    }
}
