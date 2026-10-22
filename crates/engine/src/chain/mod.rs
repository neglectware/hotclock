//! The processing chain (architecture 6): a fixed order of optional blocks between the bus and the loudness
//! gain, each with a switch and every parameter exposed.
//!
//! ```text
//! bus -> trim -> (high-pass) -> (phase rotator) -> (stereo enhancer) -> (AGC) -> (EQ pre) -> (bass enhancer)
//!     -> (multiband) -> (EQ post) -> (wideband compressor) -> output makeup -> (clipper)   [the loudness gain,
//!     the true-peak limiter and the safety limiter follow in the mixer]
//! ```
//!
//! The output makeup is the one gain stage that is always there and glides: a change (and a swap that carries it
//! to a new chain, by `adopt`) moves it at [`MAKEUP_SLEW_DB_PER_S`], so "Move to makeup" can lower the loudness
//! controller's gain at the same rate and leave the program level where it was.
//!
//! **Real-time safety.** A [`Chain`] is built off the audio thread ([`ChainControl::apply`]) from a
//! `common::chain::ChainConfig` and handed to rt-out through a ring. rt-out ([`ChainHost::process`]) never
//! allocates, locks or logs: a new chain is started by a pointer exchange, its state is copied from the old one
//! (`adopt`, same-shape blocks only), the two run side by side for [`CROSSFADE_S`] (50 ms) and are mixed with a
//! complementary linear fade, and the old one goes back on a garbage ring to be freed by the control side. Every
//! parameter change, an enable switch included, is such a swap, so a block switched in or out never clicks.
//!
//! **Latency.** No block adds delay (all are IIR or memoryless), so the chain's latency is zero for every
//! combination of switches; the 5 ms of the true-peak limiter, which sits after the chain and is always in the
//! path, is the program's delay and does not change with the chain.
//!
//! **Meters.** [`ChainMeters`] holds atomics the chain writes once per chunk (peaks into and out of each metered
//! stage, gain reduction, the AGC gain, the per-band reductions); the meter thread reads and resets them to build
//! the meter frame (11.3).

pub mod agc;
pub mod blocks;
pub mod comp;
pub mod dsp;
pub mod spectrum;

use agc::{Agc, AgcParams};
use blocks::{Bass, Clipper, Eq, HighPass, Rotator, Stereo};
use common::chain::ChainConfig;
use comp::{BandParams, Multiband, Wideband};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::*};
use std::sync::{Arc, Mutex};

/// The length of the crossfade between the old and the new chain after a swap.
pub const CROSSFADE_S: f64 = 0.05;
/// How fast the output makeup glides to a new value, dB per second. The loudness controller's shift (`Controller::
/// begin_shift`) uses the same rate, so the two cancel while they move.
pub const MAKEUP_SLEW_DB_PER_S: f64 = 3.0;
/// Chains the swap ring and the garbage ring hold.
const RING: usize = 16;

/// Indexes of the stage meters in the frame order: AGC, multiband, wideband compressor, limiter, clipper, safety.
pub const ST_AGC: usize = 0;
pub const ST_MB: usize = 1;
pub const ST_WB: usize = 2;
pub const ST_LIM: usize = 3;
pub const ST_CLIP: usize = 4;
pub const ST_SAFE: usize = 5;

/// The per-block output peaks (left, right) of the Processing page's rack, in signal order: slot `i` of
/// `ChainMeters::blk_out` is `BLK_NAMES[i]`. A block that is off reports the level at its place in the chain, which
/// is its input level. The equalizer's slot is measured where it runs (before or after the multiband).
pub const BLK_NAMES: [&str; 14] = [
    "trim",
    "highpass",
    "rotator",
    "stereo",
    "agc",
    "eq",
    "bass",
    "multiband",
    "wideband",
    "makeup",
    "clipper",
    "loudness",
    "limiter",
    "safety",
];
pub const BLK_LOUD: usize = 11;
pub const BLK_LIM: usize = 12;
pub const BLK_SAFE: usize = 13;

/// Peak of the left and the right channel of interleaved stereo.
pub fn peak_lr(buf: &[f32]) -> (f32, f32) {
    let (mut l, mut r) = (0.0f32, 0.0f32);
    for fr in buf.chunks_exact(2) {
        l = l.max(fr[0].abs());
        r = r.max(fr[1].abs());
    }
    (l, r)
}

/// Publish a block's peaks into the window (a maximum).
pub fn blk_max(a: &[AtomicU32; 28], slot: usize, (l, r): (f32, f32)) {
    f32_max(&a[slot * 2], l);
    f32_max(&a[slot * 2 + 1], r);
}

fn f32_max(a: &AtomicU32, v: f32) {
    if v > 0.0 && v.is_finite() {
        a.fetch_max(v.to_bits(), Relaxed);
    }
}

/// See [`ChainMeters::agc_view`].
#[derive(Debug, Clone, Copy)]
pub struct AgcView {
    pub state: agc::AgcState,
    pub gain_db: f64,
    pub desired_db: f64,
    pub fast_db: f64,
    pub program_db: f64,
}

/// What rt-out publishes for the meter thread and what the control side publishes for rt-out. Peaks and
/// reductions are non-negative `f32` whose bit order is their numeric order, so `fetch_max` is a maximum; the
/// reader takes them with `swap(0)`.
pub struct ChainMeters {
    /// Increments with every change of the chain's static configuration (the frame's `chain_cfg_rev`).
    pub cfg_rev: AtomicU32,
    /// Stage-enabled bits of the chain's own blocks (AGC 0, multiband 1, wideband 2, clipper 4).
    pub mask: AtomicU32,
    pub bands: AtomicU32,
    /// No block of the chain is on (the trim is at 0 dB): the frame's bypass flag.
    pub bypassed: AtomicBool,
    /// The chain's limiter block: on, and its ceiling (dBTP, `f32` bits).
    pub limiter_on: AtomicBool,
    pub limiter_ceiling_bits: AtomicU32,
    pub stage_in: [AtomicU32; 6],
    pub stage_out: [AtomicU32; 6],
    /// Output peak of every block, left then right, linear (`f32` bits), in `BLK_NAMES` order.
    pub blk_out: [AtomicU32; 28],
    /// The AGC's gain, dB (`f32` bits, the latest) and whether its gate held the gain since the last read.
    pub agc_gain_bits: AtomicU32,
    /// The AGC's fast and program levels, dBFS (`f32` bits, the latest): for the meter thread's step diagnostic.
    pub agc_fast_bits: AtomicU32,
    pub agc_prog_bits: AtomicU32,
    pub agc_gate: AtomicBool,
    /// What the AGC is doing (`agc::AgcState` as a number): the latest, the frames spent in each state since the last
    /// read of [`ChainMeters::agc_view`], and the gain it is heading for (`f32` bits, dB).
    pub agc_state_last: AtomicU32,
    pub agc_state_frames: [AtomicU32; 4],
    pub agc_desired_bits: AtomicU32,
    /// Largest reduction in the window, dB, positive.
    pub wb_gr: AtomicU32,
    pub clip_gr: AtomicU32,
    pub band_gr: [AtomicU32; 6],
    /// The safety limiter's smallest gain in the window (`f32` bits of 1.0 when it did nothing).
    pub safety_min_gain: AtomicU32,
    /// Test hook: force the bands' reductions (dB) instead of running the detectors.
    pub force_on: AtomicBool,
    pub force_gr: [AtomicU32; 6],
    /// Chains handed to rt-out, and chains rt-out could not hand back (leaked).
    pub swaps: AtomicU32,
    pub leaks: AtomicU32,
    /// The equalizer analyzer's taps (see [`spectrum`]); the chain fills them only while a client asks.
    pub spec: spectrum::SpecTaps,
}

impl Default for ChainMeters {
    fn default() -> Self {
        let z = || AtomicU32::new(0);
        Self {
            cfg_rev: z(),
            mask: z(),
            bands: z(),
            bypassed: AtomicBool::new(true),
            limiter_on: AtomicBool::new(false),
            limiter_ceiling_bits: AtomicU32::new((-1.0f32).to_bits()),
            stage_in: std::array::from_fn(|_| z()),
            stage_out: std::array::from_fn(|_| z()),
            blk_out: std::array::from_fn(|_| z()),
            agc_gain_bits: z(),
            agc_fast_bits: z(),
            agc_prog_bits: z(),
            agc_gate: AtomicBool::new(false),
            agc_state_last: z(),
            agc_state_frames: [z(), z(), z(), z()],
            agc_desired_bits: z(),
            wb_gr: z(),
            clip_gr: z(),
            band_gr: std::array::from_fn(|_| z()),
            safety_min_gain: AtomicU32::new(1.0f32.to_bits()),
            force_on: AtomicBool::new(false),
            force_gr: std::array::from_fn(|_| z()),
            swaps: z(),
            leaks: z(),
            spec: spectrum::SpecTaps::default(),
        }
    }
}

/// One read of the meters (and a reset of the window).
#[derive(Debug, Clone, Default)]
pub struct MeterRead {
    pub mask: u8,
    pub bands: u8,
    pub bypassed: bool,
    pub limiter_on: bool,
    pub cfg_rev: u16,
    /// Peak into and out of each stage, linear (0 when nothing was measured).
    pub stage_in: [f32; 6],
    pub stage_out: [f32; 6],
    /// Output peak of every block, left then right, linear.
    pub blk_out: [f32; 28],
    pub agc_gain_db: f32,
    pub agc_gate: bool,
    /// The AGC's fast and program levels, dBFS.
    pub agc_fast_db: f32,
    pub agc_prog_db: f32,
    pub wb_gr_db: f32,
    pub clip_gr_db: f32,
    pub band_gr_db: [f32; 6],
    pub safety_min_gain: f32,
}

impl ChainMeters {
    /// The AGC card's numbers: the state (the one the AGC was in for the longest time since the last call, so a blip
    /// shorter than that does not stand for the whole interval; the latest when no audio ran), the gain now and the gain
    /// it is heading for, and the fast and program levels, dBFS. Resets the counts, so one reader (the API) keeps them.
    pub fn agc_view(&self) -> AgcView {
        let n = self.agc_state_frames.each_ref().map(|a| a.swap(0, Relaxed));
        let of = |i: u32| match i {
            1 => agc::AgcState::Gated,
            2 => agc::AgcState::RidingFade,
            3 => agc::AgcState::Clamping,
            _ => agc::AgcState::Riding,
        };
        let state = match (0..4u32).max_by_key(|&i| (n[i as usize], i)) {
            Some(i) if n[i as usize] > 0 => of(i),
            _ => of(self.agc_state_last.load(Relaxed)),
        };
        let f = |a: &AtomicU32| f64::from(f32::from_bits(a.load(Relaxed)));
        AgcView {
            state,
            gain_db: f(&self.agc_gain_bits),
            desired_db: f(&self.agc_desired_bits),
            fast_db: f(&self.agc_fast_bits),
            program_db: f(&self.agc_prog_bits),
        }
    }

    /// Read the window's values and start a new window.
    pub fn take(&self) -> MeterRead {
        let g = |a: &AtomicU32| f32::from_bits(a.swap(0, Relaxed));
        MeterRead {
            mask: self.mask.load(Relaxed) as u8,
            bands: self.bands.load(Relaxed) as u8,
            bypassed: self.bypassed.load(Relaxed),
            limiter_on: self.limiter_on.load(Relaxed),
            cfg_rev: (self.cfg_rev.load(Relaxed) & 0xFFFF) as u16,
            stage_in: std::array::from_fn(|i| g(&self.stage_in[i])),
            stage_out: std::array::from_fn(|i| g(&self.stage_out[i])),
            blk_out: std::array::from_fn(|i| g(&self.blk_out[i])),
            agc_gain_db: f32::from_bits(self.agc_gain_bits.load(Relaxed)),
            agc_gate: self.agc_gate.swap(false, Relaxed),
            agc_fast_db: f32::from_bits(self.agc_fast_bits.load(Relaxed)),
            agc_prog_db: f32::from_bits(self.agc_prog_bits.load(Relaxed)),
            wb_gr_db: g(&self.wb_gr),
            clip_gr_db: g(&self.clip_gr),
            band_gr_db: std::array::from_fn(|i| g(&self.band_gr[i])),
            safety_min_gain: f32::from_bits(self.safety_min_gain.swap(1.0f32.to_bits(), Relaxed)),
        }
    }

    /// Test hook: pin the bands' gain reductions (dB, positive = reduction); `None` runs the detectors again.
    pub fn force_reductions(&self, gr: Option<&[f32]>) {
        match gr {
            Some(v) => {
                for (i, a) in self.force_gr.iter().enumerate() {
                    a.store(v.get(i).copied().unwrap_or(0.0).max(0.0).to_bits(), Relaxed);
                }
                self.force_on.store(true, Release);
            }
            None => self.force_on.store(false, Release),
        }
    }
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// One built chain: the blocks the configuration enables, with their filter and detector state.
pub struct Chain {
    meters: Arc<ChainMeters>,
    trim: f32,
    hp: Option<HighPass>,
    rot: Option<Rotator>,
    stereo: Option<Stereo>,
    agc: Option<Agc>,
    eq: Option<Eq>,
    eq_post: bool,
    bass: Option<Bass>,
    mb: Option<Multiband>,
    wb: Option<Wideband>,
    /// The output makeup: where it is now and where it is going, dB.
    mk_cur: f64,
    mk_target: f64,
    mk_step: f64,
    clip: Option<Clipper>,
}

impl Chain {
    pub fn build(cfg: &ChainConfig, rate: u32, meters: Arc<ChainMeters>) -> Chain {
        let rate = f64::from(rate);
        let n = |b: &str, k: &str| cfg.num(b, k);
        let on = |b: &str| cfg.enabled(b);
        let mb = on("multiband").then(|| {
            let (x, th, ra, kn, at, re, mk) = (
                cfg.list("multiband", "crossovers_hz"),
                cfg.list("multiband", "threshold_dbfs"),
                cfg.list("multiband", "ratio"),
                cfg.list("multiband", "knee_db"),
                cfg.list("multiband", "attack_ms"),
                cfg.list("multiband", "release_ms"),
                cfg.list("multiband", "makeup_db"),
            );
            Multiband::new(
                &BandParams {
                    crossovers: &x,
                    threshold: &th,
                    ratio: &ra,
                    knee: &kn,
                    attack: &at,
                    release: &re,
                    makeup: &mk,
                },
                rate,
            )
        });
        Chain {
            meters,
            trim: dsp::db_to_lin(n("trim", "gain_db")) as f32,
            hp: on("highpass").then(|| {
                HighPass::new(
                    n("highpass", "freq_hz"),
                    n("highpass", "order") as u32,
                    rate,
                )
            }),
            rot: on("rotator").then(|| {
                Rotator::new(
                    n("rotator", "freq_hz"),
                    n("rotator", "sections") as u32,
                    rate,
                )
            }),
            stereo: on("stereo").then(|| Stereo::new(n("stereo", "width"))),
            agc: on("agc").then(|| {
                Agc::new(
                    AgcParams {
                        target: n("agc", "target_dbfs"),
                        max_gain: n("agc", "max_gain_db"),
                        max_red: n("agc", "max_reduction_db"),
                        down: n("agc", "down_db_per_s"),
                        up: n("agc", "up_db_per_s"),
                        detector_ms: n("agc", "detector_ms"),
                        program_ms: n("agc", "program_ms"),
                        gate_db: n("agc", "gate_db"),
                        fast_attack_ms: n("agc", "fast_attack_ms"),
                        fast_trigger: n("agc", "fast_trigger_db"),
                    },
                    rate,
                )
            }),
            eq: on("eq").then(|| Eq::new(cfg, rate)),
            eq_post: cfg.text("eq", "position") == "post",
            bass: on("bass").then(|| {
                Bass::new(
                    n("bass", "freq_hz"),
                    n("bass", "amount_pct"),
                    n("bass", "drive"),
                    rate,
                )
            }),
            mb,
            wb: on("wideband").then(|| {
                Wideband::new(
                    n("wideband", "threshold_dbfs"),
                    n("wideband", "ratio"),
                    n("wideband", "knee_db"),
                    n("wideband", "attack_ms"),
                    n("wideband", "release_ms"),
                    n("wideband", "makeup_db"),
                    rate,
                )
            }),
            mk_cur: n("makeup", "gain_db"),
            mk_target: n("makeup", "gain_db"),
            mk_step: MAKEUP_SLEW_DB_PER_S / rate,
            clip: on("clipper")
                .then(|| Clipper::new(n("clipper", "threshold_dbfs"), n("clipper", "softness_db"))),
        }
    }

    /// No block does anything: the output is the input.
    pub fn is_identity(&self) -> bool {
        self.trim == 1.0
            && self.hp.is_none()
            && self.rot.is_none()
            && self.stereo.is_none()
            && self.agc.is_none()
            && self.eq.as_ref().is_none_or(Eq::is_identity)
            && self.bass.is_none()
            && self.mb.is_none()
            && self.wb.is_none()
            && self.mk_cur == 0.0
            && self.mk_target == 0.0
            && self.clip.is_none()
    }

    /// Stage bits and band count this chain runs (see [`ChainMeters::mask`]).
    pub fn mask(&self) -> (u32, u32) {
        let m = u32::from(self.agc.is_some())
            | u32::from(self.mb.is_some()) << 1
            | u32::from(self.wb.is_some()) << 2
            | u32::from(self.clip.is_some()) << 4;
        (m, self.mb.as_ref().map_or(0, |m| m.bands() as u32))
    }

    /// Start from where `old` stood: the filters' memory, the detectors, the AGC's gain, the compressors'
    /// reductions, for every block both chains have in the same shape.
    pub fn adopt(&mut self, old: &Chain) {
        if let (Some(a), Some(b)) = (&mut self.hp, &old.hp) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.rot, &old.rot) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.agc, &old.agc) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.eq, &old.eq) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.bass, &old.bass) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.mb, &old.mb) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.wb, &old.wb) {
            a.adopt(b);
        }
        if let (Some(a), Some(b)) = (&mut self.clip, &old.clip) {
            a.adopt(b);
        }
        // The makeup carries on from where the old chain's stood and glides to this chain's value.
        self.mk_cur = old.mk_cur;
    }

    /// The output makeup now, dB (for tests).
    pub fn makeup_db(&self) -> f64 {
        self.mk_cur
    }

    /// The AGC's state now (riding when it is off).
    pub fn agc_state(&self) -> agc::AgcState {
        self.agc.as_ref().map_or(agc::AgcState::Riding, Agc::state)
    }

    /// The AGC's gain now, dB (0 when the AGC is off).
    pub fn agc_gain_db(&self) -> f64 {
        self.agc.as_ref().map_or(0.0, Agc::gain_db)
    }

    /// The multiband's smoothed reduction of each band now, dB (positive).
    pub fn band_gr_now(&self) -> Vec<f64> {
        self.mb
            .as_ref()
            .map(|m| m.comps.iter().map(|c| c.gr).collect())
            .unwrap_or_default()
    }

    /// The AGC's detectors (fast and program level, dBFS) for tests.
    pub fn agc_levels(&self) -> (f64, f64) {
        self.agc
            .as_ref()
            .map_or((f64::NEG_INFINITY, f64::NEG_INFINITY), Agc::levels)
    }

    /// The output makeup: a constant gain, or, while it glides, a gain that moves a step per frame.
    fn apply_makeup(&mut self, io: &mut [f32]) {
        if self.mk_cur == self.mk_target {
            if self.mk_cur != 0.0 {
                let g = dsp::db_to_lin(self.mk_cur) as f32;
                for v in io.iter_mut() {
                    *v *= g;
                }
            }
            return;
        }
        for fr in io.chunks_exact_mut(2) {
            let d = self.mk_target - self.mk_cur;
            self.mk_cur = if d.abs() <= self.mk_step {
                self.mk_target
            } else {
                self.mk_cur + self.mk_step.copysign(d)
            };
            let g = dsp::db_to_lin(self.mk_cur) as f32;
            fr[0] *= g;
            fr[1] *= g;
        }
    }

    /// The equalizer, with the analyzer's taps around it: `tap` bits say which to fill (none: just the equalizer).
    fn run_eq(&mut self, io: &mut [f32], tap: u32, m: &ChainMeters) {
        if tap & spectrum::TAP_IN != 0 {
            m.spec.ring(0).push_stereo(io);
        }
        if let Some(e) = &mut self.eq {
            e.process(io);
        }
        if tap & spectrum::TAP_OUT != 0 {
            m.spec.ring(1).push_stereo(io);
        }
    }

    /// Process `io` (interleaved stereo, at most a few thousand frames) in place. `publish` writes the meters.
    pub fn process(&mut self, io: &mut [f32], publish: bool) {
        let m = self.meters.clone();
        // The rack's per-block output peaks: one scan of the buffer after each block that is on; a block that is
        // off reports the level it passes on (the last scan).
        let mut cur = (0.0f32, 0.0f32);
        macro_rules! tap {
            ($slot:expr, $changed:expr) => {
                if publish {
                    if $changed {
                        cur = peak_lr(io);
                    }
                    blk_max(&m.blk_out, $slot, cur);
                }
            };
        }
        if self.trim != 1.0 {
            for v in io.iter_mut() {
                *v *= self.trim;
            }
        }
        tap!(0, true);
        if let Some(h) = &mut self.hp {
            h.process(io);
        }
        tap!(1, self.hp.is_some());
        if let Some(r) = &mut self.rot {
            r.process(io);
        }
        tap!(2, self.rot.is_some());
        if let Some(s) = &mut self.stereo {
            s.process(io);
        }
        tap!(3, self.stereo.is_some());
        if let Some(a) = &mut self.agc {
            let pin = if publish { peak(io) } else { 0.0 };
            a.process(io);
            if publish {
                f32_max(&m.stage_in[ST_AGC], pin);
                f32_max(&m.stage_out[ST_AGC], peak(io));
                m.agc_gain_bits
                    .store((a.gain_db() as f32).to_bits(), Relaxed);
                let (lf, ls) = a.levels();
                m.agc_fast_bits.store((lf as f32).to_bits(), Relaxed);
                m.agc_prog_bits.store((ls as f32).to_bits(), Relaxed);
                if a.take_frozen() {
                    m.agc_gate.store(true, Relaxed);
                }
                let st = a.state() as u32;
                m.agc_state_last.store(st, Relaxed);
                m.agc_state_frames[st as usize].fetch_add((io.len() / 2) as u32, Relaxed);
                m.agc_desired_bits
                    .store((a.desired_db() as f32).to_bits(), Relaxed);
            }
        }
        tap!(4, self.agc.is_some());
        let tap = if publish { m.spec.mode() } else { 0 };
        if !self.eq_post {
            self.run_eq(io, tap, &m);
            tap!(5, self.eq.is_some());
        }
        if let Some(b) = &mut self.bass {
            b.process(io);
        }
        tap!(6, self.bass.is_some());
        if let Some(mb) = &mut self.mb {
            let pin = if publish { peak(io) } else { 0.0 };
            if m.force_on.load(Acquire) {
                let f: [f64; 6] =
                    std::array::from_fn(|i| f64::from(f32::from_bits(m.force_gr[i].load(Relaxed))));
                mb.set_forced(Some(f));
            } else {
                mb.set_forced(None);
            }
            mb.process(io);
            let gr = mb.take_gr();
            if publish {
                f32_max(&m.stage_in[ST_MB], pin);
                f32_max(&m.stage_out[ST_MB], peak(io));
                for (i, g) in gr.iter().enumerate().take(mb.bands()) {
                    // A band that did nothing still reports 0: store a tiny positive so the reader can tell
                    // it ran (the frame's rounding makes it 0.00).
                    f32_max(&m.band_gr[i], (*g as f32).max(1e-9));
                }
            }
        }
        tap!(7, self.mb.is_some());
        if self.eq_post {
            self.run_eq(io, tap, &m);
            tap!(5, self.eq.is_some());
        }
        if let Some(w) = &mut self.wb {
            let pin = if publish { peak(io) } else { 0.0 };
            w.process(io);
            let gr = w.take_gr();
            if publish {
                f32_max(&m.stage_in[ST_WB], pin);
                f32_max(&m.stage_out[ST_WB], peak(io));
                f32_max(&m.wb_gr, (gr as f32).max(1e-9));
            }
        }
        tap!(8, self.wb.is_some());
        let mk_moves = self.mk_cur != 0.0 || self.mk_cur != self.mk_target;
        self.apply_makeup(io);
        tap!(9, mk_moves);
        if let Some(c) = &mut self.clip {
            let pin = if publish { peak(io) } else { 0.0 };
            c.process(io);
            let gr = c.take_gr();
            if publish {
                f32_max(&m.stage_in[ST_CLIP], pin);
                f32_max(&m.stage_out[ST_CLIP], peak(io));
                f32_max(&m.clip_gr, gr.max(1e-9));
            }
        }
        tap!(10, self.clip.is_some());
    }
}

// ---------------------------------------------------------------------------------------------
// The control side

/// Builds chains and hands them to rt-out; one per mixer.
pub struct ChainControl {
    tx: Mutex<Producer<Box<Chain>>>,
    retired: Mutex<Consumer<Box<Chain>>>,
    rate: u32,
    pub meters: Arc<ChainMeters>,
    cfg: Mutex<ChainConfig>,
}

impl ChainControl {
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The configuration last applied.
    pub fn config(&self) -> ChainConfig {
        self.cfg.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Free chains rt-out has finished with.
    pub fn pump(&self) {
        if let Ok(mut r) = self.retired.lock() {
            while r.pop().is_ok() {}
        }
    }

    /// Build the chain for `cfg` and start it on rt-out (a 50 ms crossfade from the one running). The
    /// limiter's switch and ceiling are published for the mixer at once.
    pub fn apply(&self, cfg: &ChainConfig) -> Result<(), String> {
        self.pump();
        let chain = Chain::build(cfg, self.rate, self.meters.clone());
        let (mask, bands) = chain.mask();
        let bypassed = chain.is_identity() && !cfg.enabled("limiter");
        {
            let mut tx = self.tx.lock().map_err(|_| "chain control poisoned")?;
            tx.push(Box::new(chain))
                .map_err(|_| "the chain swap ring is full; try again".to_string())?;
        }
        let m = &self.meters;
        m.mask.store(mask, Relaxed);
        m.bands.store(bands, Relaxed);
        m.bypassed.store(bypassed, Relaxed);
        m.limiter_on.store(cfg.enabled("limiter"), Relaxed);
        m.limiter_ceiling_bits.store(
            (cfg.num("limiter", "ceiling_dbtp") as f32).to_bits(),
            Relaxed,
        );
        m.cfg_rev.fetch_add(1, AcqRel);
        m.swaps.fetch_add(1, Relaxed);
        if let Ok(mut c) = self.cfg.lock() {
            *c = cfg.clone();
        }
        Ok(())
    }
}

/// rt-out's side: the running chain, the one fading out, one waiting, and the rings.
pub struct ChainHost {
    active: Box<Chain>,
    outgoing: Option<Box<Chain>>,
    pending: Option<Box<Chain>>,
    fade_pos: usize,
    fade_len: usize,
    rx: Consumer<Box<Chain>>,
    retire: Producer<Box<Chain>>,
    scratch: Vec<f32>,
    meters: Arc<ChainMeters>,
}

/// Make a host (for the mixer) and its control.
pub fn new_pair(rate: u32, max_block: usize) -> (ChainHost, Arc<ChainControl>) {
    let (tx, rx) = RingBuffer::<Box<Chain>>::new(RING);
    let (retire, retired) = RingBuffer::<Box<Chain>>::new(RING);
    let meters = Arc::new(ChainMeters::default());
    let first = Chain::build(&ChainConfig::default(), rate, meters.clone());
    let ctl = Arc::new(ChainControl {
        tx: Mutex::new(tx),
        retired: Mutex::new(retired),
        rate,
        meters: meters.clone(),
        cfg: Mutex::new(ChainConfig::default()),
    });
    let host = ChainHost {
        active: Box::new(first),
        outgoing: None,
        pending: None,
        fade_pos: 0,
        fade_len: ((CROSSFADE_S * f64::from(rate)).round() as usize).max(1),
        rx,
        retire,
        scratch: vec![0.0; max_block * 2],
        meters,
    };
    (host, ctl)
}

impl ChainHost {
    pub fn meters(&self) -> &Arc<ChainMeters> {
        &self.meters
    }

    fn give_back(&mut self, c: Box<Chain>) {
        if let Err(rtrb::PushError::Full(c)) = self.retire.push(c) {
            // Nobody has freed anything for 16 swaps: leak rather than free on the audio thread.
            self.meters.leaks.fetch_add(1, Relaxed);
            std::mem::forget(c);
        }
    }

    fn start(&mut self, mut new: Box<Chain>) {
        new.adopt(&self.active);
        let old = std::mem::replace(&mut self.active, new);
        self.outgoing = Some(old);
        self.fade_pos = 0;
    }

    /// `input` to `out` (equal lengths, interleaved stereo, at most `max_block` frames).
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) {
        while let Ok(c) = self.rx.pop() {
            if self.outgoing.is_some() {
                if let Some(old) = self.pending.replace(c) {
                    self.give_back(old);
                }
            } else {
                self.start(c);
            }
        }
        out.copy_from_slice(input);
        self.active.process(out, true);
        if self.outgoing.is_some() {
            let n = out.len() / 2;
            let s = &mut self.scratch[..n * 2];
            s.copy_from_slice(input);
            if let Some(o) = &mut self.outgoing {
                o.process(s, false);
            }
            let len = self.fade_len as f32;
            for (i, (y, x)) in out.chunks_exact_mut(2).zip(s.chunks_exact(2)).enumerate() {
                let w = ((self.fade_pos + i) as f32 / len).min(1.0);
                y[0] = x[0] * (1.0 - w) + y[0] * w;
                y[1] = x[1] * (1.0 - w) + y[1] * w;
            }
            self.fade_pos += n;
            if self.fade_pos >= self.fade_len {
                if let Some(o) = self.outgoing.take() {
                    self.give_back(o);
                }
                if let Some(p) = self.pending.take() {
                    self.start(p);
                }
            }
        }
    }

    /// For tests: the AGC's gain and the bands' reductions of the running chain.
    pub fn active(&self) -> &Chain {
        &self.active
    }
}
