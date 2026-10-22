//! The synthetic fixture generator (milestones, "Shared test fixtures"). Nothing here is real
//! audio: every file is generated from formulas and a seeded generator, so the set is reproducible
//! and can be produced anywhere. Generated audio is never committed (`tests/fixtures/generated/`
//! is ignored); the hand-written expectations are in `tests/fixtures/expected.toml`.
//!
//! FLAC, MP3, ALAC and AAC files are encoded by `ffmpeg`, which must be on the `PATH` (or named by
//! `HOTCLOCK_FFMPEG`); the fixtures that need it are reported as skipped when it is not. (A
//! pure-Rust FLAC encoder, `flacenc` 0.5.1, was tried first and dropped: `symphonia` rejects its
//! files, and it writes absurdly large files for loud noise at 20 or 24 bits.)

// `testdir` needs two libc calls (`atexit`, `kill`); everything else stays safe.
#![deny(unsafe_code)]

pub mod files;
pub mod signals;
pub mod testdir;

use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, ItemKey, Tag, TagExt, TagType};
use signals::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const RATE: u32 = 48_000;

/// Every fixture name `generate` accepts, in generation order. A name is a file or, for the
/// ones with companions, the group of files it produces (listed in `produces`).
pub const ALL: &[&str] = &[
    "tone_fade.flac",
    "cold_start.flac",
    "pickup.flac",
    "cold_end.flac",
    "long_tail.flac",
    "ambient.flac",
    "voice_short.flac",
    "liner_8s.flac",
    "quiet_track.flac",
    "mono.wav",
    "corrupt.mp3",
    "rate_44k.flac",
    "rate_96k.flac",
    "truncated.flac",
    "quote_unquote.flac",
    "slowly_growing_deaf.flac",
    "slowly_growing_deaf_t1.flac",
    "the_summoning.flac",
    "album_t2.flac",
    "art_embedded.flac",
    "art_folder/track.mp3",
    "alac.m4a",
    "aiff.aif",
    "inbox/liner/Donate Liner 02.wav",
    "mp4_named_aac.aac",
    "flac16.flac",
];

/// M1.4 fixtures, outside `ALL` (the marker expectations do not cover them): a known noise
/// signal ([`gapless_source`]) encoded as AAC in MP4 (no `iTunSMPB` yet; see [`set_itunsmpb`]) and
/// as MP3 with a LAME header.
pub const GAPLESS: &[&str] = &["gapless_aac.m4a", "gapless_mp3.mp3"];

/// The signal behind the `GAPLESS` fixtures: 4 s of pink noise at -14 dBFS RMS. Noise correlates
/// with itself at one lag only, so the offset of a decoded copy can be measured to the sample.
pub fn gapless_source() -> Vec<f64> {
    pink(RATE, -14.0, 4.0, 300)
}

/// Write an iTunes `iTunSMPB` freeform atom into an MP4/M4A file (replacing its `ilst`): the
/// encoder priming to skip and the number of valid frames to keep, which is what iTunes and
/// ffmpeg's `-f ipod` players trim.
pub fn set_itunsmpb(path: &Path, priming: u32, valid: u64) -> Result<(), String> {
    use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst};
    let text = format!(
        " 00000000 {priming:08X} 00000000 {valid:016X} 00000000 00000000 00000000 00000000 00000000 00000000 00000000 00000000"
    );
    let mut ilst = Ilst::new();
    ilst.insert(Atom::new(
        AtomIdent::Freeform {
            mean: "com.apple.iTunes".into(),
            name: "iTunSMPB".into(),
        },
        AtomData::UTF8(text),
    ));
    ilst.save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("{}: writing iTunSMPB failed: {e}", path.display()))
}

#[derive(Debug, Default)]
pub struct Report {
    pub written: Vec<PathBuf>,
    /// `(name, reason)` for fixtures that could not be made (no `ffmpeg`).
    pub skipped: Vec<(String, String)>,
}

pub struct Generator {
    dir: PathBuf,
    ffmpeg: Option<PathBuf>,
}

fn find_ffmpeg() -> Option<PathBuf> {
    let candidate = std::env::var("HOTCLOCK_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    Command::new(&candidate)
        .arg("-version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| PathBuf::from(candidate))
}

// ---- the signal definitions of the milestones table ----

/// 0.5 s silence, 1 kHz at `level` dBFS RMS for 20 s, 5 s linear fade to silence, 1 s silence.
pub fn tone_fade(level: f64) -> Vec<f64> {
    let mut fade = sine(RATE, 1000.0, level, 5.0);
    ramp(&mut fade, 1.0, 0.0);
    concat(&[
        silence(RATE, 0.5),
        sine(RATE, 1000.0, level, 20.0),
        fade,
        silence(RATE, 1.0),
    ])
}

/// 1 kHz at -14 dBFS RMS from sample 0 for 10 s, then a 3 s linear fade.
pub fn cold_start(hz: f64) -> Vec<f64> {
    // One continuous sine (no phase jump at the join), with the last 3 s shaped into the fade.
    let mut all = sine(RATE, hz, -14.0, 13.0);
    let n = all.len();
    let fade_len = 3 * RATE as usize;
    ramp(&mut all[n - fade_len..], 1.0, 0.0);
    all
}

pub fn pickup() -> Vec<f64> {
    concat(&[
        silence(RATE, 0.3),
        sine(RATE, 500.0, -32.0, 1.5),
        sine(RATE, 500.0, -14.0, 15.0),
    ])
}

pub fn cold_end() -> Vec<f64> {
    concat(&[
        silence(RATE, 0.2),
        pink(RATE, -16.0, 12.0, 4),
        silence(RATE, 0.8),
    ])
}

pub fn long_tail() -> Vec<f64> {
    let mut tail = pink(RATE, -16.0, 30.0, 5);
    ramp_db(&mut tail, 0.0, -40.0);
    concat(&[pink(RATE, -16.0, 20.0, 6), tail])
}

pub fn ambient() -> Vec<f64> {
    let mut up = pink(RATE, -30.0, 20.0, 7);
    // Shape a constant-level noise into a swell from -50 to -30 dBFS.
    ramp_db(&mut up, -20.0, 0.0);
    let hold = pink(RATE, -30.0, 20.0, 8);
    let mut down = pink(RATE, -30.0, 20.0, 9);
    ramp_db(&mut down, 0.0, -20.0);
    concat(&[up, hold, down])
}

/// 2 s: silence 0.2 s, then three 400 ms bursts of 300 Hz to 3 kHz band noise at -18 dBFS with
/// 200 ms gaps, then 0.2 s of silence.
pub fn voice_short() -> Vec<f64> {
    let mut parts = vec![silence(RATE, 0.2)];
    for i in 0..3 {
        parts.push(band_noise(RATE, 300.0, 3000.0, -18.0, 0.4, 20 + i));
        parts.push(silence(RATE, 0.2));
    }
    concat(&parts)
}

/// 8 s: 0.2 s silence, band-noise bursts of 0.6 s with 0.2 s gaps (the last one 0.8 s long, ending
/// at 7.4 s), then a 0.6 s exponential decay of the same noise (-60 dB) to 8.0 s.
pub fn liner_8s() -> Vec<f64> {
    let mut parts = vec![silence(RATE, 0.2)];
    for k in 0..8 {
        parts.push(band_noise(RATE, 300.0, 3000.0, -18.0, 0.6, 40 + k));
        parts.push(silence(RATE, 0.2));
    }
    parts.push(band_noise(RATE, 300.0, 3000.0, -18.0, 0.8, 50));
    let mut decay = band_noise(RATE, 300.0, 3000.0, -18.0, 0.6, 51);
    ramp_db(&mut decay, 0.0, -60.0);
    parts.push(decay);
    let v = concat(&parts);
    debug_assert_eq!(v.len(), 8 * RATE as usize);
    v
}

/// Slowly modulated pink noise: `rms_dbfs` on average, +-3 dB at 0.3 Hz.
fn snoring(seconds: f64, rms_dbfs: f64, seed: u64) -> Vec<f64> {
    let mut x = pink(RATE, rms_dbfs, seconds, seed);
    for (i, v) in x.iter_mut().enumerate() {
        let t = i as f64 / RATE as f64;
        *v *= db_to_amp(3.0 * (std::f64::consts::TAU * 0.3 * t).sin());
    }
    x
}

/// Mr. Bungle analog (architecture 15): 32 s at -50 dBFS, a 0.4 s glass burst at 32.0 s at
/// (P_ref - 4) dB with P_ref the band level -12 dBFS, the band from 34.0 s for 60 s, a 5 s decay.
pub fn quote_unquote() -> Vec<f64> {
    let mut quiet = snoring(34.0, -50.0, 60);
    let glass = band_noise(RATE, 4000.0, 12_000.0, -16.0, 0.4, 61);
    add_at(&mut quiet, (32.0 * RATE as f64) as usize, &glass);
    let band = pink(RATE, -12.0, 60.0, 62);
    let mut decay = pink(RATE, -12.0, 5.0, 63);
    ramp_db(&mut decay, 0.0, -60.0);
    concat(&[quiet, band, decay])
}

/// Mr. Bungle analog: loud noise to 351.0 s, a choked hit (60 ms of 80 Hz at -6 dBFS peak plus
/// 150 ms of 4 to 12 kHz noise, silent from 351.3 s), then a 65 s speech-band interlude at -30.
pub fn slowly_growing_deaf() -> Vec<f64> {
    let loud = pink(RATE, -12.0, 351.0, 70);
    let mut hit = silence(RATE, 0.3);
    let kick: Vec<f64> = (0..(0.06 * RATE as f64) as usize)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            db_to_amp(-6.0) * (std::f64::consts::TAU * 80.0 * t).sin() * (1.0 - t / 0.06)
        })
        .collect();
    add_at(&mut hit, 0, &kick);
    let cymbal = band_noise(RATE, 4000.0, 12_000.0, -14.0, 0.15, 71);
    add_at(&mut hit, 0, &cymbal);
    // Choke: the last 5 ms fade to exact silence.
    let n = hit.len();
    let choke = (0.005 * RATE as f64) as usize;
    for i in 0..choke {
        hit[n - choke + i] *= 1.0 - (i as f64 + 1.0) / choke as f64;
    }
    // Interlude: 65 s of speech-band bursts (0.35 to 0.9 s) with 0.25 to 0.6 s gaps.
    let mut rng = Rng::new(72);
    let mut inter = Vec::new();
    let total = 65 * RATE as usize;
    while inter.len() < total {
        let burst = 0.35 + 0.55 * (rng.uniform() * 0.5 + 0.5);
        let gap = 0.25 + 0.35 * (rng.uniform() * 0.5 + 0.5);
        inter.extend(band_noise(
            RATE,
            300.0,
            3000.0,
            -30.0,
            burst,
            rng.next_u64(),
        ));
        inter.extend(silence(RATE, gap));
    }
    inter.truncate(total);
    concat(&[loud, hit, inter])
}

/// Sleep Token analog: 8 s of pad at -45 dBFS with three metallic ticks at -38 dBFS (peak),
/// then noise at -12 dBFS from 8.0 s for 30 s.
pub fn the_summoning() -> Vec<f64> {
    let mut pad = pink(RATE, -45.0, 8.0, 80);
    for t in [2.5, 4.5, 6.5] {
        add_at(
            &mut pad,
            (t * RATE as f64) as usize,
            &tick(RATE, -38.0, 0.02),
        );
    }
    concat(&[pad, pink(RATE, -12.0, 30.0, 81)])
}

impl Generator {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            ffmpeg: find_ffmpeg(),
        }
    }

    pub fn has_ffmpeg(&self) -> bool {
        self.ffmpeg.is_some()
    }

    fn path(&self, name: &str) -> std::io::Result<PathBuf> {
        let p = self.dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(p)
    }

    /// Run ffmpeg on a temporary 24-bit WAV of `mono`, writing `out`.
    fn ffmpeg_encode(
        &self,
        mono: &[f64],
        rate: u32,
        cover: Option<&Path>,
        codec_args: &[&str],
        out: &Path,
    ) -> Result<(), String> {
        let ff = self.ffmpeg.as_ref().ok_or("ffmpeg not found")?;
        let tmp = self.dir.join(format!(".tmp-{}.wav", std::process::id()));
        files::write_wav24(&tmp, mono, rate, true).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(ff);
        cmd.args(["-y", "-loglevel", "error", "-i"]).arg(&tmp);
        if let Some(c) = cover {
            cmd.arg("-i").arg(c);
            cmd.args([
                "-map",
                "0:a",
                "-map",
                "1:v",
                "-c:v",
                "mjpeg",
                "-disposition:v:0",
                "attached_pic",
            ]);
        }
        cmd.args(codec_args).arg(out);
        let res = cmd.output().map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&tmp);
        let res = res?;
        if res.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&res.stderr).trim().to_string())
        }
    }

    /// A FLAC file of `bits` (16 or 24) bit depth, stereo (both channels equal).
    fn flac(&self, out: &Path, mono: &[f64], rate: u32, bits: u32) -> Result<(), String> {
        let fmt = if bits == 16 { "s16" } else { "s32" };
        self.ffmpeg_encode(mono, rate, None, &["-c:a", "flac", "-sample_fmt", fmt], out)
    }

    /// Generate the named fixtures (all of `ALL` when `names` is empty).
    pub fn generate(&self, names: &[&str]) -> Report {
        let mut report = Report::default();
        let all: Vec<&str> = if names.is_empty() {
            ALL.to_vec()
        } else {
            names.to_vec()
        };
        for name in all {
            match self.build(name) {
                Ok(Some(paths)) => report.written.extend(paths),
                Ok(None) => report
                    .skipped
                    .push((name.to_string(), "ffmpeg not found".to_string())),
                Err(e) => report.skipped.push((name.to_string(), e)),
            }
        }
        report
    }

    /// `Ok(None)` when the fixture needs ffmpeg and there is none.
    fn build(&self, name: &str) -> Result<Option<Vec<PathBuf>>, String> {
        let io = |e: std::io::Error| format!("{name}: {e}");
        let p = self.path(name).map_err(io)?;
        if name.ends_with(".flac") && self.ffmpeg.is_none() {
            return Ok(None);
        }
        let flac = |x: &[f64], rate: u32| self.flac(&p, x, rate, 24);
        match name {
            "tone_fade.flac" => flac(&tone_fade(-14.0), RATE)?,
            "cold_start.flac" => flac(&cold_start(1000.0), RATE)?,
            "pickup.flac" => flac(&pickup(), RATE)?,
            "cold_end.flac" => flac(&cold_end(), RATE)?,
            "long_tail.flac" => flac(&long_tail(), RATE)?,
            "ambient.flac" => flac(&ambient(), RATE)?,
            "voice_short.flac" => flac(&voice_short(), RATE)?,
            "liner_8s.flac" => flac(&liner_8s(), RATE)?,
            "quiet_track.flac" => flac(&tone_fade(-30.0), RATE)?,
            "mono.wav" => files::write_wav24(&p, &sine(44_100, 1000.0, -20.0, 10.0), 44_100, false)
                .map_err(io)?,
            "rate_44k.flac" => flac(&sine(44_100, 1000.0, -14.0, 5.0), 44_100)?,
            "rate_96k.flac" => flac(&sine(96_000, 1000.0, -14.0, 5.0), 96_000)?,
            "flac16.flac" => self.flac(&p, &sine(RATE, 1000.0, -14.0, 8.0), RATE, 16)?,
            "truncated.flac" => {
                self.flac(&p, &tone_fade(-14.0), RATE, 24)?;
                let b = std::fs::read(&p).map_err(io)?;
                std::fs::write(&p, &b[..b.len() * 60 / 100]).map_err(io)?;
            }
            "quote_unquote.flac" => flac(&quote_unquote(), RATE)?,
            "slowly_growing_deaf.flac" => flac(&slowly_growing_deaf(), RATE)?,
            "slowly_growing_deaf_t1.flac" => {
                // A second copy of the same audio, tagged as track 1 of 2.
                let src = self.path("slowly_growing_deaf.flac").map_err(io)?;
                if !src.exists() {
                    self.build("slowly_growing_deaf.flac")?;
                }
                std::fs::copy(&src, &p).map_err(io)?;
                tag_file(
                    &p,
                    &TagSpec {
                        title: "Slowly Growing Deaf (fixture)",
                        album: "Fixture Album",
                        album_artist: "Fixture Artist",
                        disc: 1,
                        track: 1,
                        track_total: 2,
                        cover: None,
                    },
                )?;
            }
            "the_summoning.flac" => flac(&the_summoning(), RATE)?,
            "gapless_aac.m4a" | "gapless_mp3.mp3" => {
                if self.ffmpeg.is_none() {
                    return Ok(None);
                }
                let args: &[&str] = if name.ends_with("m4a") {
                    &["-c:a", "aac", "-b:a", "192k", "-f", "ipod"]
                } else {
                    &["-c:a", "libmp3lame", "-b:a", "192k"]
                };
                self.ffmpeg_encode(&gapless_source(), RATE, None, args, &p)?;
            }
            "album_t2.flac" => {
                flac(&cold_start(800.0), RATE)?;
                tag_file(
                    &p,
                    &TagSpec {
                        title: "Album Track Two (fixture)",
                        album: "Fixture Album",
                        album_artist: "Fixture Artist",
                        disc: 1,
                        track: 2,
                        track_total: 2,
                        cover: Some((files::png_cover(600), MimeType::Png)),
                    },
                )?;
            }
            "art_embedded.flac" => {
                flac(&sine(RATE, 440.0, -20.0, 5.0), RATE)?;
                tag_file(
                    &p,
                    &TagSpec {
                        title: "Embedded Art (fixture)",
                        album: "Art Album",
                        album_artist: "Art Artist",
                        disc: 1,
                        track: 1,
                        track_total: 1,
                        cover: Some((files::png_cover(1400), MimeType::Png)),
                    },
                )?;
            }
            "aiff.aif" => {
                files::write_aiff24(&p, &sine(RATE, 660.0, -20.0, 5.0), RATE, true).map_err(io)?
            }
            "inbox/liner/Donate Liner 02.wav" => {
                let mut parts = vec![silence(RATE, 0.1)];
                for k in 0..3 {
                    parts.push(band_noise(RATE, 300.0, 3000.0, -18.0, 1.4, 90 + k));
                    parts.push(silence(RATE, 0.3));
                }
                files::write_wav24(&p, &concat(&parts), RATE, true).map_err(io)?;
            }
            // Encoded with ffmpeg.
            "corrupt.mp3" | "art_folder/track.mp3" | "alac.m4a" | "mp4_named_aac.aac" => {
                if self.ffmpeg.is_none() {
                    return Ok(None);
                }
                let tone = sine(RATE, 1000.0, -14.0, 8.0);
                match name {
                    "corrupt.mp3" => {
                        let long = tone_fade(-14.0);
                        self.ffmpeg_encode(
                            &long,
                            RATE,
                            None,
                            &["-c:a", "libmp3lame", "-b:a", "128k"],
                            &p,
                        )?;
                        // 64 bytes overwritten at 50 percent, with 0xFF. Where and with what matters:
                        // garbage in the main data decodes as noise without any error, and random
                        // side information only sometimes fails validation. All-ones side information
                        // (main_data_begin 511, invalid block types) is rejected every time, so the
                        // damage starts 4 bytes after the first frame header at or after the middle. (128 kbit/s, 48 kHz: frames of 384 or 385 bytes.)
                        let mut b = std::fs::read(&p).map_err(io)?;
                        let start = mp3_frame_at_or_after(&b, b.len() / 2)
                            .ok_or("corrupt.mp3: no MP3 frame found")?
                            + 4;
                        for byte in &mut b[start..start + 64] {
                            *byte = 0xFF;
                        }
                        std::fs::write(&p, b).map_err(io)?;
                    }
                    "art_folder/track.mp3" => {
                        self.ffmpeg_encode(
                            &tone,
                            RATE,
                            None,
                            &["-c:a", "libmp3lame", "-b:a", "128k"],
                            &p,
                        )?;
                        let folder = self.path("art_folder/folder.jpg").map_err(io)?;
                        std::fs::write(&folder, files::jpeg_cover(800)).map_err(io)?;
                    }
                    "alac.m4a" => {
                        self.ffmpeg_encode(&tone, RATE, None, &["-c:a", "alac", "-f", "ipod"], &p)?;
                    }
                    _ => {
                        // AAC-LC in an MP4 container with an attached MJPEG cover, but named
                        // `.aac`: probing must go by content, not by extension.
                        let cover = self.dir.join(".tmp-cover.jpg");
                        std::fs::write(&cover, files::jpeg_cover(600)).map_err(io)?;
                        let r = self.ffmpeg_encode(
                            &tone,
                            RATE,
                            Some(&cover),
                            &[
                                "-c:a",
                                "aac",
                                "-b:a",
                                "128k",
                                "-metadata",
                                "title=Mislabelled AAC (fixture)",
                                "-metadata",
                                "album=Library Album",
                                "-f",
                                "ipod",
                            ],
                            &p,
                        );
                        let _ = std::fs::remove_file(&cover);
                        r?;
                    }
                }
            }
            other => return Err(format!("unknown fixture `{other}`")),
        }
        let mut written = vec![p];
        if name == "art_folder/track.mp3" {
            written.push(self.dir.join("art_folder/folder.jpg"));
        }
        Ok(Some(written))
    }
}

/// The offset of the first MP3 frame header at or after `from`, for the fixed encoding the
/// generator uses (MPEG-1 Layer III, 128 kbit/s, 48 kHz: frames of 384 or 385 bytes). The header
/// fields are checked, and so is the sync word of the following frame.
fn mp3_frame_at_or_after(b: &[u8], from: usize) -> Option<usize> {
    let is_header = |i: usize| {
        b.get(i) == Some(&0xFF)
            && b.get(i + 1).is_some_and(|&x| x & 0xFE == 0xFA) // MPEG-1, Layer III
            && b.get(i + 2).is_some_and(|&x| x >> 4 == 9 && (x >> 2) & 3 == 1) // 128k, 48 kHz
    };
    (from..b.len().saturating_sub(4)).find(|&i| {
        is_header(i) && {
            let len = 384 + usize::from((b[i + 2] >> 1) & 1);
            is_header(i + len)
        }
    })
}

pub struct TagSpec<'a> {
    pub title: &'a str,
    pub album: &'a str,
    pub album_artist: &'a str,
    pub disc: u32,
    pub track: u32,
    pub track_total: u32,
    pub cover: Option<(Vec<u8>, MimeType)>,
}

pub fn tag_file(path: &Path, spec: &TagSpec) -> Result<(), String> {
    let mut tag = Tag::new(TagType::VorbisComments);
    tag.set_title(spec.title.to_string());
    tag.set_album(spec.album.to_string());
    tag.insert_text(ItemKey::AlbumArtist, spec.album_artist.to_string());
    tag.set_disk(spec.disc);
    tag.set_track(spec.track);
    tag.set_track_total(spec.track_total);
    if let Some((data, mime)) = &spec.cover {
        tag.push_picture(
            Picture::unchecked(data.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(mime.clone())
                .build(),
        );
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("{}: tagging failed: {e}", path.display()))
}

/// The default output directory of the `fixtures` binary: `tests/fixtures/generated` at the
/// workspace root.
pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/generated")
}

/// Generate `names` into a directory shared by every test process that asks for the same fixtures,
/// under the target directory, so the work is done once per `cargo test`. Returns the directory.
pub fn shared_dir(names: &[&str]) -> (PathBuf, Report) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures-cache");
    std::fs::create_dir_all(&dir).expect("create fixture cache");
    let lock = dir.join(".lock");
    // A crude cross-process lock: create_new until it succeeds.
    let start = std::time::Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(_) => break,
            Err(_) => {
                if start.elapsed() > std::time::Duration::from_secs(600) {
                    let _ = std::fs::remove_file(&lock);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let g = Generator::new(&dir);
    let wanted: Vec<&str> = names.to_vec();
    let missing: Vec<&str> = wanted
        .iter()
        .copied()
        .filter(|n| !dir.join(n).exists())
        .collect();
    let report = if missing.is_empty() {
        Report::default()
    } else {
        g.generate(&missing)
    };
    let _ = std::fs::remove_file(&lock);
    (dir, report)
}
