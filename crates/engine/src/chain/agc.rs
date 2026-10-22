//! The wideband gated AGC (architecture 6, "Transition behavior of the AGC").
//!
//! # Detectors
//!
//! The program is K-weighted (the BS.1770 filter, less its 0.691 dB offset, so that a 1 kHz sine reads its RMS)
//! and cut into **2 ms sub-blocks** of mean square. A sub-block is *valid* when its level is above the gate
//! (`target + gate_db`). Two levels are read from the valid sub-blocks only, as the energy mean over a window:
//!
//! - the **fast level** `Lf` over `detector_ms` (400 ms), which sees a fade and an onset;
//! - the **program level** `Ls` over `program_ms` (3 s), which sets the long-term target.
//!
//! Counting only the valid sub-blocks means silence between songs does not dilute either level, so the first
//! second of a song after a gap is not mistaken for a quiet passage.
//!
//! # Law, every sub-block
//!
//! ```text
//! frozen  = the newest sub-block is under the gate, or the fast window has no valid sub-block
//! desired = clamp(target - min(Lf, Ls), -max_reduction, +max_gain)
//! gain falls toward desired at no more than down_db_per_s, rises at no more than up_db_per_s,
//!     rising only after the hold that follows a fast clamp; no change at all while frozen
//! ```
//!
//! Taking the lower of the two levels makes the AGC ride a fade up (the fast level falls with the fade while the
//! program level lags) and keeps it slow on the way down (the program level lags a loud step, the slew limits it).
//!
//! # Fast clamp
//!
//! When the fast level is `fast_trigger_db` or more over the level the gain was set for (`Lf + gain - target >=
//! trigger`: a loud song after something quieter, or a loud passage after a quiet one), the gain is pulled toward
//! `target - Lf` in `fast_attack_ms` (the time to cover 95 percent of the way, a time constant of a third of it) instead of falling at `down_db_per_s`, and its rise is held
//! for one fast window afterwards. The fast level is a 400 ms energy mean of the valid sub-blocks, so a drum hit
//! inside a loud program does not move it and does not trigger the clamp; the first valid sub-block after silence
//! already carries the new level, so the clamp starts within 2 to 4 ms of an onset. The clamp is evaluated once
//! per sub-block (10 ms) and the gain is ramped between evaluations.

use super::dsp::{Bq, Coef};

/// Sub-blocks the rings hold: 10 s at 500 per second.
const RING: usize = 5000;
/// Sub-blocks per second.
const HOPS_PER_S: f64 = 500.0;
const OFFSET: f64 = 0.691;
/// The gain is recomputed (and ramped) every this many frames.
const TICK: usize = 16;
/// Invalid sub-blocks in a row (20 ms) that make a break in the program: the fast level forgets what came before
/// it. A song that follows a fade-out and a short gap (a playlist switch fades the old item over 2 s and the next
/// starts 100 to 200 ms later) must not be read against the fade's quiet tail, which is still inside the 400 ms
/// window and, being all valid sub-blocks, would hold the fast level 15 to 25 dB under the onset for tens of
/// milliseconds, so that the clamp that pulls the gain down from its ride up the fade came 40 ms late.
const BREAK_BLOCKS: usize = 10;
/// The fade detector samples the fast level every this many sub-blocks (250 ms).
const FADE_STEP: usize = 125;
/// Steps the fade detector looks back over: 8 steps, 2 s.
const FADE_N: usize = 8;
/// A fade starts when the fast level has fallen by this much over the last 2 s ...
const FADE_DROP_DB: f64 = 4.0;
/// ... and fell in at least this many of the 8 steps. A level that steps down (a quieter bar, a softer verse) falls
/// only while the 400 ms detector window slides over the step, in 3 steps at most for each, so two steps down in 2 s
/// still fall in 6 at most. A fade falls in nearly all of them.
const FADE_FALLING: usize = 7;
/// A step falls when the fast level is this much lower than one step before.
const FADE_STEP_FALL_DB: f64 = 0.05;
/// A fade goes on while the fast level is still falling at least this much over the last [`FADE_HOLD_STEPS`].
const FADE_HOLD_DB: f64 = 1.0;
/// 6 steps, 1.5 s.
const FADE_HOLD_STEPS: usize = 6;

#[derive(Clone, Debug)]
pub struct AgcParams {
    pub target: f64,
    pub max_gain: f64,
    pub max_red: f64,
    pub down: f64,
    pub up: f64,
    pub detector_ms: f64,
    pub program_ms: f64,
    pub gate_db: f64,
    pub fast_attack_ms: f64,
    pub fast_trigger: f64,
}

#[derive(Clone, Debug)]
pub struct Agc {
    p: AgcParams,
    rate: f64,
    k: [Bq; 2],
    hop: usize,
    acc: f64,
    acc_n: usize,
    ring_e: Vec<f64>,
    ring_v: Vec<bool>,
    head: usize,
    filled: usize,
    nf: usize,
    ns: usize,
    gate_e: f64,
    fa_c: f64,
    /// The gain, dB.
    g_db: f64,
    g_lin: f64,
    tick_n: usize,
    ramp_g: f64,
    ramp_step: f64,
    ramp_left: usize,
    hold_up: usize,
    clamping: bool,
    hold_frames: usize,
    /// Invalid sub-blocks in a row up to now.
    gap_run: usize,
    /// Sub-blocks since the program last resumed after a break (saturating); the fast level reads no further back.
    since_break: usize,
    pub frozen: bool,
    /// Set while the gain was held since the meters last asked (the frame's gate flag).
    frozen_seen: bool,
    lf: f64,
    ls: f64,
    /// The gain the AGC is heading for now (before the rate limits), dB.
    desired: f64,
    /// The program is fading out and the gain follows it up (see [`AgcState::RidingFade`]).
    fading: bool,
    /// The fast level, every [`FADE_STEP`] sub-blocks, the newest last (`-inf` where the window had no valid block).
    lf_hist: [f64; FADE_N + 1],
    lf_step: usize,
    /// The fast level has been falling steadily: see [`FADE_FALLING`].
    falling: bool,
}

/// What the AGC is doing, for the card's state pill and the rack's lights. Clamping outranks the gate, which outranks a
/// fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgcState {
    /// The gain follows the program (also up through a quieter passage).
    Riding = 0,
    /// The newest sub-block is under the gate: the gain is held.
    Gated = 1,
    /// A fade: the fast level has been falling steadily (by [`FADE_DROP_DB`] or more over 2 s, in 7 of its 8 steps of
    /// 250 ms, and still falling), it is under the program level, and the gain is not falling.
    RidingFade = 2,
    /// The fast clamp is pulling the gain down on a loud onset, and the hold after it, while the gain may not rise.
    Clamping = 3,
}

impl Agc {
    pub fn new(p: AgcParams, rate: f64) -> Self {
        let kc = crate::loudness::k_coefficients(rate);
        let k = [
            Bq::new(Coef {
                b: kc[0].0,
                a: [kc[0].1[1], kc[0].1[2]],
            }),
            Bq::new(Coef {
                b: kc[1].0,
                a: [kc[1].1[1], kc[1].1[2]],
            }),
        ];
        let hop = (rate / HOPS_PER_S).round() as usize;
        Self {
            hop,
            nf: ((p.detector_ms / 2.0).round() as usize).clamp(1, RING),
            ns: ((p.program_ms / 2.0).round() as usize).clamp(1, RING),
            gate_e: 10f64.powf((p.target + p.gate_db + OFFSET) / 10.0),
            fa_c: if p.fast_attack_ms > 0.0 {
                1.0 - (-3.0 * (hop as f64) / (p.fast_attack_ms * 0.001 * rate)).exp()
            } else {
                0.0
            },
            hold_frames: (p.detector_ms * 0.001 * rate) as usize,
            gap_run: 0,
            since_break: RING,
            p,
            rate,
            k,
            acc: 0.0,
            acc_n: 0,
            ring_e: vec![0.0; RING],
            ring_v: vec![false; RING],
            head: 0,
            filled: 0,
            g_db: 0.0,
            g_lin: 1.0,
            tick_n: 0,
            ramp_g: 1.0,
            ramp_step: 0.0,
            ramp_left: 0,
            hold_up: 0,
            clamping: false,
            frozen: true,
            frozen_seen: false,
            lf: f64::NEG_INFINITY,
            ls: f64::NEG_INFINITY,
            desired: 0.0,
            fading: false,
            lf_hist: [f64::NEG_INFINITY; FADE_N + 1],
            lf_step: 0,
            falling: false,
        }
    }

    /// Carry the gain, the detectors' history and the filters across a swap (same-rate chains).
    pub fn adopt(&mut self, old: &Agc) {
        for (a, b) in self.k.iter_mut().zip(&old.k) {
            a.adopt(b);
        }
        self.acc = old.acc;
        self.acc_n = old.acc_n;
        self.ring_e.copy_from_slice(&old.ring_e);
        self.ring_v.copy_from_slice(&old.ring_v);
        self.head = old.head;
        self.filled = old.filled;
        self.g_db = old.g_db.clamp(-self.p.max_red, self.p.max_gain);
        self.g_lin = 10f64.powf(self.g_db / 20.0);
        self.tick_n = old.tick_n;
        self.ramp_g = old.ramp_g;
        self.ramp_step = old.ramp_step;
        self.ramp_left = old.ramp_left;
        self.hold_up = old.hold_up;
        self.gap_run = old.gap_run;
        self.since_break = old.since_break;
        self.clamping = old.clamping;
        self.frozen = old.frozen;
        self.lf_hist = old.lf_hist;
        self.lf_step = old.lf_step;
        self.falling = old.falling;
        // The sub-block validity was decided against the old gate; decide it again against this one.
        for i in 0..RING {
            self.ring_v[i] = self.ring_e[i] > self.gate_e;
        }
    }

    pub fn gain_db(&self) -> f64 {
        self.g_db
    }

    /// Whether the gate held the gain since the last call (the meter frame's flag), reset.
    pub fn take_frozen(&mut self) -> bool {
        std::mem::replace(&mut self.frozen_seen, false)
    }

    /// The gain the AGC is heading for, dB.
    pub fn desired_db(&self) -> f64 {
        self.desired
    }

    /// The state of the AGC now.
    pub fn state(&self) -> AgcState {
        if self.clamping || self.hold_up > 0 {
            AgcState::Clamping
        } else if self.frozen {
            AgcState::Gated
        } else if self.fading {
            AgcState::RidingFade
        } else {
            AgcState::Riding
        }
    }

    pub fn levels(&self) -> (f64, f64) {
        (self.lf, self.ls)
    }

    fn mean_level(&self, n: usize) -> f64 {
        let n = n.min(self.filled);
        let (mut s, mut c) = (0.0, 0usize);
        for j in 0..n {
            let i = (self.head + RING - 1 - j) % RING;
            if self.ring_v[i] {
                s += self.ring_e[i];
                c += 1;
            }
        }
        if c == 0 {
            f64::NEG_INFINITY
        } else {
            10.0 * (s / c as f64).log10() - OFFSET
        }
    }

    fn hop_done(&mut self) {
        let e = self.acc / self.acc_n.max(1) as f64;
        self.acc = 0.0;
        self.acc_n = 0;
        self.ring_e[self.head] = e;
        self.ring_v[self.head] = e > self.gate_e;
        self.head = (self.head + 1) % RING;
        self.filled = (self.filled + 1).min(RING);
        let newest_valid = self.ring_v[(self.head + RING - 1) % RING];
        if newest_valid {
            // The first valid sub-block after a break starts a new stretch for the fast level.
            self.since_break = if self.gap_run >= BREAK_BLOCKS {
                1
            } else {
                (self.since_break + 1).min(RING)
            };
            self.gap_run = 0;
        } else {
            self.gap_run += 1;
            self.since_break = (self.since_break + 1).min(RING);
        }
        self.lf = self.mean_level(self.nf.min(self.since_break));
        self.ls = self.mean_level(self.ns);
        self.lf_step += 1;
        if self.lf_step >= FADE_STEP {
            self.lf_step = 0;
            self.track_fade();
        }
        self.frozen = !newest_valid || self.lf == f64::NEG_INFINITY;
        if self.frozen {
            self.frozen_seen = true;
            self.fading = false;
            return;
        }
        let leff = self.lf.min(self.ls);
        let desired = (self.p.target - leff).clamp(-self.p.max_red, self.p.max_gain);
        self.desired = desired;
        // A fade being ridden up: the fast level is falling steadily, it is a dB or more under the program level and the
        // gain is following it up (or already there: the gain tracks a slow fade with no lag worth the name, or it is at
        // its maximum).
        self.fading = self.falling && self.lf < self.ls - 1.0 && desired >= self.g_db - 0.05;
        // The fast clamp on a loud onset.
        // Once it has started it runs until the gain is within 0.5 dB of the level it is pulling toward.
        let over = self.lf + self.g_db - self.p.target;
        if self.fa_c > 0.0 && (over >= self.p.fast_trigger || (self.clamping && over > 0.5)) {
            let fast = (self.p.target - self.lf).clamp(-self.p.max_red, self.p.max_gain);
            if fast < self.g_db {
                self.clamping = true;
                self.g_db += (fast - self.g_db) * self.fa_c;
                self.hold_up = self.hold_frames;
                return;
            }
        }
        self.clamping = false;
        let dt = self.hop as f64 / self.rate;
        if desired < self.g_db {
            self.g_db = desired.max(self.g_db - self.p.down * dt);
        } else if self.hold_up == 0 {
            self.g_db = desired.min(self.g_db + self.p.up * dt);
        }
    }

    /// Every [`FADE_STEP`] sub-blocks: take the fast level into the fade detector's history and decide whether it is
    /// falling the way a fade falls. A break in the program (the fast level has no valid block) ends it.
    fn track_fade(&mut self) {
        self.lf_hist.rotate_left(1);
        self.lf_hist[FADE_N] = self.lf;
        let h = &self.lf_hist;
        if h.iter().any(|x| !x.is_finite()) {
            self.falling = false;
            return;
        }
        let now = h[FADE_N];
        self.falling = if self.falling {
            h[FADE_N - FADE_HOLD_STEPS] - now >= FADE_HOLD_DB
        } else {
            let steps = h
                .windows(2)
                .filter(|w| w[0] - w[1] >= FADE_STEP_FALL_DB)
                .count();
            h[0] - now >= FADE_DROP_DB && steps >= FADE_FALLING
        };
    }

    fn tick(&mut self) {
        if self.hold_up > 0 {
            self.hold_up = self.hold_up.saturating_sub(TICK);
        }
    }

    /// Process interleaved stereo in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            if self.tick_n == 0 {
                self.tick();
                let g_next = 10f64.powf(self.g_db / 20.0);
                self.ramp_step = (g_next - self.g_lin) / TICK as f64;
                self.ramp_g = self.g_lin;
                self.g_lin = g_next;
                self.ramp_left = TICK;
            }
            let (l, r) = (f64::from(fr[0]), f64::from(fr[1]));
            // The detector sees the input, the gain acts on it.
            let (mut kl, mut kr) = self.k[0].run2(l, r);
            (kl, kr) = self.k[1].run2(kl, kr);
            let pw = 0.5 * (kl * kl + kr * kr);
            self.acc += pw;
            self.acc_n += 1;
            let g = if self.ramp_left > 0 {
                self.ramp_g += self.ramp_step;
                self.ramp_left -= 1;
                self.ramp_g
            } else {
                self.g_lin
            };
            fr[0] = (l * g) as f32;
            fr[1] = (r * g) as f32;
            self.tick_n += 1;
            if self.tick_n == TICK {
                self.tick_n = 0;
            }
            if self.acc_n == self.hop {
                self.hop_done();
            }
        }
    }
}
