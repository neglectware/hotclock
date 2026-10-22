//! `hotclock-dsp-model`: the reference response of the multiband compressor's band filters, and the live gain
//! curve the status display draws from them (architecture 11.2a).
//!
//! The effective response the compressor applies at an instant is the complex sum of the band filters, each
//! scaled by its current gain: `H(f) = | sum_i g_i B_i(f) |` with `g_i = 10^(GR_i / 20)`. The bands are in phase
//! across the crossovers, so the complex sum is right where a power sum would hide a dip or a bump.
//!
//! `B_i` is the cascaded Linkwitz-Riley split: crossover `k` is a 4th-order low-pass/high-pass pair (two
//! cascaded 2nd-order Butterworth sections each, `LP_k` and `HP_k`), the signal above a crossover goes on to the
//! next one, and a band that skips a later crossover gets that crossover's all-pass (`LP_k + HP_k`) so every band
//! keeps the same phase. For `n` bands and crossovers `c_0 .. c_(n-2)`:
//!
//! `B_i = HP_0 ... HP_(i-1) x LP_i x AP_(i+1) ... AP_(n-2)`, with `LP_(n-1) = 1`.
//!
//! With every gain at 0 dB the sum is the all-pass `AP_0 ... AP_(n-2)`: `|H| = 1` at every frequency.

use serde_json::json;
use std::ops::{Add, Div, Mul};

/// The frequency axis of the processing view: 20 Hz to 20 kHz, `x = log10(f / 20) / 3` of the zone width.
pub const F_MIN: f64 = 20.0;
pub const F_MAX: f64 = 20_000.0;

/// The x position of `f` as a fraction of the zone width (0 at 20 Hz, 1 at 20 kHz).
pub fn x_frac(f: f64) -> f64 {
    (f / F_MIN).log10() / 3.0
}

/// The frequency at a fraction of the width: the inverse of [`x_frac`].
pub fn freq_at(x: f64) -> f64 {
    F_MIN * 10f64.powf(3.0 * x)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C {
    pub re: f64,
    pub im: f64,
}

impl C {
    pub const ONE: C = C { re: 1.0, im: 0.0 };
    pub fn scale(self, k: f64) -> C {
        C {
            re: self.re * k,
            im: self.im * k,
        }
    }
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
}

impl std::ops::Mul for C {
    type Output = C;
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}

impl std::ops::Add for C {
    type Output = C;
    fn add(self, o: C) -> C {
        C {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

impl std::ops::Div for C {
    type Output = C;
    fn div(self, o: C) -> C {
        let d = o.re * o.re + o.im * o.im;
        C {
            re: (self.re * o.re + self.im * o.im) / d,
            im: (self.im * o.re - self.re * o.im) / d,
        }
    }
}

/// The 2nd-order Butterworth low-pass and high-pass at `fc`, evaluated at `f`:
/// `LP2 = 1 / (s^2 + sqrt2 s + 1)`, `HP2 = s^2 / (s^2 + sqrt2 s + 1)`, `s = j f / fc`.
fn butter2(fc: f64, f: f64) -> (C, C) {
    let w = f / fc;
    // s = j w; s^2 = -w^2
    let den = C {
        re: 1.0 - w * w,
        im: std::f64::consts::SQRT_2 * w,
    };
    let lp = C::ONE.div(den);
    let hp = C {
        re: -w * w,
        im: 0.0,
    }
    .div(den);
    (lp, hp)
}

/// The Linkwitz-Riley 4th-order pair at `fc`: each Butterworth section twice.
fn lr4(fc: f64, f: f64) -> (C, C) {
    let (lp, hp) = butter2(fc, f);
    (lp.mul(lp), hp.mul(hp))
}

/// The complex response of band `band` (0 = LOW) of `crossovers.len() + 1` bands at `f`.
pub fn band_response(crossovers: &[f64], band: usize, f: f64) -> C {
    let n = crossovers.len() + 1;
    assert!(band < n, "band {band} of {n}");
    let mut b = C::ONE;
    for (k, &fc) in crossovers.iter().enumerate() {
        let (lp, hp) = lr4(fc, f);
        b = if k < band {
            b.mul(hp)
        } else if k == band {
            b.mul(lp)
        } else {
            b.mul(lp.add(hp))
        };
    }
    b
}

/// The band filters evaluated once on a frequency grid: reused for every frame, so a frame costs
/// `bands x points` multiply-adds.
#[derive(Clone, Debug)]
pub struct BandTable {
    pub freqs: Vec<f64>,
    /// `resp[band][point]`
    pub resp: Vec<Vec<C>>,
}

impl BandTable {
    pub fn new(crossovers: &[f64], freqs: Vec<f64>) -> BandTable {
        let n = crossovers.len() + 1;
        let resp = (0..n)
            .map(|b| {
                freqs
                    .iter()
                    .map(|&f| band_response(crossovers, b, f))
                    .collect()
            })
            .collect();
        BandTable { freqs, resp }
    }

    pub fn bands(&self) -> usize {
        self.resp.len()
    }

    /// `20 log10 | sum_i g_i B_i(f) |` at every grid point. `gr_db[i]` is band i's gain reduction (0 or
    /// negative); `makeup_db[i]` is added to the band's gain when the caller wants the makeup in the curve
    /// (`display.curve_include_makeup`), empty otherwise. A missing entry reads as 0 dB.
    pub fn curve_db(&self, gr_db: &[f64], makeup_db: &[f64]) -> Vec<f64> {
        let g: Vec<f64> = (0..self.bands())
            .map(|i| {
                let db =
                    gr_db.get(i).copied().unwrap_or(0.0) + makeup_db.get(i).copied().unwrap_or(0.0);
                10f64.powf(db / 20.0)
            })
            .collect();
        (0..self.freqs.len())
            .map(|p| {
                let mut s = C { re: 0.0, im: 0.0 };
                for (i, gi) in g.iter().enumerate() {
                    s = s.add(self.resp[i][p].scale(*gi));
                }
                20.0 * s.abs().max(1e-12).log10()
            })
            .collect()
    }
}

/// `n` log-spaced frequencies from 20 Hz to 20 kHz inclusive.
pub fn log_freqs(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| freq_at(i as f64 / (n.max(2) - 1) as f64))
        .collect()
}

/// The curve in dB at a single frequency (the reference the display tests compare against).
pub fn h_db(crossovers: &[f64], gr_db: &[f64], makeup_db: &[f64], f: f64) -> f64 {
    let mut s = C { re: 0.0, im: 0.0 };
    for i in 0..=crossovers.len() {
        let db = gr_db.get(i).copied().unwrap_or(0.0) + makeup_db.get(i).copied().unwrap_or(0.0);
        s = s.add(band_response(crossovers, i, f).scale(10f64.powf(db / 20.0)));
    }
    20.0 * s.abs().max(1e-12).log10()
}

/// The shared test vectors, as JSON: the JavaScript port of the `/display` page is checked against the same
/// numbers (`crates/serve/web/curve.js`, `dev/curve-vectors.mjs`).
pub fn vectors() -> serde_json::Value {
    let cases: [(&str, Vec<f64>, Vec<f64>); 6] = [
        (
            "5 bands, GR -6 0 0 0 0",
            vec![120.0, 500.0, 2000.0, 6000.0],
            vec![-6.0, 0.0, 0.0, 0.0, 0.0],
        ),
        (
            "5 bands, GR -3 -6 -9 -6 -3",
            vec![120.0, 500.0, 2000.0, 6000.0],
            vec![-3.0, -6.0, -9.0, -6.0, -3.0],
        ),
        (
            "5 bands, GR -12 all",
            vec![120.0, 500.0, 2000.0, 6000.0],
            vec![-12.0; 5],
        ),
        (
            "5 bands, GR 0 all",
            vec![120.0, 500.0, 2000.0, 6000.0],
            vec![0.0; 5],
        ),
        (
            "3 bands, GR -4 -8 -2",
            vec![200.0, 3000.0],
            vec![-4.0, -8.0, -2.0],
        ),
        (
            "6 bands, GR -1 -2 -3 -4 -5 -6",
            vec![100.0, 400.0, 1200.0, 3500.0, 9000.0],
            vec![-1.0, -2.0, -3.0, -4.0, -5.0, -6.0],
        ),
    ];
    let freqs = log_freqs(24);
    json!(cases
        .iter()
        .map(|(name, xo, gr)| json!({
            "name": name, "crossovers": xo, "gr": gr, "freqs": freqs,
            "db": freqs.iter().map(|&f| h_db(xo, gr, &[], f)).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::{Add, Div, Mul};

    #[test]
    fn all_zero_is_flat() {
        for xo in [
            vec![120.0, 500.0, 2000.0, 6000.0],
            vec![200.0, 3000.0],
            vec![100.0, 400.0, 1200.0, 3500.0, 9000.0],
        ] {
            let gr = vec![0.0; xo.len() + 1];
            for f in log_freqs(200) {
                assert!(h_db(&xo, &gr, &[], f).abs() < 1e-9, "{f} Hz");
            }
        }
    }

    #[test]
    fn uniform_gain_is_that_gain() {
        let xo = [120.0, 500.0, 2000.0, 6000.0];
        for f in log_freqs(100) {
            assert!((h_db(&xo, &[-12.0; 5], &[], f) + 12.0).abs() < 1e-9);
        }
    }

    #[test]
    fn band_edges_are_minus_six_db() {
        // LR4: each side of a crossover is 6 dB down at fc.
        let xo = [1000.0];
        let lp = band_response(&xo, 0, 1000.0).abs();
        assert!((20.0 * lp.log10() + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn single_band_cut_shapes_the_curve() {
        let xo = [120.0, 500.0, 2000.0, 6000.0];
        let gr = [-6.0, 0.0, 0.0, 0.0, 0.0];
        assert!((h_db(&xo, &gr, &[], 20.0) + 6.0).abs() < 0.05);
        assert!(h_db(&xo, &gr, &[], 3000.0).abs() < 0.05);
        // the crossover itself is a smooth blend between the two gains
        let at = h_db(&xo, &gr, &[], 120.0);
        assert!(at < -2.0 && at > -4.5, "{at}");
    }

    #[test]
    fn table_matches_the_pointwise_function() {
        let xo = [120.0, 500.0, 2000.0, 6000.0];
        let gr = [-3.0, -6.0, -9.0, -6.0, -3.0];
        let t = BandTable::new(&xo, log_freqs(64));
        let c = t.curve_db(&gr, &[]);
        for (i, &f) in t.freqs.iter().enumerate() {
            assert!((c[i] - h_db(&xo, &gr, &[], f)).abs() < 1e-12);
        }
    }

    #[test]
    fn x_frac_round_trips() {
        assert!(x_frac(20.0).abs() < 1e-12);
        assert!((x_frac(20_000.0) - 1.0).abs() < 1e-12);
        assert!((freq_at(x_frac(1234.0)) - 1234.0).abs() < 1e-9);
    }

    /// The cascade built here is checked against a second, independent construction: the same bands made from
    /// time-domain-free polynomial arithmetic, with the numerator and denominator of every section multiplied out.
    #[test]
    fn independent_polynomial_form() {
        // Evaluate LP4 and HP4 as ratios of explicit polynomials in s.
        fn lp4(fc: f64, f: f64) -> C {
            let w = f / fc;
            let s = C { re: 0.0, im: w };
            let s2 = s.mul(s);
            let r2 = C {
                re: std::f64::consts::SQRT_2,
                im: 0.0,
            };
            let d = s2.add(r2.mul(s)).add(C::ONE);
            C::ONE.div(d.mul(d))
        }
        fn hp4(fc: f64, f: f64) -> C {
            let w = f / fc;
            let s = C { re: 0.0, im: w };
            let s2 = s.mul(s);
            let r2 = C {
                re: std::f64::consts::SQRT_2,
                im: 0.0,
            };
            let d = s2.add(r2.mul(s)).add(C::ONE);
            s2.mul(s2).div(d.mul(d))
        }
        let xo = [120.0, 500.0, 2000.0, 6000.0];
        for f in log_freqs(50) {
            // band 2 (middle): HP0 HP1 LP2 AP3
            let b = hp4(120.0, f)
                .mul(hp4(500.0, f))
                .mul(lp4(2000.0, f))
                .mul(lp4(6000.0, f).add(hp4(6000.0, f)));
            let m = band_response(&xo, 2, f);
            assert!((b.re - m.re).abs() < 1e-9 && (b.im - m.im).abs() < 1e-9);
        }
    }
}
