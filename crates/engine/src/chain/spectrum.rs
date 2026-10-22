//! The equalizer's spectrum analyzer (Processing page, Equalizer card): a live spectrum of the signal entering the
//! equalizer or leaving it, drawn behind the response curve.
//!
//! Three parts, kept apart so the audio thread does almost nothing:
//!
//! * [`SpecTaps`] lives in the chain's meters. While a tap is switched on (and only then) the chain copies the
//!   mono mix `(L + R) / 2` of that point into a ring of atomics: no lock, no allocation, one relaxed store per
//!   sample. With no tap switched on the chain does not touch the rings.
//! * [`Analyzer`] turns the latest [`FFT_N`] samples into [`BINS`] log-spaced bands (1/12 octave, 20 Hz upward) in
//!   dBFS: a Hann window, a radix-2 FFT, the power of each FFT bin shared out over the bands, and a smoother with a fast
//!   attack and a 300 ms release.
//! * [`SpectrumWorker`] is what a thread of the web server runs: it switches the taps on for the clients that ask
//!   for them (and off again when the last one goes), and builds one wire frame per tap per tick.
//!
//! Wire frame (binary, one WebSocket message): the bytes `EQS1`, the tap (0 in, 1 out), the band count, then one byte
//! per band: `(dBFS + 120) * 2`, 0 meaning -120 dBFS or less.

use super::ChainMeters;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::sync::Arc;
use std::time::Instant;

/// Samples kept per tap (a power of two).
pub const RING: usize = 1 << 15;
/// The FFT length: about 170 ms at 48 kHz, 5.9 Hz per bin.
pub const FFT_N: usize = 8192;
/// Bands: 20 Hz to 20 kHz in 1/12 octave steps.
pub const BINS: usize = 120;
pub const F_LO_HZ: f64 = 20.0;
/// The tap bits of [`SpecTaps::mode`].
pub const TAP_IN: u32 = 1;
pub const TAP_OUT: u32 = 2;
pub const MAGIC: [u8; 4] = *b"EQS1";

/// One tap: the last [`RING`] samples and how many were ever written.
pub struct Ring {
    buf: Box<[AtomicU32]>,
    pos: AtomicU64,
}

impl Default for Ring {
    fn default() -> Self {
        Self {
            buf: (0..RING).map(|_| AtomicU32::new(0)).collect(),
            pos: AtomicU64::new(0),
        }
    }
}

impl Ring {
    /// Audio thread: append the mono mix of interleaved stereo `io`. One writer only.
    pub fn push_stereo(&self, io: &[f32]) {
        let p = self.pos.load(Relaxed);
        let mut n = 0u64;
        for fr in io.chunks_exact(2) {
            let m = (fr[0] + fr[1]) * 0.5;
            self.buf[((p + n) as usize) & (RING - 1)].store(m.to_bits(), Relaxed);
            n += 1;
        }
        self.pos.store(p + n, Release);
    }

    /// Samples ever written.
    pub fn written(&self) -> u64 {
        self.pos.load(Acquire)
    }

    /// Copy the latest `out.len()` samples (at most [`RING`] / 2). False when fewer were written, or the writer
    /// lapped the copy (the caller tries again at its next tick).
    pub fn latest(&self, out: &mut [f32]) -> bool {
        let n = out.len() as u64;
        let p1 = self.pos.load(Acquire);
        if p1 < n {
            return false;
        }
        let start = p1 - n;
        for (i, o) in out.iter_mut().enumerate() {
            *o = f32::from_bits(self.buf[((start + i as u64) as usize) & (RING - 1)].load(Relaxed));
        }
        std::sync::atomic::fence(Acquire);
        let p2 = self.pos.load(Acquire);
        p2 - start <= RING as u64
    }
}

/// The two taps and the switch the chain reads.
#[derive(Default)]
pub struct SpecTaps {
    mode: AtomicU32,
    rings: [Ring; 2],
}

impl SpecTaps {
    /// Which taps the chain should fill: [`TAP_IN`] and/or [`TAP_OUT`]; 0 when nobody is looking.
    pub fn mode(&self) -> u32 {
        self.mode.load(Relaxed)
    }
    pub fn set_mode(&self, m: u32) {
        self.mode.store(m & 3, Relaxed);
    }
    pub fn ring(&self, tap: usize) -> &Ring {
        &self.rings[tap & 1]
    }
}

/// Centre of band `i`, Hz.
pub fn band_center_hz(i: usize) -> f64 {
    F_LO_HZ * 2f64.powf((i as f64 + 0.5) / 12.0)
}

/// FFT, bands and smoothing for one tap.
pub struct Analyzer {
    rate: f32,
    win: Vec<f32>,
    cos: Vec<f32>,
    sin: Vec<f32>,
    rev: Vec<u32>,
    re: Vec<f32>,
    im: Vec<f32>,
    power: Vec<f32>,
    /// The smoothed levels, dBFS.
    pub level: [f32; BINS],
    primed: bool,
}

impl Analyzer {
    pub fn new(rate: u32) -> Self {
        let n = FFT_N;
        let win: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32)
            .collect();
        let cos = (0..n / 2)
            .map(|i| (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos() as f32)
            .collect();
        let sin = (0..n / 2)
            .map(|i| (-2.0 * std::f64::consts::PI * i as f64 / n as f64).sin() as f32)
            .collect();
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        Self {
            rate: rate as f32,
            win,
            cos,
            sin,
            rev,
            re: vec![0.0; n],
            im: vec![0.0; n],
            power: vec![0.0; n / 2 + 1],
            level: [-120.0; BINS],
            primed: false,
        }
    }

    fn fft(&mut self) {
        let n = FFT_N;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                self.re.swap(i, j);
                self.im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = self.re[b] * wr - self.im[b] * wi;
                    let ti = self.re[b] * wi + self.im[b] * wr;
                    self.re[b] = self.re[a] - tr;
                    self.im[b] = self.im[a] - ti;
                    self.re[a] += tr;
                    self.im[a] += ti;
                }
            }
            len *= 2;
        }
    }

    /// The raw band levels (dBFS) of `samples` (exactly [`FFT_N`] long). A sine of amplitude A reads 20 log10 A.
    pub fn analyze(&mut self, samples: &[f32]) -> [f32; BINS] {
        debug_assert_eq!(samples.len(), FFT_N);
        for ((r, i), (x, w)) in self
            .re
            .iter_mut()
            .zip(self.im.iter_mut())
            .zip(samples.iter().zip(&self.win))
        {
            *r = x * w;
            *i = 0.0;
        }
        self.fft();
        // |X| scaled by 2 / sum(w) = 2 / (N / 2) so that a sine on a bin centre reads its amplitude; the Hann window
        // spreads a sine over three bins whose powers add up to 1.5 times its amplitude squared.
        let sc = 2.0 / (FFT_N as f32 / 2.0);
        for k in 0..=FFT_N / 2 {
            let (r, i) = (self.re[k] * sc, self.im[k] * sc);
            self.power[k] = (r * r + i * i) / 1.5;
        }
        let df = self.rate / FFT_N as f32;
        let nyq = self.rate / 2.0;
        let mut out = [-120.0f32; BINS];
        for (b, o) in out.iter_mut().enumerate() {
            let lo = (F_LO_HZ * 2f64.powf(b as f64 / 12.0)) as f32;
            let hi = (F_LO_HZ * 2f64.powf((b as f64 + 1.0) / 12.0)) as f32;
            if lo >= nyq {
                continue;
            }
            let (kl, kh) = (lo / df, hi.min(nyq) / df);
            // FFT bin j covers [j - 0.5, j + 0.5]: share its power out by overlap.
            let mut p = 0.0f32;
            let j0 = ((kl + 0.5).floor() as isize).max(0) as usize;
            let j1 = (((kh + 0.5).floor() as isize).max(0) as usize).min(FFT_N / 2);
            for j in j0..=j1 {
                let ov = (kh.min(j as f32 + 0.5) - kl.max(j as f32 - 0.5)).max(0.0);
                p += self.power[j] * ov;
            }
            *o = 10.0 * p.max(1e-12).log10();
        }
        out
    }

    /// Smooth towards `raw` over `dt` seconds: the attack takes 30 ms, the release 300 ms (time constants).
    pub fn smooth(&mut self, raw: &[f32; BINS], dt: f32) {
        if !self.primed {
            self.level = *raw;
            self.primed = true;
            return;
        }
        let up = 1.0 - (-dt / 0.03).exp();
        let down = 1.0 - (-dt / 0.3).exp();
        for (l, r) in self.level.iter_mut().zip(raw) {
            let c = if *r > *l { up } else { down };
            *l += (*r - *l) * c;
        }
    }

    /// The wire frame of the smoothed levels for `tap` (0 in, 1 out).
    pub fn frame(&self, tap: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(6 + BINS);
        v.extend_from_slice(&MAGIC);
        v.push(tap);
        v.push(BINS as u8);
        v.extend(
            self.level
                .iter()
                .map(|d| ((d + 120.0) * 2.0).round().clamp(0.0, 255.0) as u8),
        );
        v
    }
}

/// What the web server's analyzer thread runs, once per tick.
pub struct SpectrumWorker {
    taps: Arc<ChainMeters>,
    an: [Analyzer; 2],
    since: [Option<u64>; 2],
    last: [Option<Instant>; 2],
    last_pos: [u64; 2],
    buf: Vec<f32>,
    /// FFTs run so far (a test counts them to show that nothing runs with nobody looking).
    pub ffts: u64,
}

impl SpectrumWorker {
    pub fn new(meters: Arc<ChainMeters>, rate: u32) -> Self {
        Self {
            taps: meters,
            an: [Analyzer::new(rate), Analyzer::new(rate)],
            since: [None; 2],
            last: [None; 2],
            last_pos: [0; 2],
            buf: vec![0.0; FFT_N],
            ffts: 0,
        }
    }

    /// `want`: the taps (bits [`TAP_IN`], [`TAP_OUT`]) somebody is subscribed to. Switches the chain's taps to that,
    /// and for each wanted tap with enough fresh audio returns `(tap, frame)`. With `want == 0` it does nothing else.
    pub fn tick(&mut self, want: u32, now: Instant) -> Vec<(u8, Vec<u8>)> {
        if self.taps.spec.mode() != want {
            self.taps.spec.set_mode(want);
        }
        let mut out = Vec::new();
        for t in 0..2usize {
            if want >> t & 1 == 0 {
                self.since[t] = None;
                self.last[t] = None;
                self.an[t].primed = false;
                continue;
            }
            let ring = self.taps.spec.ring(t);
            let pos = ring.written();
            // A tap just switched on: its ring still holds the old audio. Wait for a full window of new audio.
            let since = *self.since[t].get_or_insert(pos);
            if pos < since + FFT_N as u64 || pos == self.last_pos[t] {
                continue;
            }
            if !ring.latest(&mut self.buf) {
                continue;
            }
            self.last_pos[t] = pos;
            let raw = self.an[t].analyze(&self.buf);
            self.ffts += 1;
            let dt = self.last[t].map_or(0.08, |l| {
                now.duration_since(l).as_secs_f32().clamp(0.01, 1.0)
            });
            self.last[t] = Some(now);
            self.an[t].smooth(&raw, dt);
            out.push((t as u8, self.an[t].frame(t as u8)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, amp: f64, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32
            })
            .collect()
    }

    fn band_of(hz: f64) -> usize {
        (12.0 * (hz / F_LO_HZ).log2()).floor() as usize
    }

    #[test]
    fn a_1k_sine_peaks_in_the_1k_band_at_its_level() {
        for rate in [44_100u32, 48_000] {
            let mut a = Analyzer::new(rate);
            let x = sine(1000.0, 0.5, rate, FFT_N);
            let raw = a.analyze(&x);
            let peak = (0..BINS)
                .max_by(|&i, &j| raw[i].total_cmp(&raw[j]))
                .unwrap();
            let want = band_of(1000.0);
            assert!(
                peak.abs_diff(want) <= 1,
                "rate {rate}: peak band {peak} ({:.0} Hz), want {want}",
                band_center_hz(peak)
            );
            assert!(
                (raw[peak] + 6.02).abs() < 1.0,
                "rate {rate}: level {} dBFS, want -6",
                raw[peak]
            );
            // far from the sine the floor is low
            assert!(raw[band_of(5000.0)] < -80.0, "{}", raw[band_of(5000.0)]);
        }
    }

    #[test]
    fn sines_across_the_range_peak_in_their_own_band() {
        let mut a = Analyzer::new(48_000);
        for hz in [31.5, 63.0, 125.0, 250.0, 2480.0, 8000.0, 16_000.0] {
            let raw = a.analyze(&sine(hz, 0.25, 48_000, FFT_N));
            let peak = (0..BINS)
                .max_by(|&i, &j| raw[i].total_cmp(&raw[j]))
                .unwrap();
            assert!(
                peak.abs_diff(band_of(hz)) <= 1,
                "{hz} Hz: peak band {peak}, want {}",
                band_of(hz)
            );
        }
    }

    #[test]
    fn the_smoother_attacks_fast_and_lets_go_slowly() {
        let mut a = Analyzer::new(48_000);
        let mut r = [-90.0f32; BINS];
        a.smooth(&r, 0.08);
        r[10] = -20.0;
        a.smooth(&r, 0.08);
        assert!(a.level[10] > -25.0, "attack {}", a.level[10]);
        r[10] = -90.0;
        a.smooth(&r, 0.08);
        assert!(a.level[10] > -45.0, "release is slow: {}", a.level[10]);
        for _ in 0..40 {
            a.smooth(&r, 0.08);
        }
        assert!(a.level[10] < -80.0);
    }

    #[test]
    fn nothing_runs_and_the_chain_writes_nothing_with_no_subscriber() {
        let meters = Arc::new(ChainMeters::default());
        let mut w = SpectrumWorker::new(meters.clone(), 48_000);
        // The chain runs with the taps off: the rings stay untouched.
        let mut chain = super::super::Chain::build(
            &common::chain::ChainConfig::default(),
            48_000,
            meters.clone(),
        );
        let mut io = vec![0.1f32; 2048];
        for _ in 0..20 {
            chain.process(&mut io, true);
            assert!(w.tick(0, Instant::now()).is_empty());
        }
        assert_eq!(w.ffts, 0);
        assert_eq!(meters.spec.mode(), 0);
        assert_eq!(
            meters.spec.ring(0).written() + meters.spec.ring(1).written(),
            0
        );
    }

    #[test]
    fn a_subscriber_gets_frames_after_a_full_window_and_the_peak_is_the_sine() {
        let meters = Arc::new(ChainMeters::default());
        let mut w = SpectrumWorker::new(meters.clone(), 48_000);
        let mut chain = super::super::Chain::build(
            &common::chain::ChainConfig::default(),
            48_000,
            meters.clone(),
        );
        let x = sine(1000.0, 0.5, 48_000, 48_000 * 3);
        let st: Vec<f32> = x.iter().flat_map(|s| [*s, *s]).collect();
        let mut frames = Vec::new();
        let t0 = Instant::now();
        for (n, blk) in st.chunks(2048).enumerate() {
            let mut io = blk.to_vec();
            chain.process(&mut io, true);
            for f in w.tick(
                TAP_OUT,
                t0 + std::time::Duration::from_millis(n as u64 * 43),
            ) {
                frames.push(f);
            }
            if n == 0 {
                assert!(frames.is_empty(), "no frame before a window of audio");
            }
        }
        assert_eq!(meters.spec.mode(), TAP_OUT);
        assert_eq!(
            meters.spec.ring(0).written(),
            0,
            "the in tap was not asked for"
        );
        let (tap, f) = frames.last().expect("frames");
        assert_eq!(*tap, 1);
        assert_eq!(&f[..4], &MAGIC);
        assert_eq!(f[5] as usize, BINS);
        let bins = &f[6..];
        let peak = (0..BINS).max_by_key(|&i| bins[i]).unwrap();
        assert!(peak.abs_diff(band_of(1000.0)) <= 1, "peak band {peak}");
        // then it is switched off again when nobody is left
        assert!(w.tick(0, Instant::now()).is_empty());
        assert_eq!(meters.spec.mode(), 0);
    }

    /// Cost of one analyzer frame and of the audio thread's copy (printed; run with --nocapture).
    #[test]
    fn cost_of_a_frame() {
        let mut a = Analyzer::new(48_000);
        let x = sine(1000.0, 0.5, 48_000, FFT_N);
        let t = Instant::now();
        let mut s = 0.0;
        for _ in 0..200 {
            s += a.analyze(&x)[67];
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / 200.0;
        let ring = Ring::default();
        let io = vec![0.1f32; 2048];
        let t = Instant::now();
        for _ in 0..2000 {
            ring.push_stereo(&io);
        }
        let push = t.elapsed().as_secs_f64() * 1e6 / 2000.0;
        println!("analyzer: {per:.3} ms per FFT frame ({s:.0}); audio-thread copy: {push:.2} us per 1024-frame block");
        assert!(per < 20.0);
    }
}
