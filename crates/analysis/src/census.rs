//! `hotclock census`: for every file, the container, codec, rate, channels, whether `symphonia`
//! decodes it fully, whether tags are present (title, album, disc and track numbers), and whether
//! artwork exists (an embedded picture, or a `cover`/`folder` image beside the file).
//!
//! **Probing goes by content, never by extension.** Library music is sometimes named `.aac` but is
//! really an MP4/M4A container; the file name is only used to decide which files in a directory
//! are audio at all, and to report a mismatch.

use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::probe::Probe;
use serde::Serialize;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Extensions treated as audio when scanning a directory.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "m4b", "mp4", "aac", "alac", "wav", "wave", "aif", "aiff", "aifc", "ogg",
    "oga", "opus", "caf", "mka", "wv",
];

/// A decoded-frame shortfall smaller than this (one MP3 frame plus decoder delay, or AAC priming)
/// is not truncation.
const TRUNCATION_TOLERANCE_FRAMES: u64 = 2_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Decoded from start to end without an error.
    Ok,
    /// Decoded to the end, but some packets failed to decode (`decode_errors > 0`).
    DecodableWithErrors,
    /// The file ends before the length its header declares.
    Truncated,
    /// Could not be probed or decoded at all.
    Undecodable,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::DecodableWithErrors => "decodable-with-errors",
            Verdict::Truncated => "truncated",
            Verdict::Undecodable => "undecodable",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Tags {
    pub present: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Artwork {
    /// Number of embedded pictures.
    pub embedded: usize,
    /// A `cover.*` or `folder.*` image in the same directory.
    pub folder_image: Option<String>,
}

impl Artwork {
    pub fn any(&self) -> bool {
        self.embedded > 0 || self.folder_image.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub path: PathBuf,
    pub size_bytes: u64,
    /// From the file's content: `flac`, `mp3`, `mp4`, `wav`, `aiff`, `ogg`, `adts-aac`, `unknown`.
    pub container: String,
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<usize>,
    pub bits_per_sample: Option<u32>,
    pub declared_frames: Option<u64>,
    pub decoded_frames: u64,
    pub duration_s: Option<f64>,
    pub decode_errors: u32,
    pub verdict: Verdict,
    /// Why the verdict is not `ok`, in words.
    pub detail: Option<String>,
    /// Set when the extension names a different container than the content.
    pub extension_mismatch: Option<String>,
    pub tags: Tags,
    pub artwork: Artwork,
}

/// The container of a file, from its first bytes.
pub fn detect_container(head: &[u8]) -> &'static str {
    if head.len() >= 4 && &head[..4] == b"fLaC" {
        "flac"
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        "wav"
    } else if head.len() >= 12
        && &head[..4] == b"FORM"
        && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC")
    {
        "aiff"
    } else if head.len() >= 4 && &head[..4] == b"OggS" {
        "ogg"
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        "mp4"
    } else if head.len() >= 3 && &head[..3] == b"ID3" {
        "mp3"
    } else if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xF6 == 0xF0 {
        // ADTS: sync 0xFFF, layer bits 00.
        "adts-aac"
    } else if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 {
        "mp3"
    } else {
        "unknown"
    }
}

/// The container an extension implies, for the mismatch note.
fn extension_container(ext: &str) -> Option<&'static str> {
    match ext {
        "flac" => Some("flac"),
        "mp3" => Some("mp3"),
        "m4a" | "m4b" | "mp4" | "alac" => Some("mp4"),
        "aac" => Some("adts-aac"),
        "wav" | "wave" => Some("wav"),
        "aif" | "aiff" | "aifc" => Some("aiff"),
        "ogg" | "oga" | "opus" => Some("ogg"),
        _ => None,
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Files named by a directory (recursively, audio extensions only, sorted) or by an `.m3u` /
/// `.m3u8` list (every entry, relative paths resolved against the list's directory).
pub fn collect_files(target: &Path) -> Result<Vec<PathBuf>, String> {
    if target.is_dir() {
        let mut out = vec![];
        walk(target, &mut out).map_err(|e| format!("{}: {e}", target.display()))?;
        out.sort();
        Ok(out)
    } else if target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("m3u") || e.eq_ignore_ascii_case("m3u8"))
    {
        let text =
            std::fs::read_to_string(target).map_err(|e| format!("{}: {e}", target.display()))?;
        let base = target.parent().unwrap_or(Path::new("."));
        Ok(text
            .lines()
            .map(|l| l.trim().trim_start_matches('\u{feff}'))
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let p = PathBuf::from(l);
                if p.is_absolute() {
                    p
                } else {
                    base.join(p)
                }
            })
            .collect())
    } else if target.is_file() {
        Ok(vec![target.to_path_buf()])
    } else {
        Err(format!("{}: no such file or directory", target.display()))
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if is_hidden(&path) {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if has_audio_extension(&path) {
            out.push(path);
        }
    }
    Ok(())
}

struct DecodeOutcome {
    codec: Option<String>,
    sample_rate: Option<u32>,
    channels: Option<usize>,
    bits: Option<u32>,
    declared_frames: Option<u64>,
    decoded_frames: u64,
    decode_errors: u32,
    fatal: Option<String>,
}

fn decode_all(path: &Path) -> DecodeOutcome {
    let mut o = DecodeOutcome {
        codec: None,
        sample_rate: None,
        channels: None,
        bits: None,
        declared_frames: None,
        decoded_frames: 0,
        decode_errors: 0,
        fatal: None,
    };
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            o.fatal = Some(format!("cannot open: {e}"));
            return o;
        }
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    // No extension hint: content decides.
    let probed = match symphonia::default::get_probe().format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            o.fatal = Some(format!("not recognised as audio: {e}"));
            return o;
        }
    };
    let mut format = probed.format;
    let Some(track) = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    else {
        o.fatal = Some("no decodable audio track".into());
        return o;
    };
    let track_id = track.id;
    let params = track.codec_params.clone();
    o.codec = symphonia::default::get_codecs()
        .get_codec(params.codec)
        .map(|d| d.short_name.to_string());
    o.sample_rate = params.sample_rate;
    o.channels = params.channels.map(|c| c.count());
    o.bits = params.bits_per_sample.or(params.bits_per_coded_sample);
    o.declared_frames = params.n_frames;
    let mut decoder =
        match symphonia::default::get_codecs().make(&params, &DecoderOptions::default()) {
            Ok(d) => d,
            Err(e) => {
                o.fatal = Some(format!("no decoder: {e}"));
                return o;
            }
        };
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Symphonia signals the normal end of a stream as an unexpected EOF.
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::DecodeError(_)) => {
                o.decode_errors += 1;
                continue;
            }
            Err(e) => {
                o.fatal = Some(format!("read error: {e}"));
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                if o.decoded_frames == 0 {
                    // Some containers (ALAC in MP4) do not state rate or channels up front.
                    let spec = buf.spec();
                    o.sample_rate.get_or_insert(spec.rate);
                    o.channels.get_or_insert(spec.channels.count());
                }
                o.decoded_frames += buf.frames() as u64;
            }
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => o.decode_errors += 1,
            Err(e) => {
                o.fatal = Some(format!("decode error: {e}"));
                break;
            }
        }
    }
    o
}

fn read_tags(path: &Path) -> (Tags, usize) {
    let mut tags = Tags::default();
    let Ok(file) = File::open(path) else {
        return (tags, 0);
    };
    // A reader, not a path: lofty then cannot use the extension.
    let Ok(probe) = Probe::new(BufReader::new(file)).guess_file_type() else {
        return (tags, 0);
    };
    let Ok(tagged) = probe.read() else {
        return (tags, 0);
    };
    let mut pictures = 0;
    for tag in tagged.tags() {
        pictures += tag.pictures().len();
    }
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        tags.present = true;
        tags.title = tag.title().map(|s| s.to_string());
        tags.artist = tag.artist().map(|s| s.to_string());
        tags.album = tag.album().map(|s| s.to_string());
        tags.album_artist = tag.get_string(ItemKey::AlbumArtist).map(|s| s.to_string());
        tags.disc = tag.disk();
        tags.track = tag.track();
        tags.track_total = tag.track_total();
    }
    (tags, pictures)
}

fn folder_image(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    for entry in std::fs::read_dir(dir).ok()? {
        let p = entry.ok()?.path();
        let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if (stem == "cover" || stem == "folder") && matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            return p.file_name()?.to_str().map(|s| s.to_string());
        }
    }
    None
}

/// Inspect one file.
pub fn inspect(path: &Path) -> FileReport {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut head = [0u8; 16];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let container = detect_container(&head[..n]);
    let ext_mismatch = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .and_then(|e| extension_container(&e).map(|c| (e, c)))
        .and_then(|(e, c)| {
            (c != container && container != "unknown")
                .then(|| format!("extension `.{e}` suggests {c}, content is {container}"))
        });

    let d = decode_all(path);
    let (tags, embedded) = read_tags(path);
    let artwork = Artwork {
        embedded,
        folder_image: folder_image(path),
    };

    let (verdict, detail) = if let Some(f) = &d.fatal {
        if d.decoded_frames > 0 {
            (Verdict::Truncated, Some(format!("stopped early: {f}")))
        } else {
            (Verdict::Undecodable, Some(f.clone()))
        }
    } else if d
        .declared_frames
        .is_some_and(|decl| d.decoded_frames + TRUNCATION_TOLERANCE_FRAMES < decl)
    {
        let decl = d.declared_frames.unwrap_or(0);
        (
            Verdict::Truncated,
            Some(format!(
                "decoded {} of {} declared frames ({:.1} percent){}",
                d.decoded_frames,
                decl,
                d.decoded_frames as f64 * 100.0 / decl as f64,
                if d.decode_errors > 0 {
                    format!(", {} packet errors", d.decode_errors)
                } else {
                    String::new()
                }
            )),
        )
    } else if d.decode_errors > 0 {
        (
            Verdict::DecodableWithErrors,
            Some(format!("{} packets failed to decode", d.decode_errors)),
        )
    } else if d.decoded_frames == 0 {
        (Verdict::Undecodable, Some("no audio frames".into()))
    } else {
        (Verdict::Ok, None)
    };

    let duration_s = match (d.sample_rate, d.declared_frames) {
        (Some(r), Some(f)) if r > 0 => Some(f as f64 / f64::from(r)),
        (Some(r), None) if r > 0 && d.decoded_frames > 0 => {
            Some(d.decoded_frames as f64 / f64::from(r))
        }
        _ => None,
    };

    FileReport {
        path: path.to_path_buf(),
        size_bytes: size,
        container: container.to_string(),
        codec: d.codec,
        sample_rate: d.sample_rate,
        channels: d.channels,
        bits_per_sample: d.bits,
        declared_frames: d.declared_frames,
        decoded_frames: d.decoded_frames,
        duration_s,
        decode_errors: d.decode_errors,
        verdict,
        detail,
        extension_mismatch: ext_mismatch,
        tags,
        artwork,
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub files: usize,
    pub ok: usize,
    pub decodable_with_errors: usize,
    pub truncated: usize,
    pub undecodable: usize,
    pub with_artwork: usize,
    pub with_title: usize,
    pub with_album_and_track: usize,
    pub extension_mismatches: usize,
}

pub fn summarize(reports: &[FileReport]) -> Summary {
    let mut s = Summary {
        files: reports.len(),
        ..Summary::default()
    };
    for r in reports {
        match r.verdict {
            Verdict::Ok => s.ok += 1,
            Verdict::DecodableWithErrors => s.decodable_with_errors += 1,
            Verdict::Truncated => s.truncated += 1,
            Verdict::Undecodable => s.undecodable += 1,
        }
        s.with_artwork += usize::from(r.artwork.any());
        s.with_title += usize::from(r.tags.title.is_some());
        s.with_album_and_track += usize::from(r.tags.album.is_some() && r.tags.track.is_some());
        s.extension_mismatches += usize::from(r.extension_mismatch.is_some());
    }
    s
}

/// One line per file, then the summary. Paths are shown relative to `base` when possible.
pub fn format_text(reports: &[FileReport], base: &Path) -> String {
    let mut out = String::new();
    for r in reports {
        let rel = r.path.strip_prefix(base).unwrap_or(&r.path);
        let art = match (r.artwork.embedded, &r.artwork.folder_image) {
            (0, None) => "none".to_string(),
            (n, None) => format!("embedded x{n}"),
            (0, Some(f)) => format!("folder:{f}"),
            (n, Some(f)) => format!("embedded x{n} + folder:{f}"),
        };
        let tags = if r.tags.present {
            format!(
                "title={:?} album={:?} disc={} track={}/{}",
                r.tags.title.as_deref().unwrap_or(""),
                r.tags.album.as_deref().unwrap_or(""),
                r.tags.disc.map_or("-".into(), |v| v.to_string()),
                r.tags.track.map_or("-".into(), |v| v.to_string()),
                r.tags.track_total.map_or("-".into(), |v| v.to_string()),
            )
        } else {
            "no tags".to_string()
        };
        out.push_str(&format!(
            "{}\n    {} / {}  {} Hz  {} ch  {} bit  {}  decoded {} frames{}  => {}{}\n    tags: {}  artwork: {}\n",
            rel.display(),
            r.container,
            r.codec.as_deref().unwrap_or("?"),
            r.sample_rate.map_or("?".into(), |v| v.to_string()),
            r.channels.map_or("?".into(), |v| v.to_string()),
            r.bits_per_sample.map_or("?".into(), |v| v.to_string()),
            r.duration_s.map_or("?".into(), |d| format!("{d:.1} s")),
            r.decoded_frames,
            r.declared_frames
                .map_or(String::new(), |d| format!(" of {d} declared")),
            r.verdict.label(),
            r.detail.as_ref().map_or(String::new(), |d| format!(" ({d})")),
            tags,
            art,
        ));
        if let Some(m) = &r.extension_mismatch {
            out.push_str(&format!("    note: {m}\n"));
        }
    }
    let s = summarize(reports);
    out.push_str(&format!(
        "\n{} files: {} ok, {} decodable-with-errors, {} truncated, {} undecodable; \
         {} with artwork, {} with a title, {} with album and track number; {} extension mismatches\n",
        s.files,
        s.ok,
        s.decodable_with_errors,
        s.truncated,
        s.undecodable,
        s.with_artwork,
        s.with_title,
        s.with_album_and_track,
        s.extension_mismatches
    ));
    out
}
