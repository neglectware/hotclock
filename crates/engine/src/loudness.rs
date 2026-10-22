//! The loudness controller (architecture 6.1): BS.1770 measurement with content gates, the sliding
//! gated window, and the feedforward law with a slow output trim.
//!
//! **This module is plain, deterministic arithmetic with no threads and no clock.** The meter thread
//! (`serve`) drives it from the tap rings; the offline acceptance harness drives it the same way from
//! rendered blocks. Time is *audio* time: one step of the law is one 100 ms hop of the taps, so a
//! stalled meter thread simply stops the law (the mixer keeps the last published gain, 1.2) and a
//! thread that catches up moves the gain by the slew limit per hop and no faster.
//!
//! Nothing here runs on rt-out. The mixer's side of the link is [`LoudnessLink`] (atomics only).
//!
//! # Measurement
//!
//! K-weighting (BS.1770-4: a high shelf and a 38 Hz high-pass, coefficients for the actual rate),
//! channels summed with weight 1. The filtered signal is cut into **100 ms sub-blocks**; the 400 ms
//! block ending at each sub-block is the mean of the last four (a 100 ms hop, 75 % overlap), exactly
//! BS.1770's gating blocks. A sub-block is *valid* when no content gate (not playing, a master-gain
//! ramp, a gated category, program silence) was raised during it; a 400 ms block is valid when its
//! four sub-blocks are. The window keeps the last `window_s` of blocks; its **gated loudness** is
//! BS.1770's for a whole programme: absolute gate -70 LUFS, then the relative gate 10 LU below the
//! absolute-gated mean, over the valid blocks only.
//!
//! # Law
//!
//! ```text
//! L_in  = window loudness of the loudness tap (before the gain), over `window_s`
//! L_out = loudness of the post tap over the last `trim_tau_s` (the trim's own stretch)
//! trim += clamp(target - L_out, -0.5, +0.5) * dt / trim_tau_s      clamped to +-trim_range_db
//! g*    = clamp(target - L_in + trim, gain_min, gain_max)
//! g     moves toward g* at no more than max_slew_db_per_min, and not at all while |g* - g| < deadband_lu
//! ```
//!
//! The trim departs from architecture 6.1 in two ways, both recorded in BUILDLOG (M6a): it measures the output
//! over `trim_tau_s` (10 minutes) instead of `window_s`, because the target is an integrated (energy-mean)
//! loudness and a lagging feedforward delivers an energy mean above the mean of its short readings; and its
//! error is clamped to 0.5 LU so that a start from 0 dB or a step in the program, which leave the long
//! window far from the target for ten minutes while the gain is still slewing, cannot wind it up.

use crate::mixer::BlockInfo;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::*};

/// `BS.1770`'s offset: `L = -0.691 + 10 log10(sum of weighted mean squares)`.
const OFFSET: f64 = -0.691;
const ABS_GATE_LUFS: f64 = -70.0;
const REL_GATE_LU: f64 = 10.0;
/// Seconds per sub-block (the hop).
pub const HOP_S: f64 = 0.1;
/// How far under the ceiling the true-peak limiter aims (`tplimiter::DETECT_MARGIN_DB`).
const DETECT_MARGIN_DB: f64 = 0.5;
/// How far from 0 dB the gain must sit, and for how long, before the API suggests moving it into the chain's makeup.
pub const FAR_GAIN_DB: f64 = 3.0;
pub const FAR_GAIN_AFTER_S: f64 = 300.0;
/// The most error, LU, the slow trim takes in at once (see the trim in [`Controller::on_hop`]).
pub const TRIM_ERROR_CLAMP_LU: f64 = 0.5;

// ---------------------------------------------------------------------------------------------
// The link to the mixer (atomics only)

/// What the meter thread, the station and the mixer share. Written by the meter thread (`gain`),
/// the station (`playing`) and the API (`enabled`, `ceiling`); read by rt-out.
pub struct LoudnessLink {
    /// The controller is on: rt-out copies the loudness tap and applies the gain.
    pub enabled: AtomicBool,
    /// The linear gain the meter thread last published (`f32` bits). rt-out ramps to it each period.
    pub gain_bits: AtomicU32,
    /// The gain in dB as last published (for the checkpoint and the API; `f32` bits).
    pub gain_db_bits: AtomicU32,
    /// The true-peak ceiling, dBTP (`f32` bits).
    pub ceiling_db_bits: AtomicU32,
    /// The station's transport is `playing` (the content gate "not playing").
    pub playing: AtomicBool,
    /// The true-peak limiter's smallest gain since the meter thread last did `swap(1.0f32.to_bits())`
    /// (`f32` bits; for non-negative floats the bit order is the numeric order, so `fetch_min` is a
    /// minimum).
    pub tp_min_gain_bits: AtomicU32,
}

impl LoudnessLink {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            gain_db_bits: AtomicU32::new(0.0f32.to_bits()),
            ceiling_db_bits: AtomicU32::new((-1.0f32).to_bits()),
            playing: AtomicBool::new(true),
            tp_min_gain_bits: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    pub fn set_gain_db(&self, db: f64) {
        self.gain_db_bits.store((db as f32).to_bits(), Relaxed);
        self.gain_bits
            .store((10f64.powf(db / 20.0) as f32).to_bits(), Relaxed);
    }

    pub fn gain_db(&self) -> f64 {
        f64::from(f32::from_bits(self.gain_db_bits.load(Relaxed)))
    }

    pub fn set_ceiling_db(&self, db: f64) {
        self.ceiling_db_bits.store((db as f32).to_bits(), Relaxed);
    }

    pub fn ceiling_db(&self) -> f64 {
        f64::from(f32::from_bits(self.ceiling_db_bits.load(Relaxed)))
    }
}

impl Default for LoudnessLink {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Parameters and state

/// The `[loudness]` keys of the station config (6.1), as the controller uses them.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub enabled: bool,
    pub target_lufs: f64,
    pub true_peak_dbtp: f64,
    pub window_s: f64,
    pub max_slew_db_per_min: f64,
    pub deadband_lu: f64,
    pub gain_min_db: f64,
    pub gain_max_db: f64,
    pub min_valid_s: f64,
    pub trim_tau_s: f64,
    pub trim_range_db: f64,
    /// `within_item = hold`: the input window judges each block by the loudness of the item it belongs to
    /// (a running mean over the item with a memory of `item_memory_s`), not by what the block itself
    /// measured, so a quiet passage inside an item does not move the gain; `false` follows the program.
    pub hold_within_item: bool,
    pub item_memory_s: f64,
    /// `hold`: the program level is the duration-weighted median of the last `robust_items` items' levels, each
    /// item counting for at most `item_cap_s` seconds, so one outlier item moves it little and several
    /// consecutive items move it in full.
    pub robust_items: u32,
    pub item_cap_s: f64,
    /// The gain is capped so that the loudest peak of the window needs at most this many dB of true-peak
    /// limiting; 24 or more switches the cap off.
    pub max_limiter_gr_db: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            enabled: false,
            target_lufs: -14.0,
            true_peak_dbtp: -1.0,
            window_s: 240.0,
            max_slew_db_per_min: 1.0,
            deadband_lu: 0.25,
            gain_min_db: -10.0,
            gain_max_db: 8.0,
            min_valid_s: 30.0,
            trim_tau_s: 600.0,
            trim_range_db: 3.0,
            hold_within_item: false,
            item_memory_s: 600.0,
            robust_items: 5,
            item_cap_s: 240.0,
            max_limiter_gr_db: 24.0,
        }
    }
}

/// The controller's state as the meter frame numbers it (11.3, byte 216).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoudState {
    Off = 0,
    Converging = 1,
    OnTarget = 2,
    Frozen = 3,
    AtLimit = 4,
}

impl LoudState {
    pub fn name(self) -> &'static str {
        match self {
            LoudState::Off => "off",
            LoudState::Converging => "converging",
            LoudState::OnTarget => "on_target",
            LoudState::Frozen => "frozen",
            LoudState::AtLimit => "at_limit",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// K-weighting

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
}

impl Biquad {
    #[inline]
    fn run(&self, s: &mut [f64; 2], x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b[0] * x + s[0];
        s[0] = self.b[1] * x - self.a[1] * y + s[1];
        s[1] = self.b[2] * x - self.a[2] * y;
        y
    }
}

/// The two K-weighting stages as `(b, a)` coefficient triples, for the processing chain's AGC detector.
pub(crate) fn k_coefficients(rate: f64) -> [([f64; 3], [f64; 3]); 2] {
    let [a, b] = k_stages(rate);
    [(a.b, a.a), (b.b, b.a)]
}

/// The two BS.1770-4 K-weighting stages for `rate` (the same arithmetic as libebur128).
fn k_stages(rate: f64) -> [Biquad; 2] {
    let (f0, g, q) = (
        1_681.974_450_955_533,
        3.999_843_853_973_347,
        0.707_175_236_955_419_6,
    );
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    [shelf, hp]
}

// ---------------------------------------------------------------------------------------------
// One measured signal: the block meter and its gated window

#[derive(Clone, Copy, Default)]
struct Sub {
    energy: f64,
    valid: bool,
    settled: bool,
    peak: f32,
}

#[derive(Clone, Copy)]
struct Hop {
    /// Mean of the four sub-block energies (the 400 ms block's weighted mean square).
    energy: f64,
    valid: bool,
    /// Recorded after the controller first reached its target (the trim measures only such blocks).
    settled: bool,
    item: u32,
    peak: f32,
}

/// The result of gating a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gated {
    /// Gated loudness, LUFS; `None` when no block passed the gates.
    pub loudness: Option<f64>,
    /// Seconds of program that passed the content gates and the absolute gate (the window's valid time).
    pub valid_s: f64,
}

/// K-weighted 100 ms sub-blocks, the 400 ms blocks at a 100 ms hop, and the sliding window of them.
pub struct BlockMeter {
    stages: [Biquad; 2],
    state: [[[f64; 2]; 2]; 2],
    sub_frames: usize,
    n: usize,
    acc: f64,
    valid: bool,
    settled: bool,
    subs: [Sub; 4],
    nsubs: usize,
    hist: VecDeque<Hop>,
    cap: usize,
    cur_item: u32,
    peak_acc: f32,
    /// `Some(memory_s)`: judge blocks by their item's running mean (see `Params::hold_within_item`).
    hold: Option<f64>,
    means: std::collections::BTreeMap<u32, (f64, u32)>,
}

impl BlockMeter {
    pub fn new(rate: u32, window_s: f64) -> Self {
        let cap = ((window_s / HOP_S).round() as usize).max(8);
        Self {
            stages: k_stages(f64::from(rate)),
            state: [[[0.0; 2]; 2]; 2],
            sub_frames: ((f64::from(rate) * HOP_S).round() as usize).max(1),
            n: 0,
            acc: 0.0,
            valid: true,
            settled: true,
            subs: [Sub::default(); 4],
            nsubs: 0,
            hist: VecDeque::with_capacity(cap + 2),
            cap,
            cur_item: 0,
            peak_acc: 0.0,
            hold: None,
            means: Default::default(),
        }
    }

    pub fn set_window(&mut self, window_s: f64) {
        self.cap = ((window_s / HOP_S).round() as usize).max(8);
        while self.hist.len() > self.cap {
            self.hist.pop_front();
        }
    }

    /// The signal entering this meter has just been made `db` louder at its source (the chain's makeup): scale
    /// everything the window holds, so that it reads what the same program would read now. Energies move by
    /// `db`, peaks by `db` in amplitude.
    pub fn shift_db(&mut self, db: f64) {
        let e = 10f64.powf(db / 10.0);
        let a = 10f64.powf(db / 20.0) as f32;
        for h in self.hist.iter_mut() {
            h.energy *= e;
            h.peak *= a;
        }
        for s in self.subs.iter_mut() {
            s.energy *= e;
            s.peak *= a;
        }
        for m in self.means.values_mut() {
            m.0 *= e;
        }
    }

    /// Forget the window (not the filter state).
    pub fn clear_window(&mut self) {
        self.hist.clear();
        self.nsubs = 0;
        self.n = 0;
        self.acc = 0.0;
        self.valid = true;
        self.settled = true;
    }

    /// Feed interleaved stereo frames; `valid` says whether the content gates were open for all of
    /// them and `settled` says the controller's gain was not slewing. Returns how many 400 ms blocks were
    /// completed (0 or more, one per 100 ms).
    pub fn push(&mut self, frames: &[f32], valid: bool, settled: bool, item: u32) -> usize {
        let mut blocks = 0;
        self.cur_item = item;
        if !valid {
            self.valid = false;
        }
        if !settled {
            self.settled = false;
        }
        for f in frames.chunks_exact(2) {
            if !settled {
                self.settled = false;
            }
            if !valid {
                // Every sub-block this chunk touches is invalid, including the ones it starts.
                self.valid = false;
            }
            let mut e = 0.0;
            for (c, &s) in f.iter().enumerate() {
                if s.is_finite() && s.abs() > self.peak_acc {
                    self.peak_acc = s.abs();
                }
                let mut y = f64::from(if s.is_finite() { s } else { 0.0 });
                y = self.stages[0].run(&mut self.state[c][0], y);
                y = self.stages[1].run(&mut self.state[c][1], y);
                e += y * y;
            }
            self.acc += e;
            self.n += 1;
            if self.n == self.sub_frames {
                blocks += self.finish_sub();
            }
        }
        blocks
    }

    /// Flag the rest of the current sub-block invalid without feeding audio of its own (used when a
    /// chunk is invalid but its samples still ran through the filter by `push`).
    fn finish_sub(&mut self) -> usize {
        let n = self.sub_frames as f64;
        let sub = Sub {
            energy: self.acc / n,
            valid: self.valid,
            settled: self.settled,
            peak: self.peak_acc,
        };
        self.peak_acc = 0.0;
        self.acc = 0.0;
        self.n = 0;
        self.valid = true;
        self.settled = true;
        if self.nsubs < 4 {
            self.subs[self.nsubs] = sub;
            self.nsubs += 1;
            if self.nsubs < 4 {
                return 0;
            }
        } else {
            self.subs.copy_within(1.., 0);
            self.subs[3] = sub;
        }
        let e = self.subs.iter().map(|s| s.energy).sum::<f64>() / 4.0;
        let hop = Hop {
            energy: e,
            valid: self.subs.iter().all(|s| s.valid),
            settled: self.subs.iter().all(|s| s.settled),
            item: self.cur_item,
            peak: self.subs.iter().fold(0.0f32, |m, s| m.max(s.peak)),
        };
        if self.hist.len() >= self.cap {
            self.hist.pop_front();
        }
        if let Some(mem) = self.hold {
            let abs_e = 10f64.powf((ABS_GATE_LUFS - OFFSET) / 10.0);
            if hop.valid && hop.energy > abs_e {
                let m = self.means.entry(hop.item).or_insert((hop.energy, 0));
                m.1 += 1;
                let alpha = (1.0 / f64::from(m.1)).max(HOP_S / mem);
                m.0 += alpha * (hop.energy - m.0);
            }
            if let Some((&last, _)) = self.means.iter().next_back() {
                let keep_from = last.saturating_sub(63);
                self.means.retain(|k, _| *k >= keep_from);
            }
        }
        self.hist.push_back(hop);
        1
    }

    /// The largest sample magnitude over the last `n` 100 ms blocks (every block, gated or not: all of it reaches
    /// the limiter).
    pub fn peak_n(&self, n: usize) -> f32 {
        let skip = self.hist.len().saturating_sub(n);
        self.hist
            .iter()
            .skip(skip)
            .fold(0.0f32, |m, h| m.max(h.peak))
    }

    /// The newest block passed the content gates and the absolute gate: program is flowing right now.
    pub fn last_is_program(&self) -> bool {
        let abs_e = 10f64.powf((ABS_GATE_LUFS - OFFSET) / 10.0);
        self.hist
            .back()
            .is_some_and(|h| h.valid && h.energy > abs_e)
    }

    /// The window's gated loudness over everything it holds (BS.1770 gates, valid blocks only).
    pub fn gated(&self) -> Gated {
        self.gated_n(usize::MAX)
    }

    /// The gated loudness over the last `n` 100 ms blocks of the window.
    pub fn gated_n(&self, n: usize) -> Gated {
        self.gated_core(n, false)
    }

    /// As [`gated_n`](Self::gated_n), over the blocks recorded while the gain was not slewing only.
    pub fn gated_settled(&self, n: usize) -> Gated {
        self.gated_core(n, true)
    }

    /// The program level of `hold`: the duration-weighted median, in dB, of the last `k` items' running
    /// levels; an item counts for its valid seconds so far, at most `cap_s`. Also the valid seconds it rests
    /// on. `None` before any item has program.
    pub fn robust_level(&self, k: usize, cap_s: f64) -> Option<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = self
            .means
            .iter()
            .rev()
            .take(k.max(1))
            .filter(|(_, m)| m.0 > 0.0)
            .map(|(_, m)| {
                (
                    OFFSET + 10.0 * m.0.log10(),
                    (f64::from(m.1) * HOP_S).min(cap_s),
                )
            })
            .collect();
        if v.is_empty() {
            return None;
        }
        let total: f64 = v.iter().map(|x| x.1).sum();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut acc = 0.0;
        for (l, w) in &v {
            acc += w;
            if acc >= total / 2.0 {
                return Some((*l, total));
            }
        }
        v.last().map(|x| (x.0, total))
    }

    /// Judge blocks by their item (`Some(memory_s)`) or by themselves (`None`).
    pub fn set_item_hold(&mut self, memory_s: Option<f64>) {
        self.hold = memory_s;
        if memory_s.is_none() {
            self.means.clear();
        }
    }

    fn gated_core(&self, n: usize, settled_only: bool) -> Gated {
        let abs_e = 10f64.powf((ABS_GATE_LUFS - OFFSET) / 10.0);
        let k = |h: &Hop| -> f64 { h.energy };
        let skip = self.hist.len().saturating_sub(n);
        let (mut n1, mut s1) = (0usize, 0.0);
        for h in self.hist.iter().skip(skip) {
            if h.valid && (h.settled || !settled_only) && k(h) > abs_e {
                n1 += 1;
                s1 += k(h);
            }
        }
        if n1 == 0 {
            return Gated {
                loudness: None,
                valid_s: 0.0,
            };
        }
        let rel_l = OFFSET + 10.0 * (s1 / n1 as f64).log10() - REL_GATE_LU;
        let rel_e = 10f64.powf((rel_l - OFFSET) / 10.0);
        let thr = rel_e.max(abs_e);
        let (mut n2, mut s2) = (0usize, 0.0);
        for h in self.hist.iter().skip(skip) {
            if h.valid && (h.settled || !settled_only) && k(h) > thr {
                n2 += 1;
                s2 += k(h);
            }
        }
        if n2 == 0 {
            return Gated {
                loudness: None,
                valid_s: n1 as f64 * HOP_S,
            };
        }
        Gated {
            loudness: Some(OFFSET + 10.0 * (s2 / n2 as f64).log10()),
            valid_s: n1 as f64 * HOP_S,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The controller

/// What the API and the meter frame read.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub state: LoudState,
    pub target_lufs: f64,
    pub gain_db: f64,
    pub target_gain_db: Option<f64>,
    /// The highest gain the headroom cap allows right now (`None`: the cap is not holding it down).
    pub headroom_cap_db: Option<f64>,
    pub trim_db: f64,
    /// Window loudness of the loudness tap (what the controller measures), LUFS.
    pub window_in: Option<f64>,
    /// Window loudness of the post tap, LUFS.
    pub window_out: Option<f64>,
    pub valid_s: f64,
    /// Seconds the gain has sat at a range limit that the law wanted to pass (alert after 600).
    pub at_limit_s: f64,
    /// Seconds frozen while the station was playing (alert after 1,800).
    pub frozen_playing_s: f64,
    /// Seconds the gain has sat still (on target, or at a limit) more than [`FAR_GAIN_DB`] from 0 dB.
    pub far_steady_s: f64,
    /// A shift into the chain's makeup is gliding: the dB of gain still to come off (negative) or go on.
    pub shifting_db: f64,
}

pub struct Controller {
    pub p: Params,
    rate: u32,
    pub in_meter: BlockMeter,
    pub out_meter: BlockMeter,
    gain_db: f64,
    trim_db: f64,
    state: LoudState,
    target_gain: Option<f64>,
    window_in: Option<f64>,
    window_out: Option<f64>,
    valid_s: f64,
    at_limit_s: f64,
    frozen_playing_s: f64,
    far_steady_s: f64,
    far_ref: Option<f64>,
    /// The gain still to be added (negative: taken off) by a shift into the chain's makeup.
    shift_left: f64,
    playing: bool,
    /// The gain has reached its target once since the start or the last reset; the trim measures only the
    /// blocks recorded from then on (the climb from the start gain would wind it up).
    settled: bool,
    headroom_cap: Option<f64>,
    /// Hops since the last law step (the law runs once per hop of the output meter).
    pub hops: u64,
}

impl Controller {
    pub fn new(rate: u32, p: Params, initial_gain_db: f64) -> Self {
        let w = p.window_s;
        let w_out = w.max(p.trim_tau_s);
        let g = initial_gain_db.clamp(p.gain_min_db, p.gain_max_db);
        let mut in_meter = BlockMeter::new(rate, w);
        in_meter.set_item_hold(p.hold_within_item.then_some(p.item_memory_s));
        Self {
            p,
            rate,
            in_meter,
            out_meter: BlockMeter::new(rate, w_out),
            gain_db: g,
            trim_db: 0.0,
            state: LoudState::Off,
            target_gain: None,
            window_in: None,
            window_out: None,
            valid_s: 0.0,
            at_limit_s: 0.0,
            frozen_playing_s: 0.0,
            far_steady_s: 0.0,
            far_ref: None,
            shift_left: 0.0,
            playing: true,
            settled: false,
            headroom_cap: None,
            hops: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// Set the gain directly (a restored checkpoint, the reset, the test hook).
    pub fn set_gain_db(&mut self, db: f64) {
        self.gain_db = db.clamp(self.p.gain_min_db, self.p.gain_max_db);
    }

    pub fn set_params(&mut self, p: Params) {
        if (p.window_s - self.p.window_s).abs() > f64::EPSILON
            || (p.trim_tau_s - self.p.trim_tau_s).abs() > f64::EPSILON
        {
            self.in_meter.set_window(p.window_s);
            self.out_meter.set_window(p.window_s.max(p.trim_tau_s));
        }
        self.in_meter
            .set_item_hold(p.hold_within_item.then_some(p.item_memory_s));
        self.p = p;
        self.gain_db = self.gain_db.clamp(self.p.gain_min_db, self.p.gain_max_db);
    }

    /// Forget both windows and the trim and put the gain at `gain_db` (the reset of 6.1: the static
    /// start estimate).
    pub fn reset(&mut self, gain_db: f64) {
        self.in_meter.clear_window();
        self.out_meter.clear_window();
        self.trim_db = 0.0;
        self.settled = false;
        self.at_limit_s = 0.0;
        self.frozen_playing_s = 0.0;
        self.far_steady_s = 0.0;
        self.far_ref = None;
        self.shift_left = 0.0;
        self.set_gain_db(gain_db);
    }

    /// The chain's makeup has just been raised by `db` (negative: lowered). The input window is scaled to match
    /// at once, and the gain glides down by the same `db` at [`crate::chain::MAKEUP_SLEW_DB_PER_S`]
    /// ([`tick_shift`](Self::tick_shift)); the law holds still until it is done, and the steady-gain clock
    /// starts again.
    pub fn begin_shift(&mut self, db: f64) {
        self.in_meter.shift_db(db);
        self.shift_left -= db;
        self.far_steady_s = 0.0;
        self.far_ref = None;
    }

    /// A shift is still gliding the gain.
    pub fn shifting(&self) -> bool {
        self.shift_left != 0.0
    }

    /// Move the gain `dt_s` seconds along a shift. True while the shift is still going.
    pub fn tick_shift(&mut self, dt_s: f64) -> bool {
        if self.shift_left == 0.0 {
            return false;
        }
        let step = crate::chain::MAKEUP_SLEW_DB_PER_S * dt_s.max(0.0);
        let d = if self.shift_left.abs() <= step {
            self.shift_left
        } else {
            step.copysign(self.shift_left)
        };
        self.shift_left -= d;
        // Not clamped to the range: the shift only ever moves the gain toward where the chain now delivers it.
        self.gain_db += d;
        self.shift_left != 0.0
    }

    pub fn status(&self) -> Status {
        Status {
            enabled: self.p.enabled,
            state: self.state,
            target_lufs: self.p.target_lufs,
            gain_db: self.gain_db,
            target_gain_db: self.target_gain,
            headroom_cap_db: self.headroom_cap,
            trim_db: self.trim_db,
            window_in: self.window_in,
            window_out: self.window_out,
            valid_s: self.valid_s,
            at_limit_s: self.at_limit_s,
            frozen_playing_s: self.frozen_playing_s,
            far_steady_s: self.far_steady_s,
            shifting_db: self.shift_left,
        }
    }

    /// Feed one tap block. `post` is the post tap (always), `loud` the loudness tap block when the mixer
    /// copied it (`info.has_loud`). Runs the law once for every 100 ms hop the block completes and returns
    /// how many it ran.
    pub fn push_block(&mut self, info: &BlockInfo, post: &[f32], loud: Option<&[f32]>) -> usize {
        self.playing = info.flags & BlockInfo::NOT_PLAYING == 0;
        let content_ok = info.flags & BlockInfo::CONTENT_GATES == 0;
        let settled = self.settled;
        let mut hops = 0;
        // The input meter must see the same frames as the output meter to keep its hops aligned: when the
        // loudness tap was not copied (controller off) it is fed zeros marked invalid.
        match loud {
            Some(l) => {
                self.in_meter.push(l, content_ok, settled, info.item);
            }
            None => {
                let n = post.len();
                // A block of invalid silence of the same length: no allocation, zeros through the filter.
                const Z: [f32; 256] = [0.0; 256];
                let mut left = n;
                while left > 0 {
                    let k = left.min(Z.len());
                    self.in_meter.push(&Z[..k], false, settled, info.item);
                    left -= k;
                }
            }
        }
        let blocks = self.out_meter.push(post, content_ok, settled, info.item);
        for _ in 0..blocks {
            self.on_hop();
            hops += 1;
        }
        hops
    }

    /// One step of the law, per 100 ms of audio.
    fn on_hop(&mut self) {
        self.hops += 1;
        let dt = HOP_S;
        let gin = if self.p.hold_within_item {
            // `hold`: the program level is the robust statistic over the last items, not the window.
            match self
                .in_meter
                .robust_level(self.p.robust_items as usize, self.p.item_cap_s)
            {
                // The level is the items'; whether there is program to judge by is the window's (so a long
                // silence still freezes the gain, however long the items before it were).
                Some((l, v)) => Gated {
                    loudness: Some(l),
                    valid_s: v.min(self.in_meter.gated().valid_s),
                },
                None => Gated {
                    loudness: None,
                    valid_s: 0.0,
                },
            }
        } else {
            self.in_meter.gated()
        };
        let n_win = (self.p.window_s / HOP_S).round() as usize;
        let gout = self.out_meter.gated_n(n_win);
        // The trim looks at a longer stretch of the output (`trim_tau_s`, 10 minutes): the target is an
        // integrated loudness, which is an energy mean, and over a program whose loudness wanders by a few LU
        // the energy mean of the output sits above the mean of its 4-minute readings.
        let glong = self.out_meter.gated_settled(usize::MAX);
        self.window_in = gin.loudness;
        self.window_out = gout.loudness;
        self.valid_s = gin.valid_s;
        if !self.p.enabled {
            self.state = LoudState::Off;
            self.target_gain = None;
            self.at_limit_s = 0.0;
            self.frozen_playing_s = 0.0;
            self.far_steady_s = 0.0;
            return;
        }
        let Some(l_in) = gin.loudness.filter(|_| gin.valid_s >= self.p.min_valid_s) else {
            self.state = LoudState::Frozen;
            self.target_gain = None;
            self.at_limit_s = 0.0;
            self.far_steady_s = 0.0;
            if self.playing {
                self.frozen_playing_s += dt;
            }
            return;
        };
        self.frozen_playing_s = 0.0;
        // A shift into the chain's makeup is gliding the gain: the law waits for it.
        if self.shift_left != 0.0 {
            return;
        }
        // No program is flowing (silence, a gated category, a stopped transport): the gain holds, whatever
        // the aging window still says.
        if !self.out_meter.last_is_program() {
            return;
        }
        // The slow output trim: what the output measures over the last `trim_tau_s` (10 minutes) against the
        // target, an integrator with that time constant. Two departures from a plain `(target - L_out)` on the
        // 4-minute window (BUILDLOG, M6a):
        //   * The longer stretch. The target is an integrated loudness, an energy mean, and the law is a lagging
        //     feedforward: over a program whose loudness wanders by a few LU the energy mean of what it delivers
        //     sits above the mean of its 4-minute readings. Measured on 3 hours of raw-level music the whole
        //     render read +0.9 LU over the target with the 4-minute window in the loop and +0.4 with this.
        //   * The input is clamped to +-`TRIM_ERROR_CLAMP_LU`. A start from 0 dB or a 6 LU step leaves the long
        //     window far from the target for ten minutes while the gain is still slewing; unclamped, that error
        //     would wind the trim up by a dB or more and the output would overshoot when the window caught up.
        //   * It holds while no program is flowing (a silence, a stopped transport, a gated category): the
        //     window it reads does not move then, and an integrator fed a constant error would drift.
        if let Some(l_out) = glong
            .loudness
            .filter(|_| glong.valid_s >= self.p.min_valid_s && self.out_meter.last_is_program())
        {
            let e = (self.p.target_lufs - l_out).clamp(-TRIM_ERROR_CLAMP_LU, TRIM_ERROR_CLAMP_LU);
            self.trim_db = (self.trim_db + e * dt / self.p.trim_tau_s)
                .clamp(-self.p.trim_range_db, self.p.trim_range_db);
        }
        let want = self.p.target_lufs - l_in + self.trim_db;
        // The headroom cap: the loudest peak of the window, lifted by the gain, may need only so much limiting.
        let mut top = self.p.gain_max_db;
        if self.p.max_limiter_gr_db < 24.0 {
            let peak = self.in_meter.peak_n(n_win);
            if peak > 1e-6 {
                let peak_db = 20.0 * f64::from(peak).log10();
                let cap =
                    (self.p.true_peak_dbtp - DETECT_MARGIN_DB) - peak_db + self.p.max_limiter_gr_db;
                top = top.min(cap.max(self.p.gain_min_db));
            }
        }
        self.headroom_cap = (top < self.p.gain_max_db).then_some(top);
        let clamped = want.clamp(self.p.gain_min_db, top);
        self.target_gain = Some(clamped);
        let diff = clamped - self.gain_db;
        let limit_hit = (want > self.p.gain_max_db && self.gain_db >= self.p.gain_max_db - 1e-9)
            || (want < self.p.gain_min_db && self.gain_db <= self.p.gain_min_db + 1e-9);
        if limit_hit {
            self.state = LoudState::AtLimit;
            self.at_limit_s += dt;
            self.track_far(dt);
            return;
        }
        self.at_limit_s = 0.0;
        if diff.abs() < self.p.deadband_lu {
            self.track_far(dt);
            self.state = LoudState::OnTarget;
            // From here on the output is what the trim should judge: the blocks before the first arrival at the
            // target (the climb from the start gain) are not.
            self.settled = true;
            return;
        }
        self.state = LoudState::Converging;
        let step = self.p.max_slew_db_per_min * dt / 60.0;
        self.gain_db += diff.clamp(-step, step);
        self.track_far(dt);
    }

    /// The steady-gain clock: seconds the gain has stayed within 1 dB of where it was when it passed
    /// [`FAR_GAIN_DB`] from 0 dB. A gain that is on its way somewhere keeps restarting it.
    fn track_far(&mut self, dt: f64) {
        if self.gain_db.abs() <= FAR_GAIN_DB {
            self.far_steady_s = 0.0;
            self.far_ref = None;
            return;
        }
        match self.far_ref {
            Some(r) if (self.gain_db - r).abs() <= 1.0 => self.far_steady_s += dt,
            _ => {
                self.far_ref = Some(self.gain_db);
                self.far_steady_s = 0.0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_stereo_1k_sine_reads_its_peak_level() {
        // EBU Tech 3341: a 1 kHz stereo sine of peak A reads 20 log10 A LUFS.
        let mut m = BlockMeter::new(48_000, 240.0);
        m.push(&sine(1000.0, -20.0, 48_000, 10.0), true, true, 0);
        let g = m.gated();
        println!("1 kHz at -20 dBFS peak reads {:?}", g.loudness);
        assert!((g.loudness.unwrap() + 20.0).abs() < 0.02);
        assert!((g.valid_s - 9.7).abs() < 0.11, "valid {}", g.valid_s);
    }

    #[test]
    fn invalid_blocks_are_left_out_and_the_gates_work() {
        let mut m = BlockMeter::new(48_000, 240.0);
        m.push(&sine(1000.0, -20.0, 48_000, 5.0), true, true, 0);
        // Loud material that is gated by content must not count.
        m.push(&sine(1000.0, -3.0, 48_000, 5.0), false, true, 0);
        m.push(&sine(1000.0, -20.0, 48_000, 5.0), true, true, 0);
        let g = m.gated();
        assert!((g.loudness.unwrap() + 20.0).abs() < 0.05, "{:?}", g);
        // Quiet material 15 LU under the loud part is dropped by the relative gate.
        let mut m = BlockMeter::new(48_000, 240.0);
        m.push(&sine(1000.0, -20.0, 48_000, 10.0), true, true, 0);
        m.push(&sine(1000.0, -45.0, 48_000, 10.0), true, true, 0);
        let g = m.gated();
        assert!((g.loudness.unwrap() + 20.0).abs() < 0.1, "{:?}", g);
    }

    #[test]
    fn the_window_forgets_what_is_older_than_window_s() {
        let mut m = BlockMeter::new(48_000, 60.0);
        m.push(&sine(1000.0, -10.0, 48_000, 30.0), true, true, 0);
        m.push(&sine(1000.0, -30.0, 48_000, 70.0), true, true, 0);
        let g = m.gated();
        assert!((g.loudness.unwrap() + 30.0).abs() < 0.05, "{:?}", g);
    }

    #[test]
    fn the_controller_converges_slowly_to_the_target_without_hunting() {
        let rate = 48_000;
        let p = Params {
            enabled: true,
            ..Params::default()
        };
        let mut c = Controller::new(rate, p, 0.0);
        let info = BlockInfo {
            frames: 1024,
            flags: BlockInfo::HAS_LOUD,
            ..Default::default()
        };
        // Program at -18 LUFS, with the output measured as input + gain.
        let x = sine(1000.0, -18.0, rate, 1.0);
        let mut err_cross = 0;
        let mut was_in = false;
        let mut steepest: f64 = 0.0;
        let mut last_gain = c.gain_db();
        let mut last_t = 0.0;
        let mut t = 0.0;
        let mut out = vec![0.0f32; x.len()];
        for _ in 0..(15 * 60) {
            let gl = 10f32.powf(c.gain_db() as f32 / 20.0);
            for (o, s) in out.iter_mut().zip(&x) {
                *o = s * gl;
            }
            for (ch, oc) in x.chunks(2048).zip(out.chunks(2048)) {
                let mut i = info;
                i.frames = (ch.len() / 2) as u32;
                c.push_block(&i, oc, Some(ch));
            }
            t += 1.0;
            if t - last_t >= 60.0 {
                steepest = steepest.max((c.gain_db() - last_gain).abs());
                last_gain = c.gain_db();
                last_t = t;
            }
            let st = c.status();
            if let Some(w) = st.window_out {
                let inside = (w - -14.0).abs() < 0.25;
                if inside && !was_in {
                    err_cross += 1;
                }
                was_in = inside;
            }
        }
        let st = c.status();
        println!(
            "after 15 min: gain {:.3} dB, window_out {:?}, state {}, steepest {:.3} dB/min, crossings {}",
            st.gain_db, st.window_out, st.state.name(), steepest, err_cross
        );
        assert!((st.gain_db - 4.0).abs() < 0.3);
        assert!(steepest <= 1.001);
        assert!(err_cross <= 2);
    }

    #[test]
    fn a_change_of_slew_target_or_deadband_does_not_make_the_controller_acquire_again() {
        // The Processing card applies each setting as it is changed, so a setting that is not about the measured
        // window must leave the measurement alone: the valid program seconds and the window readings stay.
        let rate = 48_000;
        let p = Params {
            enabled: true,
            hold_within_item: false,
            ..Params::default()
        };
        let mut c = Controller::new(rate, p.clone(), 0.0);
        let info = BlockInfo {
            frames: 1024,
            flags: BlockInfo::HAS_LOUD,
            ..Default::default()
        };
        let x = sine(1000.0, -18.0, rate, 1.0);
        let feed = |c: &mut Controller, secs: usize| {
            for _ in 0..secs {
                for ch in x.chunks(2048) {
                    let mut i = info;
                    i.frames = (ch.len() / 2) as u32;
                    c.push_block(&i, ch, Some(ch));
                }
            }
        };
        feed(&mut c, 60);
        let before = c.status();
        assert!(before.valid_s > 50.0, "valid {}", before.valid_s);
        let mut q = p;
        q.max_slew_db_per_min = 3.0;
        q.target_lufs = -16.0;
        q.deadband_lu = 0.3;
        c.set_params(q);
        feed(&mut c, 1);
        let after = c.status();
        assert!(
            after.valid_s >= before.valid_s,
            "valid {} -> {}",
            before.valid_s,
            after.valid_s
        );
        assert!(after.window_in.is_some() && after.window_out.is_some());
        assert_ne!(after.state.name(), "frozen");
    }

    #[test]
    fn a_stretch_with_too_little_valid_program_freezes_the_gain() {
        let rate = 48_000;
        let p = Params {
            enabled: true,
            ..Params::default()
        };
        let mut c = Controller::new(rate, p, 2.5);
        let x = sine(1000.0, -18.0, rate, 10.0);
        let info = BlockInfo {
            frames: (x.len() / 2) as u32,
            flags: BlockInfo::HAS_LOUD,
            ..Default::default()
        };
        c.push_block(&info, &x, Some(&x));
        assert_eq!(c.status().state, LoudState::Frozen);
        assert_eq!(c.gain_db(), 2.5);
    }

    /// Run `secs` seconds of a -18 LUFS tone (made `up_db` louder by the chain's makeup) through the controller
    /// with its output measured as input + gain, one second at a time; shifts glide in 100 ms ticks.
    fn run_closed_loop(c: &mut Controller, up_db: f64, secs: u32, worst_out: &mut f64) {
        let rate = c.rate();
        let x: Vec<f32> = sine(1000.0, -18.0 + up_db, rate, 1.0);
        let info = BlockInfo {
            frames: 1024,
            flags: BlockInfo::HAS_LOUD,
            ..Default::default()
        };
        let mut out = vec![0.0f32; x.len()];
        for _ in 0..secs {
            for tick in 0..10 {
                c.tick_shift(0.1);
                let gl = 10f32.powf(c.gain_db() as f32 / 20.0);
                let (a, b) = (tick * x.len() / 10, (tick + 1) * x.len() / 10);
                for (o, s) in out[a..b].iter_mut().zip(&x[a..b]) {
                    *o = s * gl;
                }
            }
            for (ch, oc) in x.chunks(2048).zip(out.chunks(2048)) {
                let mut i = info;
                i.frames = (ch.len() / 2) as u32;
                c.push_block(&i, oc, Some(ch));
            }
            if let Some(w) = c.status().window_out {
                *worst_out = worst_out.max((w + 14.0).abs());
            }
        }
    }

    #[test]
    fn shifting_the_window_reads_the_same_program_louder() {
        let mut m = BlockMeter::new(48_000, 240.0);
        m.push(&sine(1000.0, -20.0, 48_000, 10.0), true, true, 0);
        assert!((m.gated().loudness.unwrap() + 20.0).abs() < 0.02);
        m.shift_db(3.1);
        let l = m.gated().loudness.unwrap();
        println!("-20 LUFS window shifted by +3.1 dB reads {l:.3}");
        assert!((l + 16.9).abs() < 0.02, "{l}");
        assert!((f64::from(m.peak_n(usize::MAX)) - 10f64.powf((-20.0 + 3.1) / 20.0)).abs() < 1e-3);
    }

    #[test]
    fn moving_the_gain_into_the_makeup_leaves_the_output_alone_and_the_gain_at_zero() {
        let p = Params {
            enabled: true,
            ..Params::default()
        };
        // On target at +4 dB (-18 in, -14 out) after 10 minutes, and the steady-gain clock has run.
        let mut c = Controller::new(48_000, p, 4.0);
        let mut worst = 0.0;
        run_closed_loop(&mut c, 0.0, 600, &mut worst);
        let st = c.status();
        println!(
            "before: gain {:+.3} dB, steady {:.0} s, window_out {:?}",
            st.gain_db, st.far_steady_s, st.window_out
        );
        assert!((st.gain_db - 4.0).abs() < 0.3);
        assert!(st.far_steady_s >= FAR_GAIN_AFTER_S, "{}", st.far_steady_s);
        // The chain's makeup goes up by 4.0 dB; the gain glides down by 4.0 dB at the makeup's own rate.
        c.begin_shift(4.0);
        assert_eq!(c.status().far_steady_s, 0.0);
        assert!(c.shifting());
        let mut worst_out = 0.0;
        run_closed_loop(&mut c, 4.0, 180, &mut worst_out);
        let st = c.status();
        println!(
            "after 3 min: gain {:+.3} dB, window_out {:?}, worst window_out error {worst_out:.3} LU, state {}",
            st.gain_db, st.window_out, st.state.name()
        );
        assert!(!c.shifting());
        assert!(st.gain_db.abs() < 0.5, "gain {}", st.gain_db);
        assert!(worst_out < 0.3, "output moved {worst_out} LU");
        // And it stays there: another 5 minutes, no climb back toward the old gain from a stale window.
        run_closed_loop(&mut c, 4.0, 300, &mut worst_out);
        let st = c.status();
        assert!(st.gain_db.abs() < 0.5, "gain {}", st.gain_db);
        assert!(st.far_steady_s == 0.0);
    }

    #[test]
    fn the_shift_takes_its_size_over_three_db_a_second() {
        let mut c = Controller::new(
            48_000,
            Params {
                enabled: true,
                ..Params::default()
            },
            3.1,
        );
        c.begin_shift(3.1);
        let mut t = 0.0;
        while c.tick_shift(0.01) {
            t += 0.01;
        }
        println!("3.1 dB shift took {t:.2} s, gain now {:.4}", c.gain_db());
        assert!((t - 3.1 / crate::chain::MAKEUP_SLEW_DB_PER_S).abs() < 0.03);
        assert!(c.gain_db().abs() < 1e-9);
        // A negative makeup change raises the gain.
        c.begin_shift(-2.0);
        while c.tick_shift(0.01) {}
        assert!((c.gain_db() - 2.0).abs() < 1e-9);
    }
}
