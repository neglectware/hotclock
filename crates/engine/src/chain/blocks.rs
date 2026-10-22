//! The smaller blocks of the chain: input high-pass, phase rotator, stereo enhancer, bass enhancer, equalizer and
//! clipper. All process interleaved stereo `f32` in place, with `f64` filter state, and allocate nothing.

use super::dsp::{db_to_lin, Bq, Coef, Q_BUTTER};
use common::chain::{ChainConfig, EQ_MAX_BANDS};

// ---- input high-pass --------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HighPass {
    sections: Vec<Bq>,
}

impl HighPass {
    pub fn new(freq: f64, order: u32, rate: f64) -> Self {
        let mut s = vec![];
        match order {
            1 => s.push(Bq::new(Coef::highpass1(freq, rate))),
            2 => s.push(Bq::new(Coef::highpass(freq, Q_BUTTER, rate))),
            3 => {
                s.push(Bq::new(Coef::highpass1(freq, rate)));
                s.push(Bq::new(Coef::highpass(freq, 1.0, rate)));
            }
            _ => {
                // 4th-order Butterworth: Q = 0.5412 and 1.3066.
                s.push(Bq::new(Coef::highpass(freq, 0.541_196_1, rate)));
                s.push(Bq::new(Coef::highpass(freq, 1.306_563, rate)));
            }
        }
        Self { sections: s }
    }

    pub fn adopt(&mut self, old: &HighPass) {
        if self.sections.len() == old.sections.len() {
            for (a, b) in self.sections.iter_mut().zip(&old.sections) {
                a.adopt(b);
            }
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let (mut l, mut r) = (f64::from(fr[0]), f64::from(fr[1]));
            for s in &mut self.sections {
                (l, r) = s.run2(l, r);
            }
            fr[0] = l as f32;
            fr[1] = r as f32;
        }
    }
}

// ---- phase rotator ----------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Rotator {
    sections: Vec<Bq>,
}

impl Rotator {
    pub fn new(freq: f64, sections: u32, rate: f64) -> Self {
        Self {
            sections: (0..sections)
                .map(|_| Bq::new(Coef::allpass(freq, 0.5, rate)))
                .collect(),
        }
    }

    pub fn adopt(&mut self, old: &Rotator) {
        if self.sections.len() == old.sections.len() {
            for (a, b) in self.sections.iter_mut().zip(&old.sections) {
                a.adopt(b);
            }
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let (mut l, mut r) = (f64::from(fr[0]), f64::from(fr[1]));
            for s in &mut self.sections {
                (l, r) = s.run2(l, r);
            }
            fr[0] = l as f32;
            fr[1] = r as f32;
        }
    }
}

// ---- stereo enhancer --------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Stereo {
    width: f32,
}

impl Stereo {
    pub fn new(width: f64) -> Self {
        Self {
            width: width as f32,
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let m = 0.5 * (fr[0] + fr[1]);
            let s = 0.5 * (fr[0] - fr[1]) * self.width;
            fr[0] = m + s;
            fr[1] = m - s;
        }
    }
}

// ---- bass enhancer ----------------------------------------------------------------------------------

/// Takes the bass below `freq` (mid channel), drives it into a saturating curve, keeps only what the curve
/// added above 1.5 times the frequency and mixes it back in at `amount`.
#[derive(Clone, Debug)]
pub struct Bass {
    lp: [Bq; 2],
    hp: [Bq; 2],
    drive: f64,
    amount: f64,
}

impl Bass {
    pub fn new(freq: f64, amount_pct: f64, drive: f64, rate: f64) -> Self {
        let lp = Coef::lowpass(freq, Q_BUTTER, rate);
        let hp = Coef::highpass(freq * 1.5, Q_BUTTER, rate);
        Self {
            lp: [Bq::new(lp), Bq::new(lp)],
            hp: [Bq::new(hp), Bq::new(hp)],
            drive,
            amount: amount_pct / 100.0 / drive,
        }
    }

    pub fn adopt(&mut self, old: &Bass) {
        for i in 0..2 {
            self.lp[i].adopt(&old.lp[i]);
            self.hp[i].adopt(&old.hp[i]);
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for fr in buf.chunks_exact_mut(2) {
            let m = 0.5 * (f64::from(fr[0]) + f64::from(fr[1]));
            let b0 = self.lp[0].run(0, m);
            let b = self.lp[1].run(0, b0);
            let x = self.drive * b;
            // Odd harmonics from the saturation, even ones from the rectified part.
            let h = 0.6 * x.tanh() + 0.4 * (x.abs().tanh());
            let h0 = self.hp[0].run(0, h);
            let h = self.hp[1].run(0, h0) * self.amount;
            fr[0] += h as f32;
            fr[1] += h as f32;
        }
    }
}

// ---- equalizer --------------------------------------------------------------------------------------

/// The kind of one equalizer band (the schema's `b<n>_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BandKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    BandPass,
}

impl BandKind {
    fn parse(s: &str) -> BandKind {
        match s {
            "low_shelf" => BandKind::LowShelf,
            "high_shelf" => BandKind::HighShelf,
            "low_pass" => BandKind::LowPass,
            "high_pass" => BandKind::HighPass,
            "notch" => BandKind::Notch,
            "band_pass" => BandKind::BandPass,
            _ => BandKind::Peak,
        }
    }

    /// Whether the band's gain does anything (and a band at 0 dB is then an exact identity).
    fn has_gain(self) -> bool {
        matches!(
            self,
            BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf
        )
    }
}

/// The coefficients of one band (RBJ cookbook), shared by the equalizer and the tests that hold the Processing
/// page's curve to it.
pub fn eq_band_coef(kind: &str, hz: f64, db: f64, q: f64, rate: f64) -> Coef {
    match BandKind::parse(kind) {
        BandKind::Peak => Coef::peaking(hz, q, db, rate),
        BandKind::LowShelf => Coef::low_shelf(hz, q, db, rate),
        BandKind::HighShelf => Coef::high_shelf(hz, q, db, rate),
        BandKind::LowPass => Coef::lowpass(hz, q, rate),
        BandKind::HighPass => Coef::highpass(hz, q, rate),
        BandKind::Notch => Coef::notch(hz, q, rate),
        BandKind::BandPass => Coef::bandpass(hz, q, rate),
    }
}

/// The parametric equalizer: up to eight bands in series. A band that is off, or a bell or shelf at 0 dB, is
/// `None` (an exact identity, not run).
#[derive(Clone, Debug)]
pub struct Eq {
    bands: Vec<Option<(BandKind, Bq)>>,
}

impl Eq {
    pub fn new(cfg: &ChainConfig, rate: f64) -> Self {
        let count = (cfg.num("eq", "bands") as usize).clamp(1, EQ_MAX_BANDS);
        let bands = (1..=count)
            .map(|n| {
                let key = |s: &str| format!("b{n}_{s}");
                if !cfg.flag("eq", &key("on")) {
                    return None;
                }
                let kind = BandKind::parse(&cfg.text("eq", &key("type")));
                let (hz, db, q) = (
                    cfg.num("eq", &key("hz")),
                    cfg.num("eq", &key("db")),
                    cfg.num("eq", &key("q")),
                );
                if kind.has_gain() && db.abs() <= 1e-3 {
                    return None;
                }
                Some((
                    kind,
                    Bq::new(eq_band_coef(&cfg.text("eq", &key("type")), hz, db, q, rate)),
                ))
            })
            .collect();
        Self { bands }
    }

    pub fn is_identity(&self) -> bool {
        self.bands.iter().all(Option::is_none)
    }

    /// Carry the filters' memory across a change, band by band, where a band keeps its kind.
    pub fn adopt(&mut self, old: &Eq) {
        for (a, b) in self.bands.iter_mut().zip(&old.bands) {
            if let (Some((ka, a)), Some((kb, b))) = (a, b) {
                if ka == kb {
                    a.adopt(b);
                }
            }
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.is_identity() {
            return;
        }
        for fr in buf.chunks_exact_mut(2) {
            let (mut l, mut r) = (f64::from(fr[0]), f64::from(fr[1]));
            for (_, b) in self.bands.iter_mut().flatten() {
                (l, r) = b.run2(l, r);
            }
            fr[0] = l as f32;
            fr[1] = r as f32;
        }
    }
}

// ---- clipper ----------------------------------------------------------------------------------------

/// A soft clipper: linear up to `start`, then a `tanh` knee that approaches `thr` and never passes it.
#[derive(Clone, Debug)]
pub struct Clipper {
    thr: f32,
    start: f32,
    /// The largest reduction (dB, 0 or more) since the meters last asked.
    gr_max: f32,
}

impl Clipper {
    pub fn new(thr_db: f64, soft_db: f64) -> Self {
        let thr = db_to_lin(thr_db) as f32;
        let start = thr * db_to_lin(-soft_db) as f32;
        Self {
            thr,
            start,
            gr_max: 0.0,
        }
    }

    pub fn adopt(&mut self, _old: &Clipper) {}

    pub fn take_gr(&mut self) -> f32 {
        std::mem::replace(&mut self.gr_max, 0.0)
    }

    #[inline]
    fn shape(&self, x: f32) -> f32 {
        let a = x.abs();
        if a <= self.start {
            return x;
        }
        let room = self.thr - self.start;
        if room <= 0.0 {
            return self.thr.copysign(x);
        }
        (self.start + room * ((a - self.start) / room).tanh()).copysign(x)
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        let mut worst = 1.0f32;
        for v in buf.iter_mut() {
            let a = v.abs();
            if a > self.start {
                let y = self.shape(*v);
                let ratio = y.abs() / a;
                if ratio < worst {
                    worst = ratio;
                }
                *v = y;
            }
        }
        if worst < 1.0 {
            let gr = -20.0 * worst.max(1e-6).log10();
            if gr > self.gr_max {
                self.gr_max = gr;
            }
        }
    }
}
