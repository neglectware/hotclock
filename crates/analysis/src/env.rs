//! Envelope storage (architecture 3.2, 5.2). Every envelope is stored as `i16` in **0.1 dB
//! steps** (the design says `i8`; 0.1 dB keeps a recompute from the stored envelope within a few
//! milliseconds of a full analysis, and zstd makes the size irrelevant), compressed with zstd.
//!
//! Timing of the samples (ms from the start of the audio):
//! * `e10[b]`  covers `[10b, 10b + 10)` (the last block may be shorter);
//! * `e50[i]`  covers `[10i, 10i + 50)`;
//! * `k200[k]` covers `[20k, 20k + 200)`;
//! * `k10[b]`  is the K-weighted loudness of block `b`, `[10b, 10b + 10)`. It is the source of the
//!   ramps (100 ms blocks under a cue window start at the window, so they cannot come from `k200`).

use crate::blocks::{lufs, Blocks};

/// Floor of every stored value, dB.
pub const FLOOR_DB: f32 = -120.0;

pub fn quantize(v: &[f32]) -> Vec<i16> {
    v.iter()
        .map(|x| (x * 10.0).round().clamp(-1200.0, 1200.0) as i16)
        .collect()
}

pub fn dequantize(v: &[i16]) -> Vec<f32> {
    v.iter().map(|&x| f32::from(x) / 10.0).collect()
}

pub fn pack(v: &[i16]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(v.len() * 2);
    for x in v {
        raw.extend_from_slice(&x.to_le_bytes());
    }
    zstd::encode_all(&raw[..], 3).unwrap_or_default()
}

pub fn unpack(b: &[u8]) -> Result<Vec<i16>, String> {
    let raw = zstd::decode_all(b).map_err(|e| format!("envelope blob: {e}"))?;
    Ok(raw
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

/// The envelopes of one file in dequantized form (what every marker computation reads).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Envelopes {
    pub e10: Vec<f32>,
    pub e50: Vec<f32>,
    pub k200: Vec<f32>,
    pub k10: Vec<f32>,
}

impl Envelopes {
    /// From the analysis blocks, going through the same quantization the database applies, so
    /// a fresh analysis and a recompute from storage read identical values.
    pub fn from_blocks(b: &Blocks) -> Envelopes {
        let k10: Vec<f32> = b.kpow.iter().map(|&p| lufs(p) as f32).collect();
        Envelopes {
            e10: dequantize(&quantize(&b.e10())),
            e50: dequantize(&quantize(&b.e50())),
            k200: dequantize(&quantize(&b.k_window(20, 2))),
            k10: dequantize(&quantize(&k10)),
        }
    }

    pub fn to_stored(&self) -> StoredEnvelopes {
        StoredEnvelopes {
            e10: pack(&quantize(&self.e10)),
            e50: pack(&quantize(&self.e50)),
            k200: pack(&quantize(&self.k200)),
            k10: pack(&quantize(&self.k10)),
        }
    }

    /// Per-block K power (linear mean square incl. the -0.691 offset removed) rebuilt from `k10`,
    /// for the ramps and the exact-enough window loudness.
    pub fn kpow(&self) -> Vec<f64> {
        self.k10
            .iter()
            .map(|&l| {
                if l <= FLOOR_DB + 0.05 {
                    0.0
                } else {
                    10f64.powf((f64::from(l) + 0.691) / 10.0)
                }
            })
            .collect()
    }
}

/// The compressed blobs as stored in `analysis.env_*`.
#[derive(Debug, Clone, Default)]
pub struct StoredEnvelopes {
    pub e10: Vec<u8>,
    pub e50: Vec<u8>,
    pub k200: Vec<u8>,
    pub k10: Vec<u8>,
}

impl StoredEnvelopes {
    pub fn load(&self) -> Result<Envelopes, String> {
        Ok(Envelopes {
            e10: dequantize(&unpack(&self.e10)?),
            e50: dequantize(&unpack(&self.e50)?),
            k200: dequantize(&unpack(&self.k200)?),
            k10: dequantize(&unpack(&self.k10)?),
        })
    }
}
