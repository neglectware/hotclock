//! Filter primitives of the processing chain: biquads (RBJ cookbook, `f64` state, transposed direct form II)
//! applied to a stereo pair. Everything here is allocation free after construction.

use std::f64::consts::{PI, SQRT_2};

/// Coefficients of one second-order section, `a0` normalized to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coef {
    pub b: [f64; 3],
    pub a: [f64; 2],
}

impl Coef {
    pub const IDENTITY: Coef = Coef {
        b: [1.0, 0.0, 0.0],
        a: [0.0, 0.0],
    };

    fn norm(b: [f64; 3], a: [f64; 3]) -> Coef {
        Coef {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
        }
    }

    fn w(f: f64, rate: f64) -> (f64, f64) {
        // Keep the corner under Nyquist so a design at a high rate never goes unstable.
        let f = f.clamp(1.0, rate * 0.49);
        let w0 = 2.0 * PI * f / rate;
        (w0.cos(), w0.sin())
    }

    pub fn lowpass(f: f64, q: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let al = s / (2.0 * q);
        Self::norm(
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    pub fn highpass(f: f64, q: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let al = s / (2.0 * q);
        Self::norm(
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    pub fn allpass(f: f64, q: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let al = s / (2.0 * q);
        Self::norm(
            [1.0 - al, -2.0 * c, 1.0 + al],
            [1.0 + al, -2.0 * c, 1.0 - al],
        )
    }

    /// A first-order high-pass (bilinear), as a section with zero second-order terms.
    pub fn highpass1(f: f64, rate: f64) -> Coef {
        let k = (PI * f.clamp(1.0, rate * 0.49) / rate).tan();
        let n = 1.0 / (1.0 + k);
        Coef {
            b: [n, -n, 0.0],
            a: [(k - 1.0) * n, 0.0],
        }
    }

    pub fn peaking(f: f64, q: f64, db: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let a = 10f64.powf(db / 40.0);
        let al = s / (2.0 * q);
        Self::norm(
            [1.0 + al * a, -2.0 * c, 1.0 - al * a],
            [1.0 + al / a, -2.0 * c, 1.0 - al / a],
        )
    }

    /// The shelf's `alpha`: `sin(w0) / (2 Q)`. At the Butterworth Q (RBJ shelf slope 1, what the fixed four-band
    /// equalizer used) it is evaluated the way that equalizer did, so its audio is unchanged to the bit.
    fn shelf_alpha(s: f64, q: f64) -> f64 {
        if (q - Q_BUTTER).abs() < 1e-12 {
            s / 2.0 * SQRT_2
        } else {
            s / (2.0 * q)
        }
    }

    pub fn low_shelf(f: f64, q: f64, db: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let a = 10f64.powf(db / 40.0);
        let al = Self::shelf_alpha(s, q);
        let t = 2.0 * a.sqrt() * al;
        Self::norm(
            [
                a * ((a + 1.0) - (a - 1.0) * c + t),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - t),
            ],
            [
                (a + 1.0) + (a - 1.0) * c + t,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - t,
            ],
        )
    }

    pub fn high_shelf(f: f64, q: f64, db: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let a = 10f64.powf(db / 40.0);
        let al = Self::shelf_alpha(s, q);
        let t = 2.0 * a.sqrt() * al;
        Self::norm(
            [
                a * ((a + 1.0) + (a - 1.0) * c + t),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - t),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + t,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - t,
            ],
        )
    }

    /// The RBJ notch.
    pub fn notch(f: f64, q: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let al = s / (2.0 * q);
        Self::norm([1.0, -2.0 * c, 1.0], [1.0 + al, -2.0 * c, 1.0 - al])
    }

    /// The RBJ band-pass with a constant 0 dB peak gain.
    pub fn bandpass(f: f64, q: f64, rate: f64) -> Coef {
        let (c, s) = Self::w(f, rate);
        let al = s / (2.0 * q);
        Self::norm([al, 0.0, -al], [1.0 + al, -2.0 * c, 1.0 - al])
    }
}

/// Butterworth Q of a second-order section.
pub const Q_BUTTER: f64 = 1.0 / SQRT_2;

/// One section for both channels.
#[derive(Clone, Copy, Debug)]
pub struct Bq {
    pub c: Coef,
    z: [[f64; 2]; 2],
}

impl Bq {
    pub fn new(c: Coef) -> Self {
        Self {
            c,
            z: [[0.0; 2]; 2],
        }
    }

    #[inline]
    pub fn run(&mut self, ch: usize, x: f64) -> f64 {
        let c = &self.c;
        let z = &mut self.z[ch];
        let y = c.b[0] * x + z[0];
        z[0] = c.b[1] * x - c.a[0] * y + z[1];
        z[1] = c.b[2] * x - c.a[1] * y;
        y
    }

    #[inline]
    pub fn run2(&mut self, l: f64, r: f64) -> (f64, f64) {
        (self.run(0, l), self.run(1, r))
    }

    /// Copy the filter's memory from another section (a swap carries the state across).
    pub fn adopt(&mut self, old: &Bq) {
        self.z = old.z;
    }

    pub fn reset(&mut self) {
        self.z = [[0.0; 2]; 2];
    }
}

/// The magnitude response of a section at `f` (for tests and the model).
pub fn magnitude(c: &Coef, f: f64, rate: f64) -> f64 {
    let w = 2.0 * PI * f / rate;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let nr = c.b[0] + c.b[1] * c1 + c.b[2] * c2;
    let ni = -(c.b[1] * s1 + c.b[2] * s2);
    let dr = 1.0 + c.a[0] * c1 + c.a[1] * c2;
    let di = -(c.a[0] * s1 + c.a[1] * s2);
    (nr.hypot(ni)) / (dr.hypot(di))
}

pub fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

pub fn lin_to_db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// A one-pole smoothing coefficient for a time constant `ms` at `rate` (`y += c * (x - y)`).
pub fn smooth_coef(ms: f64, rate: f64) -> f64 {
    if ms <= 0.0 {
        1.0
    } else {
        1.0 - (-1.0 / (ms * 0.001 * rate)).exp()
    }
}
