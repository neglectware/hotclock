//! The meter thread and the meter frame (architecture 1.2 and 11.3): PPM, sample peak, momentary,
//! short-term, window and integrated loudness, and the 64-band spectra of the two taps, packed into the
//! **276-byte v3 frame** at 30 Hz (220 bytes of the original v3 layout, then the per-block output peaks).
//!
//! Levels are `i16` in 0.01 dB, `-32768` meaning "no signal", "not yet valid", "no such band" or "stage
//! disabled and not measured". The stage meters, the per-band gain reduction, the stage bitmask and the flags
//! come from the processing chain's atomics (`engine::chain::ChainMeters`, milestone M6); a meter with no chain
//! attached reports a bypassed chain, as in M1.7.

use ebur128::{EbuR128, Mode};
use engine::loudness::{Controller, LoudnessLink, Params as LoudParams, Status as LoudStatus};
use engine::mixer::{next_pre_request, BlockInfo, MeterTap, PRE_MARKER_WORDS, PRE_SENTINEL};
use serde_json::{json, Value};
use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_LEN: usize = 276;
pub const FRAME_VERSION: u8 = 3;
pub const FRAME_RATE: u32 = 30;
pub const FFT_N: usize = 4096;
pub const BANDS: usize = 64;
pub const NONE: i16 = -32768;

// ---- the FFT: 4096 points, radix 2, two real signals in one transform -----------------------------------------

pub struct Fft {
    n: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
    rev: Vec<u16>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two() && n <= 65_536);
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| ((i as u32).reverse_bits() >> (32 - bits)) as u16)
            .collect();
        let cos = (0..n / 2)
            .map(|k| (-2.0 * std::f64::consts::PI * k as f64 / n as f64).cos() as f32)
            .collect();
        let sin = (0..n / 2)
            .map(|k| (-2.0 * std::f64::consts::PI * k as f64 / n as f64).sin() as f32)
            .collect();
        Fft { n, cos, sin, rev }
    }

    /// In-place forward transform of `re + j im`.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
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
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

/// The spectrum analyzer: Hann-windowed 4096-point FFT of two mid signals (post tap, pre tap) at once.
pub struct Spectra {
    fft: Fft,
    window: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// For each band: the bin range it sums, or the interpolation point when narrower than one bin.
    bands: Vec<BandSpec>,
    rate: u32,
}

#[derive(Clone, Copy)]
enum BandSpec {
    Sum(usize, usize),
    /// Interpolate at this fractional bin.
    Interp(f32),
}

/// The band edges: 64 log-spaced bands from 20 Hz to 20 kHz.
pub fn band_edges() -> [f64; BANDS + 1] {
    let mut e = [0.0; BANDS + 1];
    for (i, v) in e.iter_mut().enumerate() {
        *v = 20.0 * 1000f64.powf(i as f64 / BANDS as f64);
    }
    e
}

impl Spectra {
    pub fn new(rate: u32) -> Spectra {
        let window = (0..FFT_N)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / FFT_N as f32).cos())
            .collect();
        let bin_hz = f64::from(rate) / FFT_N as f64;
        let edges = band_edges();
        let bands = (0..BANDS)
            .map(|b| {
                let (lo, hi) = (edges[b], edges[b + 1]);
                let first = (lo / bin_hz).ceil() as usize;
                let last = ((hi / bin_hz).ceil() as usize).saturating_sub(1);
                if hi - lo < bin_hz || first > last {
                    BandSpec::Interp(((lo * hi).sqrt() / bin_hz) as f32)
                } else {
                    BandSpec::Sum(first, last.min(FFT_N / 2 - 1))
                }
            })
            .collect();
        Spectra {
            fft: Fft::new(FFT_N),
            window,
            re: vec![0.0; FFT_N],
            im: vec![0.0; FFT_N],
            bands,
            rate,
        }
    }

    /// `post` and `pre` are the last 4096 mid samples of each tap, oldest first. Writes 64 bytes each.
    pub fn analyze(&mut self, post: &[f32], pre: &[f32], out_post: &mut [u8], out_pre: &mut [u8]) {
        for i in 0..FFT_N {
            self.re[i] = post[i] * self.window[i];
            self.im[i] = pre[i] * self.window[i];
        }
        self.fft.forward(&mut self.re, &mut self.im);
        // Split the two real transforms: P[k] = (X[k] + conj X[N-k]) / 2, Q[k] = (X[k] - conj X[N-k]) / 2j.
        let n = FFT_N;
        let mut pp = vec![0f32; n / 2 + 1];
        let mut qq = vec![0f32; n / 2 + 1];
        for k in 0..=n / 2 {
            let (xr, xi) = (self.re[k], self.im[k]);
            let m = (n - k) % n;
            let (yr, yi) = (self.re[m], self.im[m]);
            let (pr, pi) = ((xr + yr) * 0.5, (xi - yi) * 0.5);
            let (qr, qi) = ((xi + yi) * 0.5, (yr - xr) * 0.5);
            pp[k] = pr * pr + pi * pi;
            qq[k] = qr * qr + qi * qi;
        }
        // A full-scale sine has |X| = N/4 at its bin (Hann sum N/2) and 1.5 bins of noise bandwidth.
        let peak_power = (n as f32 / 4.0).powi(2);
        let sum_norm = peak_power * 1.5;
        for (spec, out) in [(&pp, out_post), (&qq, out_pre)] {
            for (b, band) in self.bands.iter().enumerate() {
                let db = match *band {
                    BandSpec::Sum(a, z) => {
                        let s: f32 = spec[a..=z].iter().sum();
                        power_db(s / sum_norm)
                    }
                    BandSpec::Interp(x) => {
                        let i = (x.floor() as usize).min(n / 2 - 1);
                        let f = x - i as f32;
                        let (a, b2) = (
                            power_db(spec[i] / peak_power),
                            power_db(spec[i + 1] / peak_power),
                        );
                        a + (b2 - a) * f
                    }
                };
                out[b] = ((db + 90.0) * 255.0 / 90.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        let _ = self.rate;
    }
}

fn power_db(p: f32) -> f32 {
    if p <= 1e-20 {
        -200.0
    } else {
        10.0 * p.log10()
    }
}

// ---- PPM, IEC 60268-10 type I ballistics on 1 ms peak sub-blocks --------------------------------------------------

/// Quasi-peak: the peak of each millisecond, rising with a 5 ms time constant and returning 20 dB in 1.7 s.
struct Ppm {
    y: f32,
    attack: f32,
    release: f32,
    sub: usize,
    n: usize,
    peak: f32,
}

impl Ppm {
    fn new(rate: u32) -> Ppm {
        let sub = (rate as usize / 1000).max(1);
        let dt = sub as f64 / f64::from(rate);
        Ppm {
            y: 0.0,
            attack: (1.0 - (-dt / 0.005).exp()) as f32,
            // 20 dB (a factor of 10) in 1.7 s.
            release: (-dt * 10f64.ln() / 1.7).exp() as f32,
            sub,
            n: 0,
            peak: 0.0,
        }
    }

    fn push(&mut self, x: f32) {
        let a = x.abs();
        if a > self.peak {
            self.peak = a;
        }
        self.n += 1;
        if self.n >= self.sub {
            if self.peak > self.y {
                self.y += (self.peak - self.y) * self.attack;
            } else {
                self.y *= self.release;
            }
            self.n = 0;
            self.peak = 0.0;
        }
    }
}

fn to_centi_db(db: f64) -> i16 {
    if !db.is_finite() || db < -320.0 {
        NONE
    } else {
        (db * 100.0).round().clamp(-32767.0, 32767.0) as i16
    }
}

fn lin_to_centi_db(x: f32) -> i16 {
    if x <= 1e-10 {
        NONE
    } else {
        to_centi_db(20.0 * f64::from(x).log10())
    }
}

// ---- the meter -------------------------------------------------------------------------------------------------

/// Called when the step diagnostic fires, for the station's name, the item on air and its gain.
pub type StepContext = Arc<dyn Fn() -> String + Send + Sync>;

/// A rise of the momentary loudness of this many LU within [`STEP_WITHIN_S`] seconds is reported.
pub const STEP_LU: f64 = 3.0;
pub const STEP_WITHIN_S: f64 = 0.5;
/// At most one report in this many seconds of audio.
pub const STEP_QUIET_S: f64 = 30.0;

/// The output's momentary loudness over the last half second, to catch a step up (a burst at an item start).
#[derive(Default)]
struct StepWatch {
    /// (frame, LUFS) of the readings of the last second.
    recent: std::collections::VecDeque<(u64, f64)>,
    last_report: Option<u64>,
}

impl StepWatch {
    /// `Some(lowest reading in the window)` when `now` is a reportable step up at `frame`.
    fn feed(&mut self, rate: u32, frame: u64, now: f64) -> Option<f64> {
        let span = (STEP_WITHIN_S * f64::from(rate)) as u64;
        // A second of readings: the last half second to compare with, and the half second before it, which must
        // have been program throughout (a start from silence fills the 400 ms window and looks like a step by itself).
        while self
            .recent
            .front()
            .is_some_and(|(f, _)| frame - f > 2 * span)
        {
            self.recent.pop_front();
        }
        let low = self
            .recent
            .iter()
            .filter(|(f, _)| frame - f <= span)
            .map(|(_, l)| *l)
            .fold(f64::INFINITY, f64::min);
        let before = self
            .recent
            .iter()
            .filter(|(f, _)| frame - f > span)
            .map(|(_, l)| *l)
            .fold(f64::INFINITY, f64::min);
        self.recent.push_back((frame, now));
        if !now.is_finite()
            || !low.is_finite()
            || before < -45.0
            || now < -30.0
            || now - low < STEP_LU
        {
            return None;
        }
        if self
            .last_report
            .is_some_and(|l| frame - l < (STEP_QUIET_S * f64::from(rate)) as u64)
        {
            return None;
        }
        self.last_report = Some(frame);
        Some(low)
    }
}

pub struct Meter {
    /// Names the station, the item on air and its gain for the step report.
    pub context: Option<StepContext>,
    /// Step reports made, and the last one's text (tests).
    pub step_reports: u32,
    pub last_step_report: Option<String>,
    watch: StepWatch,
    rate: u32,
    hop: usize,
    acc: Vec<f32>,
    acc_pre: Vec<f32>,
    ebu: EbuR128,
    /// The loudness controller (6.1): the content-gated windows of the loudness tap and the post tap, and
    /// the law. It runs whether or not the controller is enabled, because the window loudness `W` of the
    /// frame is its output window.
    pub ctl: Controller,
    /// What rt-out reads: the gain is published here after every hop of the law.
    link: Option<Arc<LoudnessLink>>,
    /// What the processing chain publishes (stage levels, reductions, the AGC gain, band reductions).
    chain: Option<Arc<engine::chain::ChainMeters>>,
    /// The true-peak limiter's smallest gain since the last frame, linear (1.0: no reduction).
    tp_gain: f32,
    ppm: [Ppm; 2],
    spectra: Spectra,
    mid_post: Vec<f32>,
    mid_pre: Vec<f32>,
    /// Frames of the post tap consumed.
    pub frames: u64,
    pub seq: u32,
    /// Target of the loudness controller, when there is one (M6a): `NONE` in M1.
    pub cfg_rev: u16,
    /// Frames since the last reset for the integrated reading.
    window_s: f64,
    /// Integrated and window loudness are recomputed twice a second (the window's gated loudness walks its
    /// whole history, and it changes slowly), not at every frame.
    cache_i: i16,
    cache_w: i16,
    /// Momentary and short-term loudness are recomputed at 10 and 5 Hz (each call walks 0.4 s and 3 s of
    /// filtered audio, the most expensive part of the thread); the numerals do not need more.
    cache_m: i16,
    cache_s: i16,
    hops: u64,
}

impl Meter {
    pub fn new(rate: u32, window_s: f64) -> Meter {
        let hop = (rate / FRAME_RATE) as usize;
        let lp = LoudParams {
            window_s,
            ..LoudParams::default()
        };
        Meter {
            context: None,
            step_reports: 0,
            last_step_report: None,
            watch: StepWatch::default(),
            rate,
            hop,
            acc: Vec::with_capacity(hop * 2),
            acc_pre: Vec::with_capacity(hop * 2),
            ebu: EbuR128::new(2, rate, Mode::M | Mode::S | Mode::I | Mode::HISTOGRAM)
                .expect("ebur128"),
            ctl: Controller::new(rate, lp, 0.0),
            link: None,
            chain: None,
            tp_gain: 1.0,
            ppm: [Ppm::new(rate), Ppm::new(rate)],
            spectra: Spectra::new(rate),
            mid_post: vec![0.0; FFT_N],
            mid_pre: vec![0.0; FFT_N],
            frames: 0,
            seq: 0,
            cfg_rev: 0,
            window_s,
            cache_i: NONE,
            cache_w: NONE,
            cache_m: NONE,
            cache_s: NONE,
            hops: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn window_s(&self) -> f64 {
        self.window_s
    }

    /// Connect the controller to the mixer's atomics (the meter thread does this once).
    pub fn set_link(&mut self, link: Arc<LoudnessLink>) {
        self.link = Some(link);
    }

    /// Connect the processing chain's meters (the meter thread does this once).
    pub fn set_chain(&mut self, chain: Arc<engine::chain::ChainMeters>) {
        self.chain = Some(chain);
    }

    /// The true-peak limiter's smallest gain since the last frame was built (from the mixer's atomic).
    pub fn set_tp_gain(&mut self, g: f32) {
        self.tp_gain = g;
    }

    /// Forget the integrated loudness (the window keeps sliding).
    pub fn reset_integrated(&mut self) {
        self.ebu.reset();
    }

    /// Feed interleaved stereo blocks of the two taps (equal lengths) with no content gates and no loudness
    /// tap (the plain meter, as in M1.7). Calls `emit` for each completed hop when `want_frames`; the
    /// loudness and PPM state advance either way.
    pub fn push(
        &mut self,
        post: &[f32],
        pre: &[f32],
        want_frames: bool,
        emit: &mut dyn FnMut(&[u8; FRAME_LEN]),
    ) {
        let info = [BlockInfo {
            frames: (post.len() / 2) as u32,
            flags: 0,
            ..Default::default()
        }];
        self.push_with(post, pre, None, &info, want_frames, emit);
    }

    /// As [`push`](Self::push), with the mixer's block entries (`infos`, whose frame counts sum to the
    /// length of `post`) and the loudness tap's audio for the blocks that carry it (`loud`, the blocks
    /// flagged `HAS_LOUD` one after another). The controller's windows and law run on these first; then
    /// the meter's own hops run over `post` and `pre`.
    pub fn push_with(
        &mut self,
        post: &[f32],
        pre: &[f32],
        loud: Option<&[f32]>,
        infos: &[BlockInfo],
        want_frames: bool,
        emit: &mut dyn FnMut(&[u8; FRAME_LEN]),
    ) {
        debug_assert_eq!(post.len(), pre.len());
        let (mut at, mut at_loud) = (0usize, 0usize);
        let mut hops = 0;
        for info in infos {
            let n = info.frames as usize * 2;
            // A block the rings did not deliver whole (cannot happen: the mixer pushes audio before its
            // entry) ends the pass rather than panicking the thread.
            let Some(p) = post.get(at..at + n) else { break };
            let l = if info.flags & BlockInfo::HAS_LOUD != 0 {
                let l = loud.and_then(|l| l.get(at_loud..at_loud + n));
                at_loud += n;
                l
            } else {
                None
            };
            hops += self.ctl.push_block(info, p, l);
            at += n;
        }
        if hops > 0 {
            if let Some(link) = &self.link {
                if self.ctl.p.enabled {
                    link.set_gain_db(self.ctl.gain_db());
                }
            }
        }
        let mut i = 0;
        while i < post.len() {
            let room = self.hop * 2 - self.acc.len();
            let take = room.min(post.len() - i);
            self.acc.extend_from_slice(&post[i..i + take]);
            self.acc_pre.extend_from_slice(&pre[i..i + take]);
            i += take;
            if self.acc.len() == self.hop * 2 {
                let frame = self.hop_done(want_frames);
                if let Some(f) = frame {
                    emit(&f);
                }
                self.acc.clear();
                self.acc_pre.clear();
            }
        }
    }

    /// The rate-limited WARN for a step up of the output's loudness, with every stage's gain at that moment.
    fn report_step(&mut self, low: f64, now: f64, cm: Option<&engine::chain::MeterRead>) {
        let st = self.ctl.status();
        let short = self.ebu.loudness_shortterm().unwrap_or(f64::NEG_INFINITY);
        let ctx = self.context.as_ref().map(|c| c()).unwrap_or_default();
        let chain = cm.map_or_else(
            || "no chain".to_string(),
            |c| {
                let n = usize::from(c.bands).min(6);
                format!(
                    "agc gain {:+.1} dB (fast level {:.1}, program level {:.1} dBFS, gate held {}), multiband reduction per band {:?} dB, wideband {:.1} dB, clipper {:.1} dB, safety limiter min gain {:.3}",
                    c.agc_gain_db,
                    c.agc_fast_db,
                    c.agc_prog_db,
                    c.agc_gate,
                    c.band_gr_db[..n].iter().map(|g| (g * 10.0).round() / 10.0).collect::<Vec<_>>(),
                    c.wb_gr_db,
                    c.clip_gr_db,
                    c.safety_min_gain
                )
            },
        );
        let msg = format!(
            "output loudness stepped up {:.1} LU within {} ms (momentary {:.1} -> {:.1} LUFS, short-term {:.1}); {ctx}; {chain}; loudness controller gain {:+.2} dB, state {}; true-peak limiter min gain {:.3}",
            now - low,
            (STEP_WITHIN_S * 1000.0) as u32,
            low,
            now,
            short,
            st.gain_db,
            st.state.name(),
            self.tp_gain,
        );
        tracing::warn!("{msg}");
        self.step_reports += 1;
        self.last_step_report = Some(msg);
    }

    fn hop_done(&mut self, want_frames: bool) -> Option<[u8; FRAME_LEN]> {
        let _ = self.ebu.add_frames_f32(&self.acc);
        let mut peak = [0f32; 2];
        for f in self.acc.chunks_exact(2) {
            for c in 0..2 {
                peak[c] = peak[c].max(f[c].abs());
                self.ppm[c].push(f[c]);
            }
        }
        // The mid channel history of both taps (the last 4096 samples).
        let n = self.hop;
        for (hist, src) in [
            (&mut self.mid_post, &self.acc),
            (&mut self.mid_pre, &self.acc_pre),
        ] {
            if n >= FFT_N {
                for (j, d) in hist.iter_mut().enumerate() {
                    let f = &src[(n - FFT_N + j) * 2..(n - FFT_N + j) * 2 + 2];
                    *d = (f[0] + f[1]) * 0.5;
                }
            } else {
                hist.copy_within(n.., 0);
                let base = FFT_N - n;
                for j in 0..n {
                    hist[base + j] = (src[j * 2] + src[j * 2 + 1]) * 0.5;
                }
            }
        }
        self.frames += n as u64;
        self.hops += 1;
        if self.hops % 3 == 1 {
            self.cache_m = to_centi_db(self.ebu.loudness_momentary().unwrap_or(f64::NEG_INFINITY));
        }
        if self.hops % 6 == 1 {
            self.cache_s = to_centi_db(self.ebu.loudness_shortterm().unwrap_or(f64::NEG_INFINITY));
        }
        if self.hops % 15 == 1 {
            self.cache_i = to_centi_db(self.ebu.loudness_global().unwrap_or(f64::NEG_INFINITY));
            self.cache_w = to_centi_db(self.ctl.status().window_out.unwrap_or(f64::NEG_INFINITY));
        }
        // The chain's window since the last hop (None: a meter with no chain, which reports a bypass).
        let cm = self.chain.as_ref().map(|c| c.take());
        if self.hops % 3 == 1 {
            let m = self.ebu.loudness_momentary().unwrap_or(f64::NEG_INFINITY);
            if let Some(low) = self.watch.feed(self.rate, self.frames, m) {
                self.report_step(low, m, cm.as_ref());
            }
        }
        if !want_frames {
            return None;
        }
        let mut b = [0u8; FRAME_LEN];
        b[0] = FRAME_VERSION;
        b[1] = u8::from(cm.as_ref().is_none_or(|c| c.bypassed)); // bit 0: chain bypassed
        b[2..4].copy_from_slice(&(FRAME_LEN as u16).to_le_bytes());
        b[4..8].copy_from_slice(&self.seq.to_le_bytes());
        self.seq = self.seq.wrapping_add(1);
        b[8..16].copy_from_slice(&self.frames.to_le_bytes());
        let put = |b: &mut [u8; FRAME_LEN], at: usize, v: i16| {
            b[at..at + 2].copy_from_slice(&v.to_le_bytes());
        };
        put(&mut b, 16, lin_to_centi_db(peak[0]));
        put(&mut b, 18, lin_to_centi_db(peak[1]));
        put(&mut b, 20, lin_to_centi_db(self.ppm[0].y));
        put(&mut b, 22, lin_to_centi_db(self.ppm[1].y));
        put(&mut b, 24, self.cache_m);
        put(&mut b, 26, self.cache_s);
        // 30: chain_cfg_rev (the loudness settings' count plus the chain's).
        let rev = self
            .cfg_rev
            .wrapping_add(cm.as_ref().map_or(0, |c| c.cfg_rev));
        b[30..32].copy_from_slice(&rev.to_le_bytes());
        // 28, 29, 32..80: the stage bitmask, the band count, the stage meters and the per-band reductions.
        for i in 0..6 {
            put(&mut b, 32 + i * 6, NONE);
            put(&mut b, 32 + i * 6 + 2, NONE);
        }
        for i in 0..6 {
            put(&mut b, 68 + i * 2, NONE);
        }
        let st = self.ctl.status();
        let mut mask = 0u8;
        if let Some(c) = &cm {
            mask |= c.mask;
            let stage = |b: &mut [u8; FRAME_LEN], i: usize, gr: Option<f64>| {
                put(b, 32 + i * 6, lin_to_centi_db(c.stage_in[i]));
                put(b, 32 + i * 6 + 2, lin_to_centi_db(c.stage_out[i]));
                if let Some(g) = gr {
                    put(b, 32 + i * 6 + 4, to_centi_db(g));
                }
            };
            if mask & 1 != 0 {
                stage(&mut b, 0, Some(f64::from(c.agc_gain_db)));
                if c.agc_gate {
                    b[1] |= 2;
                }
            }
            if mask & 2 != 0 {
                let n = usize::from(c.bands).min(6);
                let mean =
                    c.band_gr_db[..n].iter().map(|g| f64::from(*g)).sum::<f64>() / n.max(1) as f64;
                stage(&mut b, 1, Some(-mean));
                for i in 0..n {
                    put(&mut b, 68 + i * 2, to_centi_db(-f64::from(c.band_gr_db[i])));
                }
            }
            if mask & 4 != 0 {
                stage(&mut b, 2, Some(-f64::from(c.wb_gr_db)));
            }
            if mask & 16 != 0 {
                stage(&mut b, 4, Some(-f64::from(c.clip_gr_db)));
                if c.clip_gr_db > 0.05 {
                    b[1] |= 16;
                }
            }
            // The limiter (the chain's block, or the loudness controller's peak stage) and the safety limiter.
            let limiting = c.limiter_on || st.enabled;
            if limiting {
                mask |= 1 << 3;
                let g = f64::from(self.tp_gain.max(1e-6));
                stage(&mut b, 3, Some(20.0 * g.log10()));
                if self.tp_gain < 1.0 {
                    b[1] |= 4; // bit 2: limiter active
                }
            }
            mask |= 1 << 5;
            let sg = f64::from(c.safety_min_gain.max(1e-6));
            stage(&mut b, 5, Some(20.0 * sg.log10()));
            if c.safety_min_gain < 1.0 {
                b[1] |= 8; // bit 3: safety limiter active
            }
        } else if st.enabled {
            // No chain attached (the plain meter): the controller's limiter alone.
            mask |= 1 << 3;
            put(
                &mut b,
                32 + 3 * 6 + 4,
                to_centi_db(20.0 * f64::from(self.tp_gain.max(1e-6)).log10()),
            );
            if self.tp_gain < 1.0 {
                b[1] |= 4;
            }
        }
        if st.enabled {
            mask |= 1 << 6;
        }
        b[28] = mask;
        b[29] = cm.as_ref().map_or(0, |c| c.bands);
        self.tp_gain = 1.0;
        let (post_out, rest) = b[80..].split_at_mut(64);
        let pre_out = &mut rest[..64];
        self.spectra
            .analyze(&self.mid_post, &self.mid_pre, post_out, pre_out);
        // 220..276: the output peak of each of the chain's 14 blocks, left then right (centi-dB, NONE for silence).
        for i in 0..28 {
            put(
                &mut b,
                220 + i * 2,
                cm.as_ref().map_or(NONE, |c| lin_to_centi_db(c.blk_out[i])),
            );
        }
        put(&mut b, 208, self.cache_i);
        put(&mut b, 210, self.cache_w);
        put(
            &mut b,
            212,
            if st.enabled {
                to_centi_db(st.target_lufs)
            } else {
                NONE
            },
        );
        put(
            &mut b,
            214,
            if st.enabled {
                to_centi_db(st.gain_db)
            } else {
                0
            },
        );
        b[216] = st.state as u8;
        Some(b)
    }
}

// ---- decoding a frame (for `GET /chain/meters` and the tests) ---------------------------------------------------

fn i16_at(b: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([b[at], b[at + 1]])
}

fn level(v: i16) -> Value {
    if v == NONE {
        Value::Null
    } else {
        json!(f64::from(v) / 100.0)
    }
}

pub fn spectrum_db(b: &[u8]) -> Vec<f64> {
    b.iter()
        .map(|v| f64::from(*v) * 90.0 / 255.0 - 90.0)
        .collect()
}

/// The frame as JSON with the field names of 11.3; levels in dB (`null` for "no value"), both spectra as
/// arrays of dB values.
pub fn frame_json(b: &[u8]) -> Value {
    let stages = [
        "agc",
        "multiband",
        "wideband",
        "limiter",
        "clipper",
        "safety_limiter",
    ];
    let stage_meters: Vec<Value> = stages
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let o = 32 + i * 6;
            json!({"stage": name, "in": level(i16_at(b, o)), "out": level(i16_at(b, o + 2)), "gr": level(i16_at(b, o + 4))})
        })
        .collect();
    let lstate = ["off", "converging", "on_target", "frozen", "at_limit"]
        .get(b[216] as usize)
        .copied()
        .unwrap_or("off");
    json!({
        "version": b[0],
        "flags": {
            "bypassed": b[1] & 1 != 0, "agc_gate_frozen": b[1] & 2 != 0, "limiter_active": b[1] & 4 != 0,
            "safety_limiter_active": b[1] & 8 != 0, "clipper_active": b[1] & 16 != 0,
        },
        "length": u16::from_le_bytes([b[2], b[3]]),
        "seq": u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        "frame": u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
        "peak_l": level(i16_at(b, 16)), "peak_r": level(i16_at(b, 18)),
        "ppm_l": level(i16_at(b, 20)), "ppm_r": level(i16_at(b, 22)),
        "momentary": level(i16_at(b, 24)), "short_term": level(i16_at(b, 26)),
        "stage_mask": b[28], "band_count": b[29],
        "chain_cfg_rev": u16::from_le_bytes([b[30], b[31]]),
        "stages": stage_meters,
        "band_gr": (0..6).map(|i| level(i16_at(b, 68 + i * 2))).collect::<Vec<_>>(),
        "spectrum_post": spectrum_db(&b[80..144]),
        "spectrum_pre": spectrum_db(&b[144..208]),
        "integrated": level(i16_at(b, 208)), "window": level(i16_at(b, 210)),
        "block_out": if b.len() >= FRAME_LEN { (0..14).map(|i| json!([level(i16_at(b, 220 + i * 4)), level(i16_at(b, 222 + i * 4))])).collect::<Vec<_>>() } else { vec![] },
        "target": level(i16_at(b, 212)), "gain_db": f64::from(i16_at(b, 214)) / 100.0,
        "loudness_state": lstate,
    })
}

// ---- the thread ------------------------------------------------------------------------------------------------

/// The loudness controller's meeting point between the meter thread and the rest of the process (6.1,
/// 7.2): the API writes the parameters and the requests, the thread publishes the status.
#[derive(Default)]
pub struct LoudnessCtl {
    /// The parameters the controller should run with; the thread applies a change when `rev` moves.
    pub params: Mutex<LoudParams>,
    pub rev: AtomicU64,
    /// The gain the controller starts at (a restored checkpoint, or the static estimate).
    pub initial_gain_db: Mutex<f64>,
    /// The controller's status as of the last 250 ms.
    pub status: Mutex<Option<LoudStatus>>,
    /// `Some(gain)`: forget the windows and the trim and put the gain at `gain` (the reset, 6.1).
    pub reset: Mutex<Option<f64>>,
    /// Test hook: set the gain directly.
    pub set_gain: Mutex<Option<f64>>,
    /// `Some(db)`: the chain's makeup was just raised by `db` (negative: lowered); the thread scales the input
    /// window to match and glides the gain down by the same amount (`Controller::begin_shift`). Adds up if
    /// the thread has not taken the last one yet.
    pub shift: Mutex<Option<f64>>,
    /// Test hook: the thread does not drain the taps until this wall time (ms since the epoch).
    pub stall_until_ms: AtomicU64,
    /// The mixer's side of the link, set when the thread starts.
    pub link: std::sync::OnceLock<Arc<LoudnessLink>>,
}

/// What the rest of the process reads from the meter thread.
#[derive(Default)]
pub struct MeterShared {
    pub ctl: LoudnessCtl,
    pub latest: Mutex<Option<[u8; FRAME_LEN]>>,
    pub frames_emitted: AtomicU64,
    /// Thread CPU time, nanoseconds, and the wall time it was measured over.
    pub cpu_ns: AtomicU64,
    pub started_ms: AtomicU64,
    pub stop: AtomicBool,
    /// Integrated-loudness reset requests.
    pub reset: AtomicBool,
    /// A frame is wanted until this wall time (ms since the epoch) even with no subscriber
    /// (`GET /chain/meters`).
    pub want_until_ms: AtomicU64,
    /// Latest loudness numbers for `/health` and the `loudness` topic: momentary, short-term, window,
    /// integrated (LUFS x 100, `NONE` when invalid).
    pub loud: Mutex<[i16; 4]>,
}

fn thread_cpu_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Lines the pre tap up with the post tap. The post tap carries every block. The pre tap carries only the
/// runs the mixer was asked for, each opening with a marker that names the post frame its first block
/// belongs to (`engine::mixer::pre_run_marker`). Everything the reader has no aligned pre audio for is
/// silence: with nobody watching the pre spectrum is not computed, and in the moments between a request
/// and the first block of its run it is a few zeros.
#[derive(Default)]
pub struct PreAligner {
    /// Interleaved pre samples of the current run not yet used.
    pending: Vec<f32>,
    /// The post frame `pending[0]` belongs to (`None`: no run).
    pending_from: Option<u64>,
    /// Post frames handed out so far.
    post_abs: u64,
}

impl PreAligner {
    /// Take what the pre ring held (one contiguous slice: a marker never straddles two). A marker starts a
    /// new run: whatever came before it is stale.
    pub fn ingest(&mut self, words: &[f32]) {
        let mut i = 0;
        while i < words.len() {
            if words[i].to_bits() == PRE_SENTINEL && i + PRE_MARKER_WORDS <= words.len() {
                let lo = u64::from(words[i + 1].to_bits());
                let hi = u64::from(words[i + 2].to_bits());
                self.pending_from = Some(lo | (hi << 32));
                self.pending.clear();
                i += PRE_MARKER_WORDS;
            } else {
                self.pending.push(words[i]);
                i += 1;
            }
        }
    }

    /// The pre audio for the next `post_frames` post frames, into `out` (`2 * post_frames` samples).
    pub fn align(&mut self, post_frames: usize, want: bool, out: &mut Vec<f32>) {
        out.clear();
        let a0 = self.post_abs;
        self.post_abs += post_frames as u64;
        let (true, Some(mut from)) = (want, self.pending_from) else {
            out.resize(post_frames * 2, 0.0);
            self.pending.clear();
            if !want {
                self.pending_from = None;
            }
            return;
        };
        // The run's first frames may belong to post frames already handed out: they are no use now.
        if from < a0 {
            let skip = ((a0 - from) as usize).min(self.pending.len() / 2);
            self.pending.drain(..skip * 2);
            from += skip as u64;
        }
        let zeros = (from.saturating_sub(a0) as usize).min(post_frames);
        out.resize(zeros * 2, 0.0);
        let take = ((post_frames - zeros) * 2).min(self.pending.len());
        out.extend_from_slice(&self.pending[..take]);
        self.pending.drain(..take);
        self.pending_from = Some(from + (take / 2) as u64);
        out.resize(post_frames * 2, 0.0);
    }
}

/// The controller's parameters for a `[loudness]` block.
pub fn params_of(c: &common::config::LoudnessConfig) -> LoudParams {
    LoudParams {
        enabled: c.enabled,
        target_lufs: c.target_lufs,
        true_peak_dbtp: c.true_peak_dbtp,
        window_s: c.window_s,
        max_slew_db_per_min: c.max_slew_db_per_min,
        deadband_lu: c.deadband_lu,
        gain_min_db: c.gain_min_db,
        gain_max_db: c.gain_max_db,
        min_valid_s: c.min_valid_s,
        trim_tau_s: c.trim_tau_s,
        trim_range_db: c.trim_range_db,
        hold_within_item: c.within_item == "hold",
        item_memory_s: c.item_memory_s,
        robust_items: c.robust_items,
        item_cap_s: c.item_cap_s,
        max_limiter_gr_db: c.max_limiter_gr_db,
    }
}

pub type FrameSink = Arc<dyn Fn(&[u8; FRAME_LEN]) + Send + Sync>;
pub type Wanted = Arc<dyn Fn() -> bool + Send + Sync>;

/// Run the meter thread. `sink` is called with each frame while `wanted()` is true.
pub fn spawn(
    mut tap: MeterTap,
    window_s: f64,
    shared: Arc<MeterShared>,
    wanted: Wanted,
    sink: FrameSink,
    context: Option<StepContext>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("meter".into())
        .spawn(move || {
            let mut meter = Meter::new(tap.rate, window_s);
            meter.context = context;
            let link = tap.link.clone();
            meter.set_link(link.clone());
            meter.set_chain(tap.chain.clone());
            let _ = shared.ctl.link.set(link.clone());
            // The controller starts from what the station configured and from the restored (or estimated) gain.
            let mut seen_rev = shared.ctl.rev.load(Ordering::Acquire);
            {
                let p = shared
                    .ctl
                    .params
                    .lock()
                    .map(|p| p.clone())
                    .unwrap_or_default();
                let g = shared.ctl.initial_gain_db.lock().map(|g| *g).unwrap_or(0.0);
                link.enabled.store(p.enabled, Ordering::Relaxed);
                link.set_ceiling_db(p.true_peak_dbtp);
                meter.ctl.set_params(p);
                meter.ctl.set_gain_db(g);
                link.set_gain_db(g);
            }
            let (mut post, mut pre) = (Vec::<f32>::new(), Vec::<f32>::new());
            let mut loud = Vec::<f32>::new();
            let mut infos = Vec::<BlockInfo>::new();
            let mut aligner = PreAligner::default();
            let mut scratch = Vec::<f32>::new();
            let mut request = 0u32;
            let mut last_loud = Instant::now();
            let mut shift_at = Instant::now();
            while !shared.stop.load(Ordering::Relaxed) {
                // A stalled thread (test hook) neither drains the taps nor moves the law; rt-out keeps the
                // last gain it was given.
                if (crate::station_api::wall_ms() as u64)
                    < shared.ctl.stall_until_ms.load(Ordering::Relaxed)
                {
                    std::thread::sleep(Duration::from_millis(8));
                    continue;
                }
                // Parameter changes, requests.
                let rev = shared.ctl.rev.load(Ordering::Acquire);
                if rev != seen_rev {
                    seen_rev = rev;
                    meter.cfg_rev = (rev & 0xFFFF) as u16;
                    if let Ok(p) = shared.ctl.params.lock() {
                        link.enabled.store(p.enabled, Ordering::Relaxed);
                        link.set_ceiling_db(p.true_peak_dbtp);
                        meter.ctl.set_params(p.clone());
                        if !p.enabled {
                            link.set_gain_db(0.0);
                        }
                    }
                }
                if let Some(g) = shared.ctl.reset.lock().ok().and_then(|mut r| r.take()) {
                    meter.ctl.reset(g);
                    meter.reset_integrated();
                    link.set_gain_db(g);
                }
                if let Some(g) = shared.ctl.set_gain.lock().ok().and_then(|mut r| r.take()) {
                    meter.ctl.set_gain_db(g);
                    link.set_gain_db(g);
                }
                if let Some(db) = shared.ctl.shift.lock().ok().and_then(|mut r| r.take()) {
                    if !meter.ctl.shifting() {
                        shift_at = Instant::now();
                    }
                    meter.ctl.begin_shift(db);
                }
                if meter.ctl.shifting() {
                    let now = Instant::now();
                    meter
                        .ctl
                        .tick_shift(now.duration_since(shift_at).as_secs_f64());
                    shift_at = now;
                    link.set_gain_db(meter.ctl.gain_db());
                }
                // The block entries first, then exactly the audio they describe: the mixer pushes the audio
                // before the entry, so all of it is in the rings.
                let ni = tap.info.slots();
                if ni == 0 {
                    // Wake about 100 times a second, not more: each wake-up costs the scheduler more than the
                    // work of a hop does.
                    std::thread::sleep(Duration::from_millis(8));
                    continue;
                }
                infos.clear();
                if let Ok(c) = tap.info.read_chunk(ni) {
                    let (a, b) = c.as_slices();
                    infos.extend_from_slice(a);
                    infos.extend_from_slice(b);
                    c.commit_all();
                }
                let frames: usize = infos.iter().map(|i| i.frames as usize).sum();
                let loud_frames: usize = infos
                    .iter()
                    .filter(|i| i.flags & BlockInfo::HAS_LOUD != 0)
                    .map(|i| i.frames as usize)
                    .sum();
                post.clear();
                if let Ok(c) = tap.post.read_chunk(frames * 2) {
                    let (a, b) = c.as_slices();
                    post.extend_from_slice(a);
                    post.extend_from_slice(b);
                    c.commit_all();
                }
                loud.clear();
                if loud_frames > 0 {
                    if let Ok(c) = tap.loud.read_chunk(loud_frames * 2) {
                        let (a, b) = c.as_slices();
                        loud.extend_from_slice(a);
                        loud.extend_from_slice(b);
                        c.commit_all();
                    }
                }
                // The pre tap is copied by the mixer only while someone wants the pre spectrum. The post
                // ring is read first, so what the pre ring holds is at least what those blocks brought.
                let want = wanted();
                if want != (request != 0) {
                    request = if want { next_pre_request(request) } else { 0 };
                    tap.pre_wanted.store(request, Ordering::Relaxed);
                }
                let m = tap.pre.slots();
                if m > 0 {
                    if let Ok(c) = tap.pre.read_chunk(m) {
                        let (a, b) = c.as_slices();
                        scratch.clear();
                        scratch.extend_from_slice(a);
                        scratch.extend_from_slice(b);
                        c.commit_all();
                        aligner.ingest(&scratch);
                    }
                }
                aligner.align(post.len() / 2, want, &mut pre);
                if shared.reset.swap(false, Ordering::Relaxed) {
                    meter.reset_integrated();
                }
                let tpg = f32::from_bits(
                    link.tp_min_gain_bits
                        .swap(1.0f32.to_bits(), Ordering::Relaxed),
                );
                meter.set_tp_gain(tpg.min(1.0));
                let sh = &shared;
                let mut emit = |f: &[u8; FRAME_LEN]| {
                    if let Ok(mut l) = sh.latest.lock() {
                        *l = Some(*f);
                    }
                    sh.frames_emitted.fetch_add(1, Ordering::Relaxed);
                    sink(f);
                };
                meter.push_with(&post, &pre, Some(&loud), &infos, want, &mut emit);
                if last_loud.elapsed() >= Duration::from_millis(250) {
                    last_loud = Instant::now();
                    if let Ok(mut l) = shared.loud.lock() {
                        *l = [meter.cache_m, meter.cache_s, meter.cache_w, meter.cache_i];
                    }
                    if let Ok(mut st) = shared.ctl.status.lock() {
                        *st = Some(meter.ctl.status());
                    }
                }
                shared.cpu_ns.store(thread_cpu_ns(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(8));
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::mixer::pre_run_marker;

    fn ramp(from: u64, frames: usize) -> Vec<f32> {
        (from..from + frames as u64)
            .flat_map(|i| [i as f32, -(i as f32)])
            .collect()
    }

    #[test]
    fn pre_audio_is_silence_until_a_run_begins_and_then_lines_up_frame_for_frame() {
        let mut al = PreAligner::default();
        let mut out = vec![];
        // Post frames 0..100 go by with nobody wanting the pre tap: zeros.
        al.align(100, false, &mut out);
        assert!(out.iter().all(|x| *x == 0.0) && out.len() == 200);
        // Wanted now; the run starts at post frame 130 (the mixer saw the request a block late).
        al.align(50, true, &mut out); // frames 100..150, nothing ingested yet
        assert!(out.iter().all(|x| *x == 0.0));
        let mut ring = pre_run_marker(150).to_vec();
        ring.extend(ramp(150, 40)); // pre frames belong to post 150..190
        al.ingest(&ring);
        al.align(60, true, &mut out); // post frames 150..210
        assert_eq!(out.len(), 120);
        assert_eq!(out[0], 150.0);
        assert_eq!(out[2 * 39], 189.0);
        assert!(out[2 * 40..].iter().all(|x| *x == 0.0), "no more pre yet");
    }

    #[test]
    fn a_run_whose_start_is_behind_the_reader_drops_the_frames_it_has_no_use_for() {
        let mut al = PreAligner::default();
        let mut out = vec![];
        al.align(100, true, &mut out); // post 0..100 already handed out
        let mut ring = pre_run_marker(90).to_vec();
        ring.extend(ramp(90, 30)); // pre for post 90..120
        al.ingest(&ring);
        al.align(20, true, &mut out); // post 100..120
        assert_eq!(
            out[0], 100.0,
            "frame 100 of the pre run lines up with post 100"
        );
        assert_eq!(out[2 * 19], 119.0);
    }

    #[test]
    fn a_new_marker_makes_the_old_run_stale() {
        let mut al = PreAligner::default();
        let mut out = vec![];
        let mut ring = pre_run_marker(0).to_vec();
        ring.extend(ramp(0, 10));
        ring.extend(pre_run_marker(50).to_vec());
        ring.extend(ramp(50, 10));
        al.ingest(&ring);
        al.align(60, true, &mut out);
        assert!(out[..100].iter().all(|x| *x == 0.0));
        assert_eq!(out[100], 50.0);
    }

    fn sine(hz: f64, db: f64, rate: u32, secs: f64) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        let n = (secs * f64::from(rate)) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s =
                (a * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32;
            v.push(s);
            v.push(s);
        }
        v
    }

    fn run(x: &[f32], want: bool) -> Vec<[u8; FRAME_LEN]> {
        let mut m = Meter::new(48_000, 240.0);
        let mut out = vec![];
        for c in x.chunks(2048) {
            m.push(c, c, want, &mut |f| out.push(*f));
        }
        out
    }

    fn cdb(f: &[u8; FRAME_LEN], at: usize) -> f64 {
        f64::from(i16_at(f, at)) / 100.0
    }

    #[test]
    fn a_sine_reads_its_level_on_the_peak_and_the_ppm() {
        let f = run(&sine(1000.0, -20.0, 48_000, 6.0), true);
        let last = f.last().unwrap();
        println!(
            "peak {} ppm {} M {} S {}",
            cdb(last, 16),
            cdb(last, 20),
            cdb(last, 24),
            cdb(last, 26)
        );
        assert!(
            (cdb(last, 16) + 20.0).abs() < 0.05,
            "peak {}",
            cdb(last, 16)
        );
        assert!((cdb(last, 20) + 20.0).abs() < 0.2, "ppm {}", cdb(last, 20));
        // A stereo 1 kHz sine of peak A reads 20 log10 A LUFS (EBU Tech 3341).
        assert!((cdb(last, 24) + 20.0).abs() < 0.1, "M {}", cdb(last, 24));
        assert!((cdb(last, 26) + 20.0).abs() < 0.1, "S {}", cdb(last, 26));
        assert!((cdb(last, 208) + 20.0).abs() < 0.1, "I {}", cdb(last, 208));
        assert!((cdb(last, 210) + 20.0).abs() < 0.1, "W {}", cdb(last, 210));
        assert_eq!(f.len(), 6 * 30);
    }

    #[test]
    fn the_frame_header_is_v3_276_bytes_with_consecutive_sequence_numbers() {
        let f = run(&sine(1000.0, -20.0, 48_000, 2.0), true);
        for (i, fr) in f.iter().enumerate() {
            assert_eq!(fr[0], 3);
            assert_eq!(u16::from_le_bytes([fr[2], fr[3]]), 276);
            assert_eq!(u32::from_le_bytes([fr[4], fr[5], fr[6], fr[7]]), i as u32);
            assert_eq!(fr[1] & 1, 1, "bypassed");
            assert_eq!(fr[28], 0);
            assert!(fr[217..220].iter().all(|b| *b == 0));
            let frame = u64::from_le_bytes(fr[8..16].try_into().unwrap());
            assert_eq!(frame, 1600 * (i as u64 + 1));
        }
    }

    #[test]
    fn the_spectrum_peaks_in_the_band_that_holds_the_tone() {
        let f = run(&sine(1000.0, -20.0, 48_000, 1.0), true);
        let fr = f.last().unwrap();
        let post = &fr[80..144];
        let edges = band_edges();
        let band = (0..BANDS)
            .find(|b| edges[*b] <= 1000.0 && 1000.0 < edges[b + 1])
            .unwrap();
        let (imax, vmax) = post.iter().enumerate().max_by_key(|(_, v)| **v).unwrap();
        println!(
            "1 kHz is in band {band}; the maximum is band {imax} at {} = {:.1} dB",
            vmax,
            f64::from(*vmax) * 90.0 / 255.0 - 90.0
        );
        assert!(
            imax.abs_diff(band) <= 1,
            "peak band {imax}, tone band {band}"
        );
        for far in [band.saturating_sub(21), band + 21] {
            if far < BANDS {
                let diff = (f64::from(*vmax) - f64::from(post[far])) * 90.0 / 255.0;
                assert!(diff >= 40.0, "band {far} only {diff} dB below the peak");
            }
        }
        let peak_db = f64::from(*vmax) * 90.0 / 255.0 - 90.0;
        assert!(
            (peak_db + 20.0).abs() < 1.5,
            "the tone should read about -20 dB in its band, got {peak_db}"
        );
        // The pre tap carried the same signal here.
        assert_eq!(&fr[80..144], &fr[144..208]);
    }

    #[test]
    fn the_two_taps_are_analyzed_separately() {
        let a = sine(1000.0, -20.0, 48_000, 1.0);
        let b = sine(4000.0, -20.0, 48_000, 1.0);
        let mut m = Meter::new(48_000, 240.0);
        let mut last = None;
        for (x, y) in a.chunks(2048).zip(b.chunks(2048)) {
            m.push(x, y, true, &mut |f| last = Some(*f));
        }
        let f = last.unwrap();
        let top = |s: &[u8]| s.iter().enumerate().max_by_key(|(_, v)| **v).unwrap().0;
        let edges = band_edges();
        let bandof = |hz: f64| {
            (0..BANDS)
                .find(|b| edges[*b] <= hz && hz < edges[b + 1])
                .unwrap()
        };
        assert!(top(&f[80..144]).abs_diff(bandof(1000.0)) <= 1);
        assert!(top(&f[144..208]).abs_diff(bandof(4000.0)) <= 1);
    }

    #[test]
    fn silence_reads_as_no_signal() {
        let f = run(&vec![0.0; 48_000 * 2], true);
        let l = f.last().unwrap();
        assert_eq!(i16_at(l, 16), NONE);
        assert_eq!(i16_at(l, 20), NONE);
        assert_eq!(i16_at(l, 24), NONE);
    }

    #[test]
    fn frames_are_not_built_while_nobody_listens_but_loudness_still_integrates() {
        let x = sine(1000.0, -20.0, 48_000, 5.0);
        let mut m = Meter::new(48_000, 240.0);
        let mut n = 0;
        for c in x.chunks(2048) {
            m.push(c, c, false, &mut |_| n += 1);
        }
        assert_eq!(n, 0);
        let mut got = None;
        m.push(&x[..3200], &x[..3200], true, &mut |f| got = Some(*f));
        let f = got.unwrap();
        assert_ne!(i16_at(&f, 208), NONE, "integrated loudness continued");
        assert_ne!(i16_at(&f, 26), NONE, "short-term continued");
    }

    #[test]
    fn meter_thread_cost_of_a_minute_of_program() {
        // 60 s of a noisy signal through the whole meter (two loudness instances, PPM, two spectra) with
        // frames wanted: the wall time is the CPU the thread needs, as a share of the 60 s it covers.
        let mut x = Vec::with_capacity(48_000 * 2 * 60);
        let mut r = 12345u32;
        for i in 0..48_000 * 60 {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (r >> 9) as f32 / 8_388_608.0 - 1.0;
            let s = 0.1 * (2.0 * PI * 440.0 * (i as f32 / 48_000.0)).sin() + 0.05 * n;
            x.push(s);
            x.push(s);
        }
        let mut m = Meter::new(48_000, 240.0);
        let t = std::time::Instant::now();
        let mut frames = 0;
        for c in x.chunks(2048) {
            m.push(c, c, true, &mut |_| frames += 1);
        }
        let el = t.elapsed().as_secs_f64();
        println!(
            "meter cost: {frames} frames, {el:.3} s for 60 s of audio = {:.2} % of one core",
            el / 60.0 * 100.0
        );
    }

    #[test]
    fn a_step_up_of_the_output_is_reported_once_in_thirty_seconds() {
        let mut m = Meter::new(48_000, 240.0);
        m.context = Some(Arc::new(|| "station test, item 7".to_string()));
        let quiet = sine(1000.0, -40.0, 48_000, 1.0);
        let loud = sine(1000.0, -20.0, 48_000, 1.0);
        for c in quiet.chunks(1600) {
            m.push(c, c, false, &mut |_| {});
        }
        assert_eq!(m.step_reports, 0);
        for c in loud.chunks(1600) {
            m.push(c, c, false, &mut |_| {});
        }
        assert_eq!(m.step_reports, 1, "{:?}", m.last_step_report);
        let text = m.last_step_report.clone().unwrap();
        println!("{text}");
        assert!(text.contains("station test, item 7") && text.contains("LU within 500 ms"));
        // Another step a few seconds later is inside the quiet period.
        for c in quiet.chunks(1600).chain(loud.chunks(1600)) {
            m.push(c, c, false, &mut |_| {});
        }
        assert_eq!(m.step_reports, 1);
    }

    #[test]
    fn a_slow_rise_and_a_start_from_silence_are_not_reported() {
        let mut m = Meter::new(48_000, 240.0);
        let silence = vec![0.0f32; 48_000 * 2];
        for c in silence.chunks(1600) {
            m.push(c, c, false, &mut |_| {});
        }
        let loud = sine(1000.0, -14.0, 48_000, 2.0);
        for c in loud.chunks(1600) {
            m.push(c, c, false, &mut |_| {});
        }
        assert_eq!(m.step_reports, 0);
        // 1 dB every 250 ms is far under 3 LU in half a second.
        let mut m = Meter::new(48_000, 240.0);
        for k in 0..20 {
            let x = sine(1000.0, -40.0 + f64::from(k), 48_000, 0.25);
            for c in x.chunks(1600) {
                m.push(c, c, false, &mut |_| {});
            }
        }
        assert_eq!(m.step_reports, 0, "{:?}", m.last_step_report);
    }

    #[test]
    fn ppm_ballistics_type_i() {
        // A 10 ms burst of a tone reads about -1 dB, and the reading falls 20 dB in about 1.7 s.
        let mut m = Meter::new(48_000, 240.0);
        let mut x = sine(1000.0, 0.0, 48_000, 0.010);
        x.extend(vec![0.0; 48_000 * 2 * 2]);
        let mut readings = vec![];
        for c in x.chunks(1600 * 2) {
            m.push(c, c, true, &mut |f| {
                readings.push(f64::from(i16_at(f, 20)) / 100.0)
            });
        }
        let peak = readings.iter().cloned().fold(-200.0, f64::max);
        println!("ppm of a 10 ms burst: {peak:.2} dB");
        assert!(peak > -2.0 && peak < -0.5, "{peak}");
        // 1.7 s after the maximum it is 20 dB lower (to within a frame).
        let imax = readings.iter().position(|v| *v == peak).unwrap();
        let after = readings[imax + (1.7 * 30.0) as usize];
        println!("1.7 s later: {after:.2} dB ({:.2} down)", peak - after);
        assert!((peak - after - 20.0).abs() < 1.5);
    }
}
