//! Artwork (architecture 5.4 step 8): pick a source, normalize to a 600 px and a 144 px JPEG,
//! deduplicated by the hash of the source bytes. Never fails the import.

use crate::store::{art_rel, publish, tmp_name};
use crate::util::sha256_bytes;
use image::{imageops::FilterType, DynamicImage, ImageReader};
use std::io::Cursor;
use std::path::Path;

pub const MAX_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_SIDE: u32 = 10_000;
const NAMES: [&str; 5] = ["cover", "folder", "front", "album", "art"];

pub struct ArtSource {
    pub bytes: Vec<u8>,
    /// `embedded` or `folder`.
    pub origin: &'static str,
}

/// An image beside the file named `cover`, `folder`, `front`, `album` or `art` (`.jpg`, `.jpeg`,
/// `.png`, any case), first match in that name order, then file name order.
pub fn folder_image(audio: &Path) -> Option<std::path::PathBuf> {
    let dir = audio.parent()?;
    let mut found: Vec<(usize, std::path::PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let ext = p.extension()?.to_str()?.to_ascii_lowercase();
            let rank = NAMES.iter().position(|n| *n == stem)?;
            matches!(ext.as_str(), "jpg" | "jpeg" | "png").then_some((rank, p))
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, p)| p)
}

pub struct Normalized {
    pub sha: String,
    pub origin: &'static str,
    pub src_w: u32,
    pub src_h: u32,
    pub bytes_600: u64,
    pub bytes_144: u64,
    /// The image files were newly written (false when the same source was stored before).
    pub written: bool,
}

fn fit(img: &DynamicImage, side: u32) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= side {
        return img.clone();
    }
    // Longest side becomes exactly `side`; the other keeps the aspect (rounded, at least 1).
    let (nw, nh) = if w >= h {
        (
            side,
            ((f64::from(h) * f64::from(side) / f64::from(w)).round() as u32).max(1),
        )
    } else {
        (
            ((f64::from(w) * f64::from(side) / f64::from(h)).round() as u32).max(1),
            side,
        )
    };
    img.resize_exact(nw, nh, FilterType::Lanczos3)
}

fn encode_jpeg(img: &DynamicImage) -> Result<Vec<u8>, String> {
    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    rgb.write_with_encoder(enc).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Decode, normalize and store. `Err` carries the reason (the caller logs it as a warning and
/// imports the audio without art).
pub fn store_art(media: &Path, tmp_dir: &Path, src: &ArtSource) -> Result<Normalized, String> {
    if src.bytes.is_empty() || src.bytes.len() > MAX_BYTES {
        return Err(format!(
            "cover image is {} bytes (limit {MAX_BYTES})",
            src.bytes.len()
        ));
    }
    let sha = sha256_bytes(&src.bytes);
    let reader = ImageReader::new(Cursor::new(&src.bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("cannot read cover image: {e}"))?;
    if w > MAX_SIDE || h > MAX_SIDE || w == 0 || h == 0 {
        return Err(format!("cover image is {w} x {h} px (limit {MAX_SIDE})"));
    }
    let img = ImageReader::new(Cursor::new(&src.bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| format!("cannot decode cover image: {e}"))?;
    let p600 = media.join(art_rel(&sha, 600));
    let p144 = media.join(art_rel(&sha, 144));
    if p600.exists() && p144.exists() {
        return Ok(Normalized {
            sha,
            origin: src.origin,
            src_w: w,
            src_h: h,
            bytes_600: std::fs::metadata(&p600).map_or(0, |m| m.len()),
            bytes_144: std::fs::metadata(&p144).map_or(0, |m| m.len()),
            written: false,
        });
    }
    let b600 = encode_jpeg(&fit(&img, 600))?;
    let b144 = encode_jpeg(&fit(&img, 144))?;
    for (bytes, dest) in [(&b600, &p600), (&b144, &p144)] {
        let tmp = tmp_name(tmp_dir, "art");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        publish(&tmp, dest).map_err(|e| e.to_string())?;
    }
    Ok(Normalized {
        sha,
        origin: src.origin,
        src_w: w,
        src_h: h,
        bytes_600: b600.len() as u64,
        bytes_144: b144.len() as u64,
        written: true,
    })
}
