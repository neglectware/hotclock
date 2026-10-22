//! A minimal WAV writer and reader: PCM 16, 24 and 32 bit and IEEE float 32, any channel count.
//! Used by the `File` output backend, the offline renderer and the tests.

use common::config::SampleFormat;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavFormat {
    F32,
    Int(SampleFormat),
}

impl WavFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            WavFormat::F32 => 4,
            WavFormat::Int(f) => f.bytes_per_sample(),
        }
    }
    fn tag(self) -> u16 {
        match self {
            WavFormat::F32 => 3,
            WavFormat::Int(_) => 1,
        }
    }
}

pub struct WavWriter {
    w: BufWriter<File>,
    data_bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path, rate: u32, channels: u16, fmt: WavFormat) -> io::Result<Self> {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
        let bps = fmt.bytes_per_sample() as u16;
        w.write_all(b"RIFF")?;
        w.write_all(&0u32.to_le_bytes())?; // patched in finish
        w.write_all(b"WAVEfmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&fmt.tag().to_le_bytes())?;
        w.write_all(&channels.to_le_bytes())?;
        w.write_all(&rate.to_le_bytes())?;
        w.write_all(&(rate * u32::from(channels) * u32::from(bps)).to_le_bytes())?;
        w.write_all(&(channels * bps).to_le_bytes())?;
        w.write_all(&(bps * 8).to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&0u32.to_le_bytes())?; // patched in finish
        Ok(Self { w, data_bytes: 0 })
    }

    pub fn write_bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.data_bytes += b.len() as u64;
        self.w.write_all(b)
    }

    /// Patch the header sizes and flush. Returns the data length in bytes.
    pub fn finish(mut self) -> io::Result<u64> {
        let data = u32::try_from(self.data_bytes)
            .map_err(|_| io::Error::other("WAV data larger than 4 GiB"))?;
        self.w.flush()?;
        let f = self.w.get_mut();
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&(36 + data).to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&data.to_le_bytes())?;
        f.flush()?;
        Ok(self.data_bytes)
    }
}

/// Decoded WAV: interleaved `f32` samples in -1..1.
pub struct WavData {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl WavData {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }
}

pub fn read_wav(path: &Path) -> io::Result<WavData> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let (mut tag, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut pos = 12;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            tag = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = Some(body);
        }
        pos += 8 + len + (len & 1);
    }
    let data = data.ok_or_else(|| bad("no data chunk"))?;
    if channels == 0 {
        return Err(bad("no fmt chunk"));
    }
    let samples: Vec<f32> = match (tag, bits) {
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| {
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
                v as f32 / 8_388_608.0
            })
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as f32 / 2_147_483_648.0)
            .collect(),
        _ => {
            return Err(bad(&format!(
                "unsupported WAV format tag {tag}, {bits} bits"
            )))
        }
    };
    Ok(WavData {
        rate,
        channels,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip_all_formats() {
        let dir = fixtures::testdir::process_dir().join("wav");
        std::fs::create_dir_all(&dir).unwrap();
        for (fmt, tol) in [
            (WavFormat::F32, 0.0),
            (WavFormat::Int(SampleFormat::S32Le), 1e-8),
            (WavFormat::Int(SampleFormat::S24_3Le), 2e-7),
            (WavFormat::Int(SampleFormat::S16Le), 4e-5),
        ] {
            let p = dir.join("t.wav");
            let mut w = WavWriter::create(&p, 48_000, 2, fmt).unwrap();
            let input = [0.5f32, -0.25, 0.0, 0.999];
            match fmt {
                WavFormat::F32 => {
                    for s in input {
                        w.write_bytes(&s.to_le_bytes()).unwrap();
                    }
                }
                WavFormat::Int(f) => {
                    let mut c = crate::convert::Converter::new(f, 4);
                    let b = c.convert(&input).to_vec();
                    // 16-bit output is dithered; that is fine within the tolerance below
                    w.write_bytes(&b).unwrap();
                }
            }
            w.finish().unwrap();
            let r = read_wav(&p).unwrap();
            assert_eq!((r.rate, r.channels, r.frames()), (48_000, 2, 2));
            for (a, b) in input.iter().zip(&r.samples) {
                let tol = if fmt == WavFormat::Int(SampleFormat::S16Le) {
                    1.6 / 32768.0
                } else {
                    tol
                };
                assert!((a - b).abs() <= tol as f32 + 1e-9, "{fmt:?}: {a} vs {b}");
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
