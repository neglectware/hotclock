//! Final sample conversion (architecture 1.3): `f32` to `S32_LE` without dither, `S24_3LE`
//! without dither, `S16_LE` with TPDF dither of one LSB peak each side. The output buffer is
//! allocated once, at construction, so `convert` is allocation-free.

use common::config::SampleFormat;

/// Small deterministic xorshift generator for the dither. Not for anything else.
#[derive(Clone)]
pub struct Xorshift64(u64);

impl Xorshift64 {
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in [-0.5, 0.5).
    #[inline]
    pub fn uniform_half(&mut self) -> f32 {
        // 24 random bits.
        ((self.next_u64() >> 40) as f32) * (1.0 / 16_777_216.0) - 0.5
    }
}

pub struct Converter {
    format: SampleFormat,
    out: Vec<u8>,
    rng: Xorshift64,
}

impl Converter {
    /// `max_samples` is the largest slice (in samples, not frames) `convert` will be given.
    pub fn new(format: SampleFormat, max_samples: usize) -> Self {
        Self {
            format,
            out: vec![0; max_samples * format.bytes_per_sample()],
            rng: Xorshift64::new(0x00C0_FFEE),
        }
    }

    pub fn format(&self) -> SampleFormat {
        self.format
    }

    /// Convert interleaved samples. The returned slice is valid until the next call.
    pub fn convert(&mut self, samples: &[f32]) -> &[u8] {
        let bps = self.format.bytes_per_sample();
        let n = samples.len();
        assert!(n * bps <= self.out.len(), "converter buffer too small");
        match self.format {
            SampleFormat::S32Le => {
                for (i, &s) in samples.iter().enumerate() {
                    let v = quantize(s as f64 * 2_147_483_647.0, 2_147_483_647.0);
                    self.out[i * 4..i * 4 + 4].copy_from_slice(&(v as i32).to_le_bytes());
                }
            }
            SampleFormat::S24_3Le => {
                for (i, &s) in samples.iter().enumerate() {
                    let v = quantize(s as f64 * 8_388_607.0, 8_388_607.0) as i32;
                    let b = v.to_le_bytes();
                    self.out[i * 3..i * 3 + 3].copy_from_slice(&b[..3]);
                }
            }
            SampleFormat::S16Le => {
                for (i, &s) in samples.iter().enumerate() {
                    // TPDF: the sum of two independent uniform variables of one LSB width, so
                    // the peak is one LSB each side.
                    let d = (self.rng.uniform_half() + self.rng.uniform_half()) as f64;
                    let v = quantize(s as f64 * 32_767.0 + d, 32_767.0) as i16;
                    self.out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        &self.out[..n * bps]
    }
}

#[inline]
fn quantize(x: f64, max: f64) -> f64 {
    x.round().clamp(-max - 1.0, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s32_full_scale_and_zero() {
        let mut c = Converter::new(SampleFormat::S32Le, 4);
        let b = c.convert(&[1.0, -1.0, 0.0, 2.0]).to_vec();
        assert_eq!(i32::from_le_bytes(b[0..4].try_into().unwrap()), i32::MAX);
        assert_eq!(i32::from_le_bytes(b[4..8].try_into().unwrap()), -i32::MAX);
        assert_eq!(i32::from_le_bytes(b[8..12].try_into().unwrap()), 0);
        // Out-of-range input clamps instead of wrapping.
        assert_eq!(i32::from_le_bytes(b[12..16].try_into().unwrap()), i32::MAX);
    }

    #[test]
    fn s24_packs_three_bytes_little_endian() {
        let mut c = Converter::new(SampleFormat::S24_3Le, 2);
        let b = c.convert(&[1.0, -1.0]).to_vec();
        assert_eq!(b.len(), 6);
        assert_eq!(b[..3], [0xFF, 0xFF, 0x7F]);
        // -8388607 = 0xFF800001
        assert_eq!(b[3..], [0x01, 0x00, 0x80]);
    }

    #[test]
    fn s16_dither_is_at_most_one_lsb_and_zero_mean() {
        let mut c = Converter::new(SampleFormat::S16Le, 100_000);
        let input = vec![0.25f32; 100_000];
        let b = c.convert(&input).to_vec();
        let ideal = 0.25 * 32_767.0;
        let (mut sum, mut max_dev) = (0.0f64, 0.0f64);
        for ch in b.chunks_exact(2) {
            let v = i16::from_le_bytes([ch[0], ch[1]]) as f64;
            sum += v - ideal;
            max_dev = max_dev.max((v - ideal).abs());
        }
        let mean = sum / 100_000.0;
        // Rounding adds up to half an LSB to the +-1 LSB dither: at most 1.5 LSB total.
        assert!(
            max_dev <= 1.5 + 1e-9,
            "max deviation {max_dev} LSB (limit 1.5)"
        );
        assert!(mean.abs() < 0.02, "mean error {mean} LSB (limit 0.02)");
    }

    #[test]
    fn s16_silence_stays_within_dither() {
        let mut c = Converter::new(SampleFormat::S16Le, 1000);
        let b = c.convert(&[0.0f32; 1000]).to_vec();
        for ch in b.chunks_exact(2) {
            let v = i16::from_le_bytes([ch[0], ch[1]]);
            assert!(v.abs() <= 1, "{v}");
        }
    }
}
