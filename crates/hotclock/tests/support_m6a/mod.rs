//! Shared helpers for the M6a acceptance tests: the controller rig (the meter thread's loop body, run
//! deterministically on the real taps), a streaming `ffmpeg` loudness measurement, synthetic program,
//! and the `lufs_mix` item list built from the test sets.
#![allow(dead_code)]

use engine::loudness::{LoudnessLink, Params};
use engine::mixer::{BlockInfo, MeterTap};
use serve::meters::Meter;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

pub const RATE: u32 = 48_000;

// ---- the rig: what the meter thread does with the taps ---------------------------------------------------

pub struct Rig {
    pub meter: Meter,
    pub link: Arc<LoudnessLink>,
    post: Vec<f32>,
    loud: Vec<f32>,
    zeros: Vec<f32>,
    infos: Vec<BlockInfo>,
    /// Frames of audio the controller has seen.
    pub frames: u64,
    /// Meter passes (each covers the blocks rendered since the last) and how many of them saw the true-peak
    /// limiter reduce the gain, and the deepest reduction (linear gain).
    pub tp_passes: u64,
    pub tp_limited: u64,
    pub tp_worst: f32,
    /// Passes whose reduction exceeded 0.5, 1 and 3 dB.
    pub tp_over: [u64; 3],
    pub tp_worst_at: u64,
}

impl Rig {
    pub fn new(params: Params, link: Arc<LoudnessLink>, initial_gain_db: f64) -> Rig {
        let mut meter = Meter::new(RATE, params.window_s);
        link.enabled
            .store(params.enabled, std::sync::atomic::Ordering::Relaxed);
        link.set_ceiling_db(params.true_peak_dbtp);
        link.set_gain_db(if params.enabled { initial_gain_db } else { 0.0 });
        meter.set_link(link.clone());
        meter.ctl.set_params(params);
        meter.ctl.set_gain_db(initial_gain_db);
        Rig {
            meter,
            link,
            post: vec![],
            loud: vec![],
            zeros: vec![],
            infos: vec![],
            frames: 0,
            tp_passes: 0,
            tp_limited: 0,
            tp_worst: 1.0,
            tp_over: [0; 3],
            tp_worst_at: 0,
        }
    }

    /// One pass of the meter thread's loop over whatever the taps hold.
    pub fn pump(&mut self, tap: &mut MeterTap) {
        let ni = tap.info.slots();
        if ni == 0 {
            return;
        }
        self.infos.clear();
        if let Ok(c) = tap.info.read_chunk(ni) {
            let (a, b) = c.as_slices();
            self.infos.extend_from_slice(a);
            self.infos.extend_from_slice(b);
            c.commit_all();
        }
        let frames: usize = self.infos.iter().map(|i| i.frames as usize).sum();
        let loud_frames: usize = self
            .infos
            .iter()
            .filter(|i| i.flags & BlockInfo::HAS_LOUD != 0)
            .map(|i| i.frames as usize)
            .sum();
        self.post.clear();
        if let Ok(c) = tap.post.read_chunk(frames * 2) {
            let (a, b) = c.as_slices();
            self.post.extend_from_slice(a);
            self.post.extend_from_slice(b);
            c.commit_all();
        }
        self.loud.clear();
        if loud_frames > 0 {
            if let Ok(c) = tap.loud.read_chunk(loud_frames * 2) {
                let (a, b) = c.as_slices();
                self.loud.extend_from_slice(a);
                self.loud.extend_from_slice(b);
                c.commit_all();
            }
        }
        // The pre tap is not read here: drain it so it cannot fill (the mixer only copies it when wanted).
        if self.zeros.len() < self.post.len() {
            self.zeros.resize(self.post.len(), 0.0);
        }
        let tpg = f32::from_bits(
            self.link
                .tp_min_gain_bits
                .swap(1.0f32.to_bits(), std::sync::atomic::Ordering::Relaxed),
        );
        self.meter.set_tp_gain(tpg.min(1.0));
        self.tp_passes += 1;
        if tpg < 1.0 {
            self.tp_limited += 1;
            if tpg < self.tp_worst {
                self.tp_worst_at = self.frames;
                if std::env::var_os("HOTCLOCK_M6A_TRACE").is_some() && tpg < 0.5 {
                    println!(
                        "  limiter reduction {:.2} dB at {:.1} s",
                        -20.0 * f64::from(tpg).log10(),
                        self.frames as f64 / 48000.0
                    );
                }
            }
            self.tp_worst = self.tp_worst.min(tpg);
            let db = -20.0 * f64::from(tpg).log10();
            for (i, t) in [0.5, 1.0, 3.0].iter().enumerate() {
                if db > *t {
                    self.tp_over[i] += 1;
                }
            }
        }
        self.meter.push_with(
            &self.post,
            &self.zeros[..self.post.len()],
            Some(&self.loud),
            &self.infos,
            false,
            &mut |_| {},
        );
        self.frames += frames as u64;
    }

    pub fn gain_db(&self) -> f64 {
        self.meter.ctl.gain_db()
    }
}

// ---- ffmpeg: an independent BS.1770 measurement of a stream ------------------------------------------------

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A running `ffmpeg -af ebur128=peak=true` reading interleaved stereo f32 on stdin.
pub struct Ebu {
    child: Child,
    err: Option<std::thread::JoinHandle<String>>,
}

#[derive(Debug, Clone, Copy)]
pub struct EbuResult {
    pub integrated: f64,
    /// True peak, dBTP (ffmpeg calls it "Peak: dBFS" under "True peak").
    pub true_peak: f64,
}

impl Ebu {
    pub fn spawn() -> Ebu {
        let mut child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "info",
                "-f",
                "f32le",
                "-ar",
                &RATE.to_string(),
                "-ac",
                "2",
                "-i",
                "-",
                "-af",
                "ebur128=peak=true:framelog=-8",
                "-f",
                "null",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ffmpeg");
        let mut stderr = child.stderr.take().unwrap();
        let err = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
            s
        });
        Ebu {
            child,
            err: Some(err),
        }
    }

    pub fn write(&mut self, samples: &[f32]) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            stdin.write_all(&bytes).expect("ffmpeg stdin");
        }
    }

    pub fn finish(mut self) -> EbuResult {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
        let text = self.err.take().unwrap().join().unwrap();
        parse_summary(&text).unwrap_or_else(|| panic!("no ebur128 summary in:\n{text}"))
    }
}

fn parse_summary(text: &str) -> Option<EbuResult> {
    let sum = &text[text.rfind("Summary:")?..];
    let num = |after: &str, key: &str| -> Option<f64> {
        let a = sum.find(after)?;
        let rest = &sum[a..];
        let k = rest.find(key)?;
        rest[k + key.len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some(EbuResult {
        integrated: num("Integrated loudness:", "I:")?,
        true_peak: num("True peak:", "Peak:")?,
    })
}

/// Measure a whole stereo buffer with ffmpeg.
pub fn measure(samples: &[f32]) -> EbuResult {
    let mut e = Ebu::spawn();
    for c in samples.chunks(1 << 16) {
        e.write(c);
    }
    e.finish()
}

/// ffmpeg's short-term loudness (3 s window) as it reports it every 100 ms over `samples`: the last values,
/// oldest first.
pub fn short_term_tail(samples: &[f32], n: usize) -> Vec<f64> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-nostats",
            "-loglevel",
            "info",
            "-f",
            "f32le",
            "-ar",
            &RATE.to_string(),
            "-ac",
            "2",
            "-i",
            "-",
            "-af",
            "ebur128",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    let mut stderr = child.stderr.take().unwrap();
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
        s
    });
    {
        let stdin = child.stdin.as_mut().unwrap();
        for c in samples.chunks(1 << 16) {
            let mut b = Vec::with_capacity(c.len() * 4);
            for v in c {
                b.extend_from_slice(&v.to_le_bytes());
            }
            if stdin.write_all(&b).is_err() {
                break;
            }
        }
    }
    drop(child.stdin.take());
    let _ = child.wait();
    let text = err.join().unwrap();
    let mut v: Vec<f64> = text
        .lines()
        .filter(|l| l.contains(" S:") && l.contains("TARGET"))
        .filter_map(|l| {
            let i = l.find(" S:")? + 3;
            l[i..].split_whitespace().next()?.parse().ok()
        })
        .collect();
    let k = v.len().saturating_sub(n);
    v.drain(..k);
    v
}

// ---- synthetic program -------------------------------------------------------------------------------------

#[allow(clippy::excessive_precision)]
/// Pink-ish noise (Paul Kellet's filter), stereo, scaled so its BS.1770 loudness is `lufs`, with a slow
/// 0.3 Hz amplitude wobble of +-1.5 dB so that it behaves like program rather than a stationary tone.
pub fn program(seconds: f64, lufs: f64, seed: u64) -> Vec<f32> {
    let n = (seconds * f64::from(RATE)) as usize;
    let mut out = vec![0.0f32; n * 2];
    let mut s = seed | 1;
    for ch in 0..2 {
        let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
            (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
        for i in 0..n {
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
            let v = (b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362) * 0.11;
            b6 = w * 0.115926;
            let wob = 10f32.powf(
                1.5 * (2.0 * std::f32::consts::PI * 0.3 * i as f32 / RATE as f32).sin() / 20.0,
            );
            out[i * 2 + ch] = v * wob;
        }
    }
    // Scale to the wanted loudness (measured by the same K-weighted blocks as the controller).
    let mut m = engine::loudness::BlockMeter::new(RATE, 3600.0);
    m.push(&out, true, true, 0);
    let l = m.gated().loudness.expect("program is audible");
    let g = 10f32.powf(((lufs - l) / 20.0) as f32);
    for v in out.iter_mut() {
        *v *= g;
    }
    out
}

/// Scale `x` by `db`.
pub fn scaled(x: &[f32], db: f64) -> Vec<f32> {
    let g = 10f32.powf((db / 20.0) as f32);
    x.iter().map(|v| v * g).collect()
}

/// A natural fade: `x` multiplied by a ramp that is linear in dB from 0 to -60 dB over its whole length
/// and then silent for `silence_s`.
pub fn faded(x: &[f32], silence_s: f64) -> Vec<f32> {
    let n = x.len() / 2;
    let mut out = Vec::with_capacity(x.len() + (silence_s * f64::from(RATE)) as usize * 2);
    for i in 0..n {
        let g = 10f32.powf(-60.0 * (i as f32 / n as f32) / 20.0);
        out.push(x[2 * i] * g);
        out.push(x[2 * i + 1] * g);
    }
    out.resize(out.len() + (silence_s * f64::from(RATE)) as usize * 2, 0.0);
    out
}

// ---- the lufs_mix item list ------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct MixItem {
    pub path: PathBuf,
    pub l_int: f64,
    pub true_peak: f64,
    pub seconds: f64,
}

/// The real music of the test sets (`office`, `preshow`, `elevator`) with each file's integrated loudness
/// and true peak, from a manifest cached in `<testdata>/lufs_mix/manifest.json` (made on first use).
pub fn lufs_mix() -> Option<Vec<MixItem>> {
    let root = PathBuf::from(std::env::var("HOTCLOCK_TESTDATA").ok()?);
    let dir = root.join("lufs_mix");
    let manifest = dir.join("manifest.json");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(&text) {
            let items: Vec<MixItem> = v
                .iter()
                .filter_map(|e| {
                    Some(MixItem {
                        path: PathBuf::from(e["path"].as_str()?),
                        l_int: e["l_int"].as_f64()?,
                        true_peak: e["true_peak"].as_f64()?,
                        seconds: e["seconds"].as_f64()?,
                    })
                })
                .collect();
            if !items.is_empty() {
                return Some(items);
            }
        }
    }
    let mut files = vec![];
    for set in ["office", "preshow", "elevator"] {
        let d = root.join(set);
        if d.is_dir() {
            files.extend(analysis::census::collect_files(&d).ok()?);
        }
    }
    files.sort();
    let mut items = vec![];
    for f in &files {
        match analysis::analyze::analyze_file(f, &analysis::markers::ProfileParams::music()) {
            Ok(a) if a.l_int.is_finite() && a.l_int > -50.0 && a.file_ms > 20_000 => {
                items.push(MixItem {
                    path: f.clone(),
                    l_int: a.l_int,
                    true_peak: a.true_peak,
                    seconds: a.file_ms as f64 / 1000.0,
                })
            }
            Ok(_) => {}
            Err(e) => eprintln!("lufs_mix: {} skipped: {e}", f.display()),
        }
    }
    std::fs::create_dir_all(&dir).ok()?;
    let json: Vec<serde_json::Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({"path": i.path, "l_int": i.l_int, "true_peak": i.true_peak, "seconds": i.seconds})
        })
        .collect();
    std::fs::write(&manifest, serde_json::to_vec_pretty(&json).ok()?).ok()?;
    Some(items)
}

/// A deterministic pseudo-random permutation, repeated until `seconds` of program are covered.
pub fn random_order(items: &[MixItem], seconds: f64, seed: u64) -> Vec<usize> {
    let mut s = seed | 1;
    let mut next = move |m: usize| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s % m as u64) as usize
    };
    let mut order = vec![];
    let mut total = 0.0;
    while total < seconds {
        let mut p: Vec<usize> = (0..items.len()).collect();
        for i in (1..p.len()).rev() {
            p.swap(i, next(i + 1));
        }
        for i in p {
            order.push(i);
            total += items[i].seconds;
            if total >= seconds {
                break;
            }
        }
    }
    order
}

pub fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hotclock-m6a-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn _unused(_: &Path) {}
