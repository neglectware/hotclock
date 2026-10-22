//! Deterministic synthetic signals. Everything here is generated from formulas and a seeded
//! generator, so the fixtures are reproducible and no real audio is involved.

use std::f64::consts::TAU;

/// xorshift64*, seeded per fixture.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [-1, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

pub fn db_to_amp(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Peak amplitude of a sine at the given RMS level.
pub fn sine_peak(rms_dbfs: f64) -> f64 {
    db_to_amp(rms_dbfs) * std::f64::consts::SQRT_2
}

pub fn rms(x: &[f64]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// Scale `x` in place so its RMS is `rms_dbfs`. Noise at loud levels would exceed full scale
/// (pink noise has a crest factor over 12 dB), so peaks above 0.7 are softly limited toward 0.98
/// and the level is re-fitted, a few times; the final RMS is within about 0.02 dB of the request
/// and no sample exceeds 0.98. (Clipped noise also makes `flacenc` write absurdly large files.)
pub fn set_rms(x: &mut [f64], rms_dbfs: f64) {
    for _ in 0..6 {
        let r = rms(x);
        if r <= 0.0 {
            return;
        }
        let g = db_to_amp(rms_dbfs) / r;
        x.iter_mut().for_each(|v| *v *= g);
        if x.iter().all(|v| v.abs() <= 0.98) {
            return;
        }
        soft_limit(x);
    }
    soft_limit(x);
}

fn soft_limit(x: &mut [f64]) {
    const KNEE: f64 = 0.7;
    const HEAD: f64 = 0.28;
    for v in x.iter_mut() {
        let a = v.abs();
        if a > KNEE {
            *v = v.signum() * (KNEE + HEAD * ((a - KNEE) / HEAD).tanh());
        }
    }
}

pub fn silence(rate: u32, seconds: f64) -> Vec<f64> {
    vec![0.0; (seconds * rate as f64).round() as usize]
}

pub fn sine(rate: u32, hz: f64, rms_dbfs: f64, seconds: f64) -> Vec<f64> {
    let n = (seconds * rate as f64).round() as usize;
    let a = sine_peak(rms_dbfs);
    (0..n)
        .map(|i| a * (TAU * hz * i as f64 / rate as f64).sin())
        .collect()
}

/// Pink noise (Paul Kellet's filter), scaled to exactly `rms_dbfs`.
pub fn pink(rate: u32, rms_dbfs: f64, seconds: f64, seed: u64) -> Vec<f64> {
    let n = (seconds * rate as f64).round() as usize;
    let mut rng = Rng::new(seed);
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let w = rng.uniform();
        b0 = 0.99886 * b0 + w * 0.0555179;
        b1 = 0.99332 * b1 + w * 0.0750759;
        b2 = 0.96900 * b2 + w * 0.1538520;
        b3 = 0.86650 * b3 + w * 0.3104856;
        b4 = 0.55000 * b4 + w * 0.5329522;
        b5 = -0.7616 * b5 - w * 0.0168980;
        out.push(b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362);
        b6 = w * 0.115926;
    }
    set_rms(&mut out, rms_dbfs);
    out
}

/// White noise band-limited to `[lo, hi]` Hz with cascaded one-pole filters (a fixture does not
/// need a sharp skirt), scaled to exactly `rms_dbfs`.
pub fn band_noise(rate: u32, lo: f64, hi: f64, rms_dbfs: f64, seconds: f64, seed: u64) -> Vec<f64> {
    let n = (seconds * rate as f64).round() as usize;
    let mut rng = Rng::new(seed);
    let a_hi = (-TAU * hi / rate as f64).exp();
    let a_lo = (-TAU * lo / rate as f64).exp();
    let (mut l1, mut l2, mut h1, mut h2) = (0.0, 0.0, 0.0, 0.0);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let w = rng.uniform();
        l1 = (1.0 - a_hi) * w + a_hi * l1; // low-pass at hi
        l2 = (1.0 - a_hi) * l1 + a_hi * l2;
        h1 = (1.0 - a_lo) * l2 + a_lo * h1; // slow low-pass, subtracted: high-pass at lo
        h2 = (1.0 - a_lo) * h1 + a_lo * h2;
        out.push(l2 - h2);
    }
    set_rms(&mut out, rms_dbfs);
    out
}

/// Multiply by a linear ramp from `from` to `to` (amplitude).
pub fn ramp(x: &mut [f64], from: f64, to: f64) {
    let n = x.len().max(2) as f64;
    for (i, v) in x.iter_mut().enumerate() {
        *v *= from + (to - from) * i as f64 / (n - 1.0);
    }
}

/// Multiply by an envelope that moves linearly in dB from `from_db` to `to_db`.
pub fn ramp_db(x: &mut [f64], from_db: f64, to_db: f64) {
    let n = x.len().max(2) as f64;
    for (i, v) in x.iter_mut().enumerate() {
        *v *= db_to_amp(from_db + (to_db - from_db) * i as f64 / (n - 1.0));
    }
}

pub fn concat(parts: &[Vec<f64>]) -> Vec<f64> {
    parts.iter().flatten().copied().collect()
}

pub fn add_at(dst: &mut [f64], at: usize, src: &[f64]) {
    for (i, v) in src.iter().enumerate() {
        if at + i < dst.len() {
            dst[at + i] += v;
        }
    }
}

/// A short metallic tick: inharmonic sines with a fast exponential decay, peaking at `peak_dbfs`.
pub fn tick(rate: u32, peak_dbfs: f64, seconds: f64) -> Vec<f64> {
    let n = (seconds * rate as f64).round() as usize;
    let partials = [2_317.0, 3_671.0, 5_479.0, 7_823.0];
    let mut out: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / rate as f64;
            let env = (-t / (seconds / 4.0)).exp();
            partials.iter().map(|f| (TAU * f * t).sin()).sum::<f64>() * env
        })
        .collect();
    let peak = out.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let g = db_to_amp(peak_dbfs) / peak;
    out.iter_mut().for_each(|v| *v *= g);
    out
}
