//! Shared helpers for the M1.2 tests: fixtures, an offline renderer, and audio measurements.
#![allow(dead_code)]

use engine::output::{FileOutput, NullOutput};
use engine::player::{render_offline_with, PlayOptions, PlayReport};
use engine::wav::WavFormat;
use engine::HwRequest;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const RATE: u32 = 48_000;

/// Generate (once) and return the directory holding the named fixtures, or `None` (with a
/// message) when they cannot be made because `ffmpeg` is missing.
pub fn fixture_dir(names: &[&str]) -> Option<PathBuf> {
    let (dir, report) = fixtures::shared_dir(names);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED: fixtures not available: {:?}", report.skipped);
        return None;
    }
    Some(dir)
}

pub fn out_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-out");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Play `files` offline at 48 kHz into memory. Returns the interleaved stereo output (exactly
/// `end_frame` frames) and the report.
pub fn render_files(files: &[PathBuf]) -> (Vec<f32>, PlayReport) {
    let clock = Arc::new(common::VirtualClock::new(0));
    let mut out = NullOutput::new(&HwRequest::defaults(), clock);
    let mut samples: Vec<f32> = Vec::new();
    let report = render_offline_with(
        files.to_vec(),
        &mut out,
        &PlayOptions::default(),
        |_, block| samples.extend_from_slice(block),
    )
    .expect("render");
    (samples, report)
}

/// Offline render to a WAV file (for the CLI-like path).
pub fn render_to_wav(files: &[PathBuf], path: &Path, fmt: WavFormat) -> PlayReport {
    let mut out = FileOutput::create(path, &HwRequest::defaults(), fmt, None).unwrap();
    let r = render_offline_with(files.to_vec(), &mut out, &PlayOptions::default(), |_, _| {})
        .expect("render");
    use engine::Output;
    out.finish().unwrap();
    r
}

pub fn left(x: &[f32]) -> Vec<f32> {
    x.chunks_exact(2).map(|f| f[0]).collect()
}

pub fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * ms.log10()
}

/// Result of fitting a sine of unknown frequency to a signal.
#[derive(Debug, Clone, Copy)]
pub struct SineFit {
    pub freq_hz: f64,
    /// RMS of the fitted sine, dBFS.
    pub level_dbfs: f64,
    /// Everything that is not the fitted sine (harmonics, noise, aliasing, DC), relative to the
    /// fitted sine, in dB: THD+N over the full band.
    pub thdn_db: f64,
}

/// Least-squares fit of `a sin + b cos` at a frequency chosen by golden-section search around
/// `guess_hz` (within +-`span_hz`). `x` is one channel at `rate`.
pub fn fit_sine(x: &[f32], rate: u32, guess_hz: f64, span_hz: f64) -> SineFit {
    let n = x.len();
    let w = |f: f64| 2.0 * std::f64::consts::PI * f / f64::from(rate);
    // Residual energy and (a, b) for a given frequency.
    let eval = |f: f64| -> (f64, f64, f64) {
        let (mut ss, mut cc, mut sc, mut xs, mut xc, mut xx) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        let dw = w(f);
        // Rotate a phasor instead of calling sin/cos per sample; re-sync periodically.
        let (mut s, mut c) = (0.0f64, 1.0f64);
        let (ds, dc) = (dw.sin(), dw.cos());
        for (i, &v) in x.iter().enumerate() {
            if i % 4096 == 0 {
                let p = dw * i as f64;
                s = p.sin();
                c = p.cos();
            }
            let v = f64::from(v);
            ss += s * s;
            cc += c * c;
            sc += s * c;
            xs += v * s;
            xc += v * c;
            xx += v * v;
            let ns = s * dc + c * ds;
            let nc = c * dc - s * ds;
            s = ns;
            c = nc;
        }
        let det = ss * cc - sc * sc;
        let a = (xs * cc - xc * sc) / det;
        let b = (xc * ss - xs * sc) / det;
        let resid = xx - a * xs - b * xc;
        (resid, a, b)
    };
    let (mut lo, mut hi) = (guess_hz - span_hz, guess_hz + span_hz);
    let phi = (5f64.sqrt() - 1.0) / 2.0;
    let (mut c1, mut c2) = (hi - phi * (hi - lo), lo + phi * (hi - lo));
    let (mut f1, mut f2) = (eval(c1).0, eval(c2).0);
    for _ in 0..70 {
        if f1 < f2 {
            hi = c2;
            c2 = c1;
            f2 = f1;
            c1 = hi - phi * (hi - lo);
            f1 = eval(c1).0;
        } else {
            lo = c1;
            c1 = c2;
            f1 = f2;
            c2 = lo + phi * (hi - lo);
            f2 = eval(c2).0;
        }
    }
    let f = 0.5 * (lo + hi);
    let (resid, a, b) = eval(f);
    let sig_power = (a * a + b * b) / 2.0;
    let resid_power = (resid / n as f64).max(1e-30);
    SineFit {
        freq_hz: f,
        level_dbfs: 10.0 * sig_power.log10(),
        thdn_db: 10.0 * (resid_power / sig_power).log10(),
    }
}
