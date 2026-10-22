//! The compressor core, the wideband compressor and the multiband compressor with Linkwitz-Riley crossovers
//! (architecture 6).
//!
//! # The compressor
//!
//! Feed-forward, stereo-linked (the louder channel drives both). The detector is the mean square of the louder
//! channel through a fast one-pole (a few milliseconds: just enough to read a steady tone without ripple), in dB
//! referred to a full-scale sine (`10 log10(p) + 3.01`: a sine of peak amplitude A reads `20 log10 A`). The gain
//! computer is the usual soft-knee static curve; its reduction (dB) is smoothed by a branching one-pole, with the
//! attack time constant while the reduction grows and the release time constant while it falls. Time constants are
//! those of the *gain reduction in dB*, so a level step into the compressing region shows an exponential of exactly
//! that time constant in the reduction.
//!
//! # The crossovers
//!
//! Band `k` is `HP_0 ... HP_(k-1) LP_k AP_(k+1) ... AP_(n-2)` where `LP` and `HP` are 4th-order Linkwitz-Riley
//! (two cascaded Butterworth sections) and `AP_j` is the second-order all-pass `LP_j + HP_j` of crossover `j`
//! (`(s^2 + wc^2)^2 / D^2` summed gives `D~/D`, the all-pass with the Butterworth denominator). With every band
//! gain at unity the bands sum to `AP_0 ... AP_(n-2)`, flat in magnitude. This is the same structure as
//! `hotclock-dsp-model`, whose `H(f)` the tests compare against.

use super::dsp::{db_to_lin, smooth_coef, Bq, Coef, Q_BUTTER};

const LN10_20: f64 = std::f64::consts::LN_10 / 20.0;

/// One compressor (one band, or the whole program).
#[derive(Clone, Debug)]
pub struct Comp {
    thr: f64,
    slope: f64,
    knee: f64,
    makeup: f64,
    att: f64,
    rel: f64,
    det: f64,
    p: f64,
    /// The smoothed gain reduction, dB (0 or more).
    pub gr: f64,
}

impl Comp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        thr_db: f64,
        ratio: f64,
        knee_db: f64,
        attack_ms: f64,
        release_ms: f64,
        makeup_db: f64,
        det_ms: f64,
        rate: f64,
    ) -> Self {
        Self {
            thr: thr_db,
            slope: 1.0 / ratio.max(1.0) - 1.0,
            knee: knee_db.max(0.0),
            makeup: db_to_lin(makeup_db),
            // The detector's own lag adds to the reduction's time constants (measured: about 0.4 of its time
            // constant to the attack's 63 % time and 1.2 to the release's); take that out so that the
            // reduction follows the configured times.
            att: smooth_coef((attack_ms - 0.4 * det_ms).max(0.25 * attack_ms), rate),
            rel: smooth_coef((release_ms - 1.2 * det_ms).max(0.25 * release_ms), rate),
            det: smooth_coef(det_ms, rate),
            p: 0.0,
            gr: 0.0,
        }
    }

    pub fn adopt(&mut self, old: &Comp) {
        self.p = old.p;
        self.gr = old.gr;
    }

    /// The static curve: gain reduction (dB, 0 or more) for a level `x` dB.
    #[inline]
    pub fn static_gr(&self, x: f64) -> f64 {
        let over = x - self.thr;
        if 2.0 * over < -self.knee {
            0.0
        } else if self.knee > 0.0 && 2.0 * over.abs() <= self.knee {
            -self.slope * (over + self.knee / 2.0).powi(2) / (2.0 * self.knee)
        } else {
            -self.slope * over
        }
    }

    /// One frame in; returns the linear gain (with the makeup) for it.
    #[inline]
    pub fn step(&mut self, l: f64, r: f64) -> f64 {
        let e = (l * l).max(r * r);
        self.p += self.det * (e - self.p);
        let x = 10.0 * self.p.max(1e-20).log10() + 3.0103;
        let target = self.static_gr(x);
        let c = if target > self.gr { self.att } else { self.rel };
        self.gr += c * (target - self.gr);
        (-self.gr * LN10_20).exp() * self.makeup
    }

    /// The gain for a reduction forced from outside (the test hook), with the makeup.
    pub fn forced_gain(&self, gr_db: f64) -> f64 {
        (-gr_db * LN10_20).exp() * self.makeup
    }
}

/// The multiband compressor.
#[derive(Clone, Debug)]
pub struct Multiband {
    n: usize,
    /// Per crossover `k`: two low-pass and two high-pass sections.
    lp: Vec<[Bq; 2]>,
    hp: Vec<[Bq; 2]>,
    /// `ap[k][j]`: band `k`'s compensation for crossover `k + 1 + j`.
    ap: Vec<Vec<Bq>>,
    pub comps: Vec<Comp>,
    /// The largest gain reduction of each band since the last `take_gr`.
    gr_max: [f64; 6],
    pub xover: Vec<f64>,
    /// Reductions forced by the test hook (dB), read at the start of each chunk.
    forced: Option<[f64; 6]>,
}

pub struct BandParams<'a> {
    pub crossovers: &'a [f64],
    pub threshold: &'a [f64],
    pub ratio: &'a [f64],
    pub knee: &'a [f64],
    pub attack: &'a [f64],
    pub release: &'a [f64],
    pub makeup: &'a [f64],
}

impl Multiband {
    pub fn new(p: &BandParams, rate: f64) -> Self {
        let n = p.crossovers.len() + 1;
        let lo = |f: f64| Bq::new(Coef::lowpass(f, Q_BUTTER, rate));
        let hi = |f: f64| Bq::new(Coef::highpass(f, Q_BUTTER, rate));
        let lp = p.crossovers.iter().map(|&f| [lo(f), lo(f)]).collect();
        let hp = p.crossovers.iter().map(|&f| [hi(f), hi(f)]).collect();
        let ap = (0..n - 1)
            .map(|k| {
                (k + 1..n - 1)
                    .map(|j| Bq::new(Coef::allpass(p.crossovers[j], Q_BUTTER, rate)))
                    .collect()
            })
            .collect();
        let comps = (0..n)
            .map(|i| {
                let lo_f = if i == 0 { 20.0 } else { p.crossovers[i - 1] };
                let hi_f = if i == n - 1 {
                    20_000.0
                } else {
                    p.crossovers[i]
                };
                let fc = (lo_f * hi_f).sqrt();
                let det_ms = (1200.0 / fc).clamp(1.0, 30.0);
                Comp::new(
                    p.threshold[i],
                    p.ratio[i],
                    p.knee[i],
                    p.attack[i],
                    p.release[i],
                    p.makeup[i],
                    det_ms,
                    rate,
                )
            })
            .collect();
        Self {
            n,
            lp,
            hp,
            ap,
            comps,
            gr_max: [0.0; 6],
            xover: p.crossovers.to_vec(),
            forced: None,
        }
    }

    pub fn bands(&self) -> usize {
        self.n
    }

    pub fn adopt(&mut self, old: &Multiband) {
        if old.n != self.n || old.xover != self.xover {
            return;
        }
        for k in 0..self.n - 1 {
            for s in 0..2 {
                self.lp[k][s].adopt(&old.lp[k][s]);
                self.hp[k][s].adopt(&old.hp[k][s]);
            }
            for (a, b) in self.ap[k].iter_mut().zip(&old.ap[k]) {
                a.adopt(b);
            }
        }
        for (a, b) in self.comps.iter_mut().zip(&old.comps) {
            a.adopt(b);
        }
    }

    pub fn set_forced(&mut self, f: Option<[f64; 6]>) {
        self.forced = f;
    }

    /// The largest reduction of each band since the last call (positive dB), reset.
    pub fn take_gr(&mut self) -> [f64; 6] {
        std::mem::replace(&mut self.gr_max, [0.0; 6])
    }

    /// Process interleaved stereo in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = self.n;
        let forced = self.forced;
        let mut bands = [(0.0f64, 0.0f64); 6];
        for fr in buf.chunks_exact_mut(2) {
            let mut rest: (f64, f64) = (f64::from(fr[0]), f64::from(fr[1]));
            #[allow(clippy::needless_range_loop)]
            for k in 0..n - 1 {
                let mut lo = rest;
                let mut hi = rest;
                for s in 0..2 {
                    lo = self.lp[k][s].run2(lo.0, lo.1);
                    hi = self.hp[k][s].run2(hi.0, hi.1);
                }
                for a in self.ap[k].iter_mut() {
                    lo = a.run2(lo.0, lo.1);
                }
                bands[k] = lo;
                rest = hi;
            }
            bands[n - 1] = rest;
            let (mut ol, mut or) = (0.0, 0.0);
            for (i, b) in bands.iter().enumerate().take(n) {
                let c = &mut self.comps[i];
                let g = match forced {
                    Some(f) => {
                        c.gr = f[i];
                        c.forced_gain(f[i])
                    }
                    None => c.step(b.0, b.1),
                };
                if c.gr > self.gr_max[i] {
                    self.gr_max[i] = c.gr;
                }
                ol += b.0 * g;
                or += b.1 * g;
            }
            fr[0] = ol as f32;
            fr[1] = or as f32;
        }
    }
}

/// The wideband compressor: one `Comp` over the whole program.
#[derive(Clone, Debug)]
pub struct Wideband {
    pub comp: Comp,
    gr_max: f64,
}

impl Wideband {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        thr: f64,
        ratio: f64,
        knee: f64,
        attack: f64,
        release: f64,
        makeup: f64,
        rate: f64,
    ) -> Self {
        Self {
            comp: Comp::new(thr, ratio, knee, attack, release, makeup, 2.0, rate),
            gr_max: 0.0,
        }
    }

    pub fn adopt(&mut self, old: &Wideband) {
        self.comp.adopt(&old.comp);
    }

    pub fn take_gr(&mut self) -> f64 {
        std::mem::replace(&mut self.gr_max, 0.0)
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let g = self.comp.step(f64::from(fr[0]), f64::from(fr[1]));
            if self.comp.gr > self.gr_max {
                self.gr_max = self.comp.gr;
            }
            fr[0] = (f64::from(fr[0]) * g) as f32;
            fr[1] = (f64::from(fr[1]) * g) as f32;
        }
    }
}
