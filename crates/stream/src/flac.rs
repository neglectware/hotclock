//! FLAC frames from `flacenc` (pure Rust, no C dependency), one fixed-size block at a time.
//!
//! The stream is 24-bit stereo. A float sample becomes `round(x * 2^23)` clamped to the 24-bit range
//! (so full scale, 1.0, is 2^23 - 1 after the clamp), which is exact for every sample a 24-bit
//! converter could carry; the stream is as lossless as a 24-bit DAC feed would be.

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::config::Encoder;
use flacenc::error::Verified;
use flacenc::error::Verify;
use flacenc::source::{Fill, FrameBuf};

pub const BITS: usize = 24;

/// The 24-bit integer a float sample becomes in the stream.
#[inline]
pub fn to_i24(x: f32) -> i32 {
    // NaN converts to 0 (`as` saturates and maps NaN to 0).
    (x * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32
}

pub struct FlacEncoder {
    cfg: Verified<Encoder>,
    info: StreamInfo,
    fb: FrameBuf,
    block: usize,
    frame_no: usize,
}

/// The frame number is coded in at most 31 bits.
const MAX_FRAMES: usize = (1 << 31) - 1;

impl FlacEncoder {
    /// `level` is 0 to 8: the number of prediction orders tried, roughly as `flac -0` to `flac -8`.
    pub fn new(rate: u32, block: usize, level: u8) -> Result<Self, String> {
        let mut c = Encoder::default();
        c.multithread = false;
        c.block_size = block;
        let order = match level {
            0 => 0,
            1 => 4,
            2 => 6,
            3 => 8,
            4 => 10,
            5 => 12,
            6 => 16,
            7 => 20,
            _ => 24,
        };
        if order == 0 {
            c.subframe_coding.use_lpc = false;
        } else {
            c.subframe_coding.qlpc.lpc_order = order;
        }
        let cfg = c.into_verified().map_err(|(_, e)| format!("{e}"))?;
        let info = StreamInfo::new(rate as usize, 2, BITS).map_err(|e| format!("{e}"))?;
        let fb = FrameBuf::with_size(2, block).map_err(|e| format!("{e}"))?;
        Ok(Self {
            cfg,
            info,
            fb,
            block,
            frame_no: 0,
        })
    }

    pub fn block(&self) -> usize {
        self.block
    }

    /// Continue an existing stream's frame numbering (after a restart).
    pub fn set_frame_number(&mut self, n: usize) {
        self.frame_no = n;
    }

    /// True when the frame counter is about to overflow: the caller starts a new stream.
    pub fn exhausted(&self) -> bool {
        self.frame_no >= MAX_FRAMES
    }

    /// Encode exactly one block of interleaved stereo 24-bit samples into one FLAC frame.
    pub fn encode_block(&mut self, interleaved: &[i32]) -> Result<Vec<u8>, String> {
        debug_assert_eq!(interleaved.len(), self.block * 2);
        self.fb
            .fill_interleaved(interleaved)
            .map_err(|e| format!("{e}"))?;
        let frame =
            flacenc::encode_fixed_size_frame(&self.cfg, &self.fb, self.frame_no, &self.info)
                .map_err(|e| format!("{e}"))?;
        self.frame_no += 1;
        let mut sink = ByteSink::with_capacity(self.block * 6 * 8);
        frame.write(&mut sink).map_err(|e| format!("{e}"))?;
        Ok(sink.as_slice().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_clamps_and_rounds() {
        assert_eq!(to_i24(0.0), 0);
        assert_eq!(to_i24(1.0), 8_388_607);
        assert_eq!(to_i24(-1.0), -8_388_608);
        assert_eq!(to_i24(2.0), 8_388_607);
        assert_eq!(to_i24(f32::NAN), 0);
        assert_eq!(to_i24(0.5), 4_194_304);
        assert_eq!(to_i24(1.0 / 8_388_608.0), 1);
    }

    #[test]
    fn every_level_encodes_a_block() {
        let block: Vec<i32> = (0..2048)
            .map(|i| ((i as f64 * 0.05).sin() * 1_000_000.0) as i32)
            .collect();
        let mut sizes = vec![];
        for level in 0..=8 {
            let mut e = FlacEncoder::new(48_000, 1024, level).unwrap();
            let f = e.encode_block(&block).unwrap();
            assert_eq!(&f[..2], &[0xFF, 0xF8], "sync code, fixed block size");
            sizes.push(f.len());
        }
        assert!(sizes.iter().all(|s| *s < 3000), "{sizes:?}");
    }
}
