//! ITU-R BS.1770 K-weighting (a high shelf followed by the RLB high-pass), computed for any
//! sample rate with the same constants libebur128 uses.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z1: f64,
    z2: f64,
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        // Transposed direct form II.
        let y = self.b[0] * x + self.z1;
        self.z1 = self.b[1] * x - self.a[0] * y + self.z2;
        self.z2 = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// One channel's K-weighting filter.
#[derive(Clone, Debug)]
pub struct KFilter {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KFilter {
    pub fn new(rate: f64) -> KFilter {
        // Stage 1: high shelf.
        let f0 = 1681.974450955533;
        let g = 3.999843853973347;
        let q = 0.7071752369554196;
        let k = (PI * f0 / rate).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad {
            b: [
                (vh + vb * k / q + k * k) / a0,
                2.0 * (k * k - vh) / a0,
                (vh - vb * k / q + k * k) / a0,
            ],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            z1: 0.0,
            z2: 0.0,
        };
        // Stage 2: RLB high-pass.
        let f0 = 38.13547087602444;
        let q = 0.5003270373238773;
        let k = (PI * f0 / rate).tan();
        let a0 = 1.0 + k / q + k * k;
        let high_pass = Biquad {
            b: [1.0, -2.0, 1.0],
            a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
            z1: 0.0,
            z2: 0.0,
        };
        KFilter { shelf, high_pass }
    }

    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.high_pass.process(self.shelf.process(x))
    }
}

/// BS.1770 channel weights for a stream of `channels` interleaved channels. Mono is treated as
/// dual mono (the same signal in L and R, which is how playout will present it), so its weight
/// is 2 in power. Stereo is 1 and 1. Beyond two channels the layout is taken as L R C LFE Ls Rs:
/// the LFE is dropped and the surrounds weigh 1.41 in power (BS.1770's G, about +1.5 dB).
pub fn channel_power_weights(channels: usize) -> Vec<f64> {
    match channels {
        0 => vec![],
        1 => vec![2.0],
        2 => vec![1.0, 1.0],
        n => (0..n)
            .map(|i| match (n, i) {
                (_, 0..=2) => 1.0,
                (6.., 3) => 0.0,
                (6.., 4 | 5) => 1.41,
                (_, _) => 1.0,
            })
            .collect(),
    }
}
