//! The one decode front end shared by playout (`engine::decode`) and analysis
//! (`analysis::decode`): how a file is opened and which frames of it count as audio.
//!
//! Marker times in the library are milliseconds from the first audible sample, so the two decoders
//! must agree, to the frame, on where that sample is. Before this crate, analysis trimmed the
//! AAC-in-MP4 encoder priming (`iTunSMPB`) and playout did not, which put every AAC marker 43 ms
//! off on air. The rule is now written once:
//!
//! * **MP4/M4A with an `iTunSMPB` tag** (AAC from iTunes, ffmpeg, Apple Music): skip `priming`
//!   frames, keep `valid` frames ([`Trim`], [`itunsmpb`]). `symphonia` does not do this itself.
//! * **MP3 with a LAME or Xing header:** `symphonia` trims the encoder delay and padding when
//!   `FormatOptions::enable_gapless` is set, which [`open`] always does. Nothing more is done here.
//! * **Everything else** (FLAC, ALAC, WAV, AIFF, Vorbis, files without gapless information):
//!   nothing is trimmed.
//!
//! [`open`] returns the format reader, the decoder, the track and its parameters with the [`Trim`]
//! to apply to the decoded frames, in stream order (see [`Trim::window`]).

#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use symphonia::core::codecs::{CodecParameters, Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// One box header in an MP4 file.
struct BoxHead {
    kind: [u8; 4],
    /// Offset of the first payload byte.
    payload: u64,
    /// Offset just past the box.
    end: u64,
}

/// Read the header of the box at `pos`, which must lie inside `[pos, limit)`. Handles the 64-bit
/// size (`size == 1`) and the to-the-end size (`size == 0`).
fn box_head(f: &mut File, pos: u64, limit: u64) -> Option<BoxHead> {
    if pos + 8 > limit {
        return None;
    }
    f.seek(SeekFrom::Start(pos)).ok()?;
    let mut h = [0u8; 8];
    f.read_exact(&mut h).ok()?;
    let size32 = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let kind = [h[4], h[5], h[6], h[7]];
    let (size, hdr) = match size32 {
        0 => (limit - pos, 8),
        1 => {
            let mut l = [0u8; 8];
            f.read_exact(&mut l).ok()?;
            (u64::from_be_bytes(l), 16)
        }
        n => (u64::from(n), 8),
    };
    if size < hdr || pos.checked_add(size)? > limit {
        return None;
    }
    Some(BoxHead {
        kind,
        payload: pos + hdr,
        end: pos + size,
    })
}

/// The first child of type `kind` in the box payload `[from, to)`, found by walking the child
/// headers and seeking over their contents (so a large `mdat` or sample table costs one read).
fn find_child(f: &mut File, from: u64, to: u64, kind: &[u8; 4]) -> Option<BoxHead> {
    let mut pos = from;
    while pos + 8 <= to {
        let b = box_head(f, pos, to)?;
        if &b.kind == kind {
            return Some(b);
        }
        pos = b.end;
    }
    None
}

/// The text of the freeform `iTunSMPB` atom inside the `ilst` payload `[from, to)`: each `----`
/// child holds a `mean`, a `name` and a `data` box.
fn smpb_text(f: &mut File, from: u64, to: u64) -> Option<String> {
    let mut pos = from;
    while pos + 8 <= to {
        let item = box_head(f, pos, to)?;
        pos = item.end;
        if &item.kind != b"----" {
            continue;
        }
        let name = find_child(f, item.payload, item.end, b"name")?;
        // `name` and `data` are full boxes: 4 bytes of version and flags first.
        let n = name.end.checked_sub(name.payload + 4)?;
        if n > 64 {
            continue;
        }
        f.seek(SeekFrom::Start(name.payload + 4)).ok()?;
        let mut nb = vec![0u8; n as usize];
        f.read_exact(&mut nb).ok()?;
        if nb != b"iTunSMPB" {
            continue;
        }
        let data = find_child(f, item.payload, item.end, b"data")?;
        // version/flags (4) and locale (4), then the text.
        let n = data.end.checked_sub(data.payload + 8)?.min(4096);
        f.seek(SeekFrom::Start(data.payload + 8)).ok()?;
        let mut db = vec![0u8; n as usize];
        f.read_exact(&mut db).ok()?;
        return Some(String::from_utf8_lossy(&db).into_owned());
    }
    None
}

/// Encoder delay and padding from an iTunes `iTunSMPB` tag in an MP4/M4A file: `(priming, valid)`
/// in frames, i.e. skip `priming` decoded frames and keep `valid` after that. `None` when the tag
/// is absent or malformed.
///
/// The tag is found by walking the boxes, `moov/udta/meta/ilst` (and `moov/meta/ilst`, which some
/// writers use), so it does not matter where in the file the `moov` sits or how large its sample
/// tables are. (It used to be a text search of the first and last megabyte, which missed the tag of
/// a long file with a large `moov` in front.)
pub fn itunsmpb(path: &Path) -> Option<(u64, u64)> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[4..8] != b"ftyp" {
        return None;
    }
    let moov = find_child(&mut f, 0, len, b"moov")?;
    let meta_paths: [&[&[u8; 4]]; 2] = [&[b"udta", b"meta"], &[b"meta"]];
    for path in meta_paths {
        let (mut from, mut to) = (moov.payload, moov.end);
        let mut ok = true;
        for kind in path {
            match find_child(&mut f, from, to, kind) {
                Some(b) => {
                    from = b.payload;
                    to = b.end;
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // `meta` is a full box (version and flags) in ISO files; QuickTime's has none. The child
        // walk works either way: try after the 4 bytes first, then straight.
        let ilst = find_child(&mut f, from + 4, to, b"ilst")
            .or_else(|| find_child(&mut f, from, to, b"ilst"));
        let Some(ilst) = ilst else { continue };
        let Some(text) = smpb_text(&mut f, ilst.payload, ilst.end) else {
            continue;
        };
        let toks: Vec<u64> = text
            .split(|c: char| !c.is_ascii_hexdigit())
            .filter(|t| t.len() == 8 || t.len() == 16)
            .filter_map(|t| u64::from_str_radix(t, 16).ok())
            .collect();
        if toks.len() >= 4 && toks[3] > 0 {
            return Some((toks[1], toks[3]));
        }
    }
    None
}

/// The frames of the decoded stream that are audio: skip `skip` frames at the start, keep at most
/// `keep` after that. `Trim::none()` keeps everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trim {
    skip: u64,
    keep: u64,
    /// The `iTunSMPB` figures this trim came from, if any.
    itunes: Option<(u64, u64)>,
}

impl Trim {
    pub const fn none() -> Self {
        Self {
            skip: 0,
            keep: u64::MAX,
            itunes: None,
        }
    }

    pub const fn new(priming: u64, valid: u64) -> Self {
        Self {
            skip: priming,
            keep: valid,
            itunes: Some((priming, valid)),
        }
    }

    /// The trim a file calls for (`iTunSMPB` in an MP4; otherwise none: MP3 is trimmed by the
    /// demuxer).
    pub fn for_file(path: &Path) -> Self {
        itunsmpb(path).map_or(Self::none(), |(p, v)| Self::new(p, v))
    }

    /// True when this trim removes anything.
    pub fn is_active(&self) -> bool {
        self.itunes.is_some()
    }

    /// `(priming, valid)` when the trim comes from an `iTunSMPB` tag.
    pub fn itunes(&self) -> Option<(u64, u64)> {
        self.itunes
    }

    /// The trim for a stream that was opened at `start` audible frames into the file, with the
    /// decoder positioned exactly at that frame: nothing more to skip, `valid - start` to keep.
    pub fn after_seek(&self, start: u64) -> Self {
        Self {
            skip: 0,
            keep: self.keep.saturating_sub(start),
            itunes: self.itunes,
        }
    }

    /// The part of the next decoded chunk of `frames` frames that is audio, as a range of frame
    /// indices. Call once per chunk, in stream order.
    pub fn window(&mut self, frames: usize) -> std::ops::Range<usize> {
        let s = self.skip.min(frames as u64);
        self.skip -= s;
        let avail = frames as u64 - s;
        let k = self.keep.min(avail);
        self.keep -= k;
        s as usize..(s + k) as usize
    }

    /// The length the container declares (`n_frames`, which counts the priming) once trimmed:
    /// `min(declared - priming, valid)` for an `iTunSMPB` file, else as declared.
    pub fn declared(&self, n_frames: Option<u64>) -> Option<u64> {
        match (n_frames, self.itunes) {
            (Some(d), Some((p, v))) => Some(d.saturating_sub(p).min(v)),
            (d, _) => d,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot open {0}: {1}")]
    Open(String, String),
    #[error("not recognised as audio: {0}")]
    NotAudio(String),
    #[error("no decodable audio track")]
    NoTrack,
    #[error("no decoder for this codec: {0}")]
    NoDecoder(String),
}

/// An opened file: demuxer, decoder and the trim, ready for the packet loop.
pub struct Opened {
    pub format: Box<dyn FormatReader>,
    pub decoder: Box<dyn Decoder>,
    pub track_id: u32,
    pub params: CodecParameters,
    pub trim: Trim,
}

/// The format options every decoder in Hotclock uses. Gapless is on: `symphonia` then trims the
/// LAME/Xing delay and padding of an MP3.
pub fn format_options() -> FormatOptions {
    FormatOptions {
        enable_gapless: true,
        ..FormatOptions::default()
    }
}

/// Open `path` by content (never by extension) and pick the first decodable track.
pub fn open(path: &Path) -> Result<Opened, OpenError> {
    let file =
        File::open(path).map_err(|e| OpenError::Open(path.display().to_string(), e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &format_options(),
            &MetadataOptions::default(),
        )
        .map_err(|e| OpenError::NotAudio(e.to_string()))?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or(OpenError::NoTrack)?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| OpenError::NoDecoder(e.to_string()))?;
    Ok(Opened {
        format,
        decoder,
        track_id,
        params,
        trim: Trim::for_file(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_skips_then_keeps() {
        let mut t = Trim::new(2112, 5000);
        assert!(t.window(1024).is_empty());
        assert!(t.window(1024).is_empty());
        assert_eq!(t.window(1024), 64..1024);
        assert_eq!(t.window(2000), 0..2000);
        // 5000 valid: 960 + 2000 = 2960 so far, 2040 left.
        assert_eq!(t.window(4000), 0..2040);
        assert!(t.window(1024).is_empty());
    }

    /// One MP4 box.
    fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(payload);
        v
    }

    /// A file with `moov` first holding `junk` bytes of sample table before the `udta`, then
    /// `tail` bytes of `mdat`: the tag is at an arbitrary distance from both ends.
    fn mp4_with_tag(junk: usize, tail: usize, priming: u64, valid: u64) -> Vec<u8> {
        let text = format!(
            " 00000000 {priming:08X} 00000000 {valid:016X} 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000"
        );
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(text.as_bytes());
        let mut name = vec![0, 0, 0, 0];
        name.extend_from_slice(b"iTunSMPB");
        let mut mean = vec![0, 0, 0, 0];
        mean.extend_from_slice(b"com.apple.iTunes");
        let mut item = bx(b"mean", &mean);
        item.extend(bx(b"name", &name));
        item.extend(bx(b"data", &data));
        // Another freeform atom first, to be skipped.
        let mut other_name = vec![0, 0, 0, 0];
        other_name.extend_from_slice(b"iTunNORM");
        let other = bx(b"----", &bx(b"name", &other_name));
        let mut ilst_payload = other;
        ilst_payload.extend(bx(b"----", &item));
        let ilst = bx(b"ilst", &ilst_payload);
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend(ilst);
        let udta = bx(b"udta", &bx(b"meta", &meta_payload));
        let mut moov_payload = bx(b"trak", &vec![0u8; junk]);
        moov_payload.extend(udta);
        let mut f = bx(b"ftyp", b"M4A \0\0\0\0M4A mp42isom");
        f.extend(bx(b"moov", &moov_payload));
        f.extend(bx(b"mdat", &vec![0u8; tail]));
        f
    }

    #[test]
    fn itunsmpb_is_found_wherever_the_moov_puts_it() {
        let dir = std::env::temp_dir().join(format!("hotclock-smpb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Small, then a tag 3 MB from the start and 3 MB from the end (the old search saw only
        // the first and last megabyte).
        for (junk, tail) in [(100, 100), (3 << 20, 3 << 20), (20 << 20, 1 << 20)] {
            let p = dir.join(format!("t-{junk}-{tail}.m4a"));
            std::fs::write(&p, mp4_with_tag(junk, tail, 2112, 176_000)).unwrap();
            assert_eq!(
                itunsmpb(&p),
                Some((2112, 176_000)),
                "junk {junk} tail {tail}"
            );
        }
        // No tag, and not an MP4.
        let p = dir.join("none.m4a");
        let mut f = bx(b"ftyp", b"M4A \0\0\0\0M4A ");
        f.extend(bx(b"moov", &bx(b"udta", &[])));
        std::fs::write(&p, f).unwrap();
        assert_eq!(itunsmpb(&p), None);
        let p = dir.join("x.bin");
        std::fs::write(&p, vec![7u8; 100]).unwrap();
        assert_eq!(itunsmpb(&p), None);
        // A truncated box header never panics.
        let mut f = mp4_with_tag(100, 100, 1, 2);
        f.truncate(60);
        let p = dir.join("cut.m4a");
        std::fs::write(&p, f).unwrap();
        assert_eq!(itunsmpb(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_keeps_everything() {
        let mut t = Trim::none();
        assert_eq!(t.window(1152), 0..1152);
        assert!(!t.is_active());
        assert_eq!(t.declared(Some(10)), Some(10));
    }

    #[test]
    fn declared_length_is_trimmed() {
        let t = Trim::new(2112, 5000);
        assert_eq!(t.declared(Some(7500)), Some(5000));
        assert_eq!(t.declared(Some(4000)), Some(1888));
        assert_eq!(t.declared(None), None);
    }

    #[test]
    fn after_seek_keeps_the_remainder() {
        let mut t = Trim::new(2112, 5000).after_seek(1000);
        assert_eq!(t.window(10_000), 0..4000);
    }
}
