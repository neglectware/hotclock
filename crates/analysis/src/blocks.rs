//! The streaming front end of the analysis pass. Audio arrives in any packet size; it is reduced
//! to **10 ms base blocks** (block `i` covers frames `[i*rate/100, (i+1)*rate/100)`, integer
//! arithmetic, so 44.1 kHz gives exactly 441 frames). Every envelope of architecture 3.2 is a
//! sliding sum over these blocks:
//!
//! * `E10` unweighted RMS, max over channels, one block per sample;
//! * `E50` the same over 5 blocks, hop 1 block;
//! * `K200` K-weighted mean square, channels summed with the BS.1770 weights, over 20 blocks, hop 2;
//! * any other K-weighted window (the 400 ms momentary and 3 s short-term of the ramp fit) from
//!   the stored per-block K power.

use crate::kweight::{channel_power_weights, KFilter};

/// Everything the pass keeps per 10 ms block.
#[derive(Debug, Clone, Default)]
pub struct Blocks {
    pub rate: u32,
    pub channels: usize,
    /// Frames in the file.
    pub frames: u64,
    /// Per block and channel: unweighted mean square (linear power). `ms[b * channels + c]`.
    /// The last block may be shorter than 10 ms.
    pub ms: Vec<f32>,
    /// Per block and channel: the mean sample value (the DC part), laid out as `ms`.
    pub mean: Vec<f32>,
    /// Per block: K-weighted power summed over channels with the BS.1770 weights (linear mean
    /// square, before the -0.691 offset).
    pub kpow: Vec<f64>,
    /// Per block and channel: min and max sample. `minmax[(b * channels + c) * 2 ..]`.
    pub minmax: Vec<f32>,
    /// A run of 16 or more consecutive samples at full scale was seen.
    pub clipped: bool,
    /// Largest absolute sample value.
    pub sample_peak: f32,
}

impl Blocks {
    pub fn block_count(&self) -> usize {
        self.kpow.len()
    }

    /// Frames in block `b` (10 ms, except possibly the last).
    pub fn block_frames(&self, b: usize) -> u64 {
        let r = u64::from(self.rate);
        let start = b as u64 * r / 100;
        let end = ((b as u64 + 1) * r / 100).min(self.frames);
        end - start
    }

    /// Complete 10 ms blocks (excludes a short final block).
    pub fn full_blocks(&self) -> usize {
        (self.frames * 100 / u64::from(self.rate)) as usize
    }

    pub fn file_ms(&self) -> f64 {
        self.frames as f64 * 1000.0 / f64::from(self.rate)
    }
}

pub struct BlockAnalyzer {
    blocks: Blocks,
    filters: Vec<KFilter>,
    weights: Vec<f64>,
    sq: Vec<f64>,
    sum: Vec<f64>,
    ksq: Vec<f64>,
    mn: Vec<f32>,
    mx: Vec<f32>,
    run: Vec<u32>,
    frame_in_block: u64,
    block_len: u64,
    block_idx: u64,
}

impl BlockAnalyzer {
    pub fn new(rate: u32, channels: usize) -> Self {
        let mut a = BlockAnalyzer {
            blocks: Blocks {
                rate,
                channels,
                ..Blocks::default()
            },
            filters: (0..channels)
                .map(|_| KFilter::new(f64::from(rate)))
                .collect(),
            weights: channel_power_weights(channels),
            sq: vec![0.0; channels],
            sum: vec![0.0; channels],
            ksq: vec![0.0; channels],
            mn: vec![f32::MAX; channels],
            mx: vec![f32::MIN; channels],
            run: vec![0; channels],
            frame_in_block: 0,
            block_len: 0,
            block_idx: 0,
        };
        a.block_len = a.len_of(0);
        a
    }

    fn len_of(&self, b: u64) -> u64 {
        let r = u64::from(self.blocks.rate);
        (b + 1) * r / 100 - b * r / 100
    }

    fn close_block(&mut self) {
        let n = self.frame_in_block.max(1) as f64;
        let mut k = 0.0;
        for c in 0..self.blocks.channels {
            self.blocks.ms.push((self.sq[c] / n) as f32);
            self.blocks.mean.push((self.sum[c] / n) as f32);
            k += self.weights[c] * self.ksq[c] / n;
            self.blocks.minmax.push(self.mn[c]);
            self.blocks.minmax.push(self.mx[c]);
            self.sq[c] = 0.0;
            self.sum[c] = 0.0;
            self.ksq[c] = 0.0;
            self.mn[c] = f32::MAX;
            self.mx[c] = f32::MIN;
        }
        self.blocks.kpow.push(k);
        self.frame_in_block = 0;
        self.block_idx += 1;
        self.block_len = self.len_of(self.block_idx);
    }

    /// Feed interleaved samples.
    pub fn push(&mut self, samples: &[f32]) {
        let ch = self.blocks.channels;
        for frame in samples.chunks_exact(ch) {
            for (c, &s) in frame.iter().enumerate() {
                let x = f64::from(s);
                self.sq[c] += x * x;
                self.sum[c] += x;
                let y = self.filters[c].process(x);
                self.ksq[c] += y * y;
                if s < self.mn[c] {
                    self.mn[c] = s;
                }
                if s > self.mx[c] {
                    self.mx[c] = s;
                }
                let a = s.abs();
                if a > self.blocks.sample_peak {
                    self.blocks.sample_peak = a;
                }
                if a >= 1.0 {
                    self.run[c] += 1;
                    if self.run[c] >= 16 {
                        self.blocks.clipped = true;
                    }
                } else {
                    self.run[c] = 0;
                }
            }
            self.frame_in_block += 1;
            self.blocks.frames += 1;
            if self.frame_in_block == self.block_len {
                self.close_block();
            }
        }
    }

    pub fn finish(mut self) -> Blocks {
        if self.frame_in_block > 0 {
            self.close_block();
        }
        self.blocks
    }
}

// ---- windows over the blocks ----

/// Power in dB (relative to full scale, `10 log10`) with a floor for silence.
#[inline]
pub fn db(power: f64) -> f64 {
    if power > 1e-12 {
        10.0 * power.log10()
    } else {
        -120.0
    }
}

/// LUFS from a K-weighted mean square.
#[inline]
pub fn lufs(kpow: f64) -> f64 {
    if kpow > 1e-12 {
        -0.691 + 10.0 * kpow.log10()
    } else {
        -120.0
    }
}

impl Blocks {
    /// Sliding K-weighted loudness in LUFS: `window` and `hop` in 10 ms blocks; sample `i` covers
    /// blocks `[i*hop, i*hop + window)`. Only complete windows of complete blocks.
    pub fn k_window(&self, window: usize, hop: usize) -> Vec<f32> {
        let n = self.full_blocks();
        if n < window || window == 0 {
            return vec![];
        }
        let mut out = vec![];
        let mut sum: f64 = self.kpow[..window].iter().sum();
        let mut i = 0;
        loop {
            out.push(lufs(sum / window as f64) as f32);
            let next = (i + 1) * hop;
            if next + window > n {
                break;
            }
            // Recompute from scratch when the hop is large; slide when it is 1.
            if hop == 1 {
                sum += self.kpow[next + window - 1] - self.kpow[next - 1];
            } else {
                sum = self.kpow[next..next + window].iter().sum();
            }
            i += 1;
        }
        out
    }

    /// E10 in dBFS: max over channels of the block RMS (10 ms, hop 10 ms). The last block may
    /// be short.
    pub fn e10(&self) -> Vec<f32> {
        (0..self.block_count())
            .map(|b| {
                let m = (0..self.channels)
                    .map(|c| self.ms[b * self.channels + c])
                    .fold(0.0f32, f32::max);
                db(f64::from(m)) as f32
            })
            .collect()
    }

    /// E50 in dBFS: max over channels of the 50 ms RMS, hop 10 ms; sample `i` covers blocks
    /// `[i, i+5)`. Complete blocks only.
    pub fn e50(&self) -> Vec<f32> {
        let n = self.full_blocks();
        if n < 5 {
            return vec![];
        }
        (0..=n - 5)
            .map(|i| {
                let m = (0..self.channels)
                    .map(|c| {
                        (i..i + 5)
                            .map(|b| f64::from(self.ms[b * self.channels + c]))
                            .sum::<f64>()
                            / 5.0
                    })
                    .fold(0.0f64, f64::max);
                db(m) as f32
            })
            .collect()
    }

    /// Min and max per 20 ms (two blocks) per channel, as i8 (`sample * 127`, clamped), for the
    /// waveform display. Layout: `[(w * channels + c) * 2 + {0: min, 1: max}]`.
    pub fn wave(&self) -> Vec<i8> {
        let n = self.block_count().div_ceil(2);
        let mut out = Vec::with_capacity(n * self.channels * 2);
        for w in 0..n {
            for c in 0..self.channels {
                let mut mn = f32::MAX;
                let mut mx = f32::MIN;
                for b in [2 * w, 2 * w + 1] {
                    if b < self.block_count() {
                        mn = mn.min(self.minmax[(b * self.channels + c) * 2]);
                        mx = mx.max(self.minmax[(b * self.channels + c) * 2 + 1]);
                    }
                }
                let q = |v: f32| (v * 127.0).round().clamp(-127.0, 127.0) as i8;
                out.push(q(mn));
                out.push(q(mx));
            }
        }
        out
    }
}

/// BS.1770 gated integrated loudness over K power blocks (400 ms blocks, 100 ms hop, absolute
/// gate -70 LUFS, relative gate -10 LU). `None` when nothing passes the absolute gate.
pub fn integrated_from_blocks(kpow: &[f64]) -> Option<f64> {
    const W: usize = 40;
    const HOP: usize = 10;
    if kpow.len() < W {
        return None;
    }
    let mut sum: f64 = kpow[..W].iter().sum();
    let mut powers = vec![sum / W as f64];
    let mut i = 1;
    while i * HOP + W <= kpow.len() {
        // Recompute (no drift, cheap: one sum per 100 ms).
        sum = kpow[i * HOP..i * HOP + W].iter().sum();
        powers.push(sum / W as f64);
        i += 1;
    }
    gate(&powers)
}

/// The two-stage gate over window powers (any window length).
pub fn gate(powers: &[f64]) -> Option<f64> {
    let abs_thr = 10f64.powf((-70.0 + 0.691) / 10.0);
    let above: Vec<f64> = powers.iter().copied().filter(|&p| p > abs_thr).collect();
    if above.is_empty() {
        return None;
    }
    let mean = above.iter().sum::<f64>() / above.len() as f64;
    let rel_thr = mean * 10f64.powf(-1.0);
    let kept: Vec<f64> = above.into_iter().filter(|&p| p > rel_thr).collect();
    if kept.is_empty() {
        return None;
    }
    Some(lufs(kept.iter().sum::<f64>() / kept.len() as f64))
}
