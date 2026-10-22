//! Writers for the fixture containers: 24-bit WAV and AIFF, PNG and JPEG images. FLAC, MP3 and
//! M4A go through an external encoder (`ffmpeg`), see `lib.rs`.

use std::io::{Cursor, Write};
use std::path::Path;

fn q24(x: f64) -> i32 {
    (x * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32
}

/// Interleave one or two channels of `f64` as 24-bit integers.
fn interleave24(left: &[f64], right: Option<&[f64]>) -> Vec<i32> {
    match right {
        None => left.iter().map(|&v| q24(v)).collect(),
        Some(r) => left
            .iter()
            .zip(r)
            .flat_map(|(&l, &r)| [q24(l), q24(r)])
            .collect(),
    }
}

/// 24-bit PCM WAV.
pub fn write_wav24(path: &Path, mono: &[f64], rate: u32, stereo: bool) -> std::io::Result<()> {
    let channels: u16 = if stereo { 2 } else { 1 };
    let samples = interleave24(mono, stereo.then_some(mono));
    let data_len = samples.len() * 3;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len as u32).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * u32::from(channels) * 3).to_le_bytes())?;
    f.write_all(&(channels * 3).to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&(data_len as u32).to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes()[..3])?;
    }
    f.flush()
}

/// 80-bit IEEE extended, as AIFF wants the sample rate.
fn extended80(rate: u32) -> [u8; 10] {
    let mut e = [0u8; 10];
    let exp = 31 - rate.leading_zeros(); // floor(log2(rate))
    let mantissa = (u64::from(rate)) << (63 - exp);
    let biased = (16383 + exp) as u16;
    e[..2].copy_from_slice(&biased.to_be_bytes());
    e[2..].copy_from_slice(&mantissa.to_be_bytes());
    e
}

/// 24-bit AIFF.
pub fn write_aiff24(path: &Path, mono: &[f64], rate: u32, stereo: bool) -> std::io::Result<()> {
    let channels: u16 = if stereo { 2 } else { 1 };
    let samples = interleave24(mono, stereo.then_some(mono));
    let frames = mono.len() as u32;
    let data_len = samples.len() * 3;
    let ssnd_len = 8 + data_len;
    let form_len = 4 + (8 + 18) + (8 + ssnd_len);
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"FORM")?;
    f.write_all(&(form_len as u32).to_be_bytes())?;
    f.write_all(b"AIFFCOMM")?;
    f.write_all(&18u32.to_be_bytes())?;
    f.write_all(&channels.to_be_bytes())?;
    f.write_all(&frames.to_be_bytes())?;
    f.write_all(&24u16.to_be_bytes())?;
    f.write_all(&extended80(rate))?;
    f.write_all(b"SSND")?;
    f.write_all(&(ssnd_len as u32).to_be_bytes())?;
    f.write_all(&0u32.to_be_bytes())?; // offset
    f.write_all(&0u32.to_be_bytes())?; // block size
    for s in samples {
        f.write_all(&s.to_be_bytes()[1..])?;
    }
    if data_len % 2 == 1 {
        f.write_all(&[0])?;
    }
    f.flush()
}

fn cover_image(size: u32) -> image::RgbImage {
    image::RgbImage::from_fn(size, size, |x, y| {
        let r = (x * 255 / size) as u8;
        let g = (y * 255 / size) as u8;
        let b = (((x / 50) + (y / 50)) % 2 * 120 + 40) as u8;
        image::Rgb([r, g, b])
    })
}

pub fn png_cover(size: u32) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    cover_image(size)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("png");
    out.into_inner()
}

pub fn jpeg_cover(size: u32) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    cover_image(size)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .expect("jpeg");
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_80_bit_48000() {
        // 48000 = 0x400E BB80 0000 0000 0000
        assert_eq!(
            extended80(48_000),
            [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            extended80(44_100),
            [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]
        );
    }
}
