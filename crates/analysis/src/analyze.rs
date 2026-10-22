//! The analysis pass (architecture 3.2): one decode of the whole file, then loudness, the
//! envelopes, the ramps and the auto markers. This is also the decode test of the import.

use crate::blocks::{integrated_from_blocks, BlockAnalyzer, Blocks};
use crate::decode::{decode_file, DecodeError, StreamInfo};
use crate::effective::{effective_markers, Effective, EffectiveInput, Overrides};
use crate::env::{Envelopes, StoredEnvelopes};
use crate::markers::*;
use crate::ramp::{compute_ramps, format_ramp};
use ebur128::{Channel, EbuR128, Mode};
use std::collections::BTreeSet;
use std::path::Path;

/// Bump when any algorithm here changes what it stores (3.2, "Algorithm version"). 2: the
/// `truncated_fade` flag. An item analyzed by an older version can be analyzed again
/// (`POST /api/v1/reanalyze {"stale": true}`, `hotclock reanalyze --stale`).
pub const ALGO_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("the file is only {0} ms long; the minimum is 250 ms")]
    TooShort(i64),
    #[error("no sound found in the first 60 seconds (no_audio)")]
    NoAudio,
    #[error("loudness measurement failed: {0}")]
    Loudness(String),
}

#[derive(Debug, Clone)]
pub struct FileAnalysis {
    pub info: StreamInfo,
    pub file_ms: i64,
    pub l_int: f64,
    pub lra: f64,
    /// dBTP, max over channels.
    pub true_peak: f64,
    /// dBFS, max over channels.
    pub sample_peak: f64,
    pub env: Envelopes,
    pub stored: StoredEnvelopes,
    pub wave: Vec<u8>,
    pub ramp_start: String,
    pub ramp_end: String,
    /// Flags independent of markers (`clipped`, `gapless_unknown`, `truncated_fade`).
    pub base_flags: BTreeSet<Flag>,
    /// The auto markers with the profile's defaults and no overrides.
    pub auto: Effective,
    /// The integrated loudness of the same audio by this crate's own gate (equals `l_int`; kept to
    /// cross-check against the `ebur128` crate).
    pub l_int_crosscheck: f64,
    pub decode_warnings: u32,
    /// The end of the file as the `truncated_fade` detector measured it.
    pub tail: crate::tail::TailReport,
}

fn to_db(x: f64) -> f64 {
    if x > 1e-6 {
        20.0 * x.log10()
    } else {
        -120.0
    }
}

fn mp3_lacks_gapless_header(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; 8192];
    let n = f.read(&mut head).unwrap_or(0);
    head.truncate(n);
    let is_mp3 =
        head.starts_with(b"ID3") || (head.len() > 1 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0);
    if !is_mp3 {
        return false;
    }
    // Skip an ID3v2 tag, then look for the Xing/Info/LAME header in the first frame.
    let mut off = 0usize;
    if head.starts_with(b"ID3") && head.len() >= 10 {
        let sz = ((head[6] as usize & 0x7f) << 21)
            | ((head[7] as usize & 0x7f) << 14)
            | ((head[8] as usize & 0x7f) << 7)
            | (head[9] as usize & 0x7f);
        off = 10 + sz;
    }
    let mut buf = vec![0u8; 4096];
    if let Ok(mut f2) = std::fs::File::open(path) {
        use std::io::{Seek, SeekFrom};
        if f2.seek(SeekFrom::Start(off as u64)).is_ok() {
            let n = f2.read(&mut buf).unwrap_or(0);
            buf.truncate(n);
        }
    }
    // The Xing/Info tag sits at a fixed place in the first frame, right after the side
    // information (offset 36 for MPEG-1 stereo, 21 for MPEG-1 mono and MPEG-2 stereo, 13 for
    // MPEG-2 mono, plus 2 with a CRC). The string "LAME" alone proves nothing: libmp3lame writes
    // its version into the ancillary data of ordinary frames, tag or no tag (found 2026-09-29 with
    // ffmpeg `-write_xing 0`), and that made the flag miss headerless files.
    let Some(sync) =
        (0..buf.len().saturating_sub(1)).find(|&i| buf[i] == 0xFF && buf[i + 1] & 0xE0 == 0xE0)
    else {
        return false;
    };
    let frame = &buf[sync..];
    let tag_at = |o: usize| matches!(frame.get(o..o + 4), Some(b"Xing" | b"Info"));
    !([13, 15, 21, 23, 36, 38].into_iter().any(tag_at))
}

/// Decode `path` completely and analyze it with `profile` (the item's category profile).
pub fn analyze_file(path: &Path, profile: &ProfileParams) -> Result<FileAnalysis, AnalysisError> {
    let mut ba: Option<BlockAnalyzer> = None;
    let mut ebu: Option<EbuR128> = None;
    let mut ebu_err: Option<String> = None;
    let info = decode_file(path, |s, ch, rate| {
        let b = ba.get_or_insert_with(|| BlockAnalyzer::new(rate, ch));
        b.push(s);
        if ebu.is_none() && ebu_err.is_none() {
            match EbuR128::new(
                ch as u32,
                rate,
                Mode::I | Mode::LRA | Mode::TRUE_PEAK | Mode::SAMPLE_PEAK,
            ) {
                Ok(mut e) => {
                    if ch == 1 {
                        let _ = e.set_channel_map(&[Channel::DualMono]);
                    }
                    ebu = Some(e);
                }
                Err(e) => ebu_err = Some(e.to_string()),
            }
        }
        if let Some(e) = ebu.as_mut() {
            if let Err(err) = e.add_frames_f32(s) {
                ebu_err = Some(err.to_string());
            }
        }
    })?;
    let Some(ba) = ba else {
        return Err(DecodeError::Empty.into());
    };
    if let Some(e) = ebu_err {
        return Err(AnalysisError::Loudness(e));
    }
    let ebu = ebu.ok_or_else(|| AnalysisError::Loudness("no audio".into()))?;
    let blocks: Blocks = ba.finish();
    let file_ms = (blocks.frames as f64 * 1000.0 / f64::from(blocks.rate)).round() as i64;
    if file_ms < MIN_WINDOW_MS {
        return Err(AnalysisError::TooShort(file_ms));
    }

    let full = blocks.full_blocks();
    let own = integrated_from_blocks(&blocks.kpow[..full]);
    let crate_l = ebu.loudness_global().ok().filter(|l| l.is_finite());
    // Both are the same BS.1770 measurement; the crate's value is the one stored (it handles
    // the last partial 100 ms the way the standard's reference does). A file entirely under the
    // absolute gate has no integrated loudness: fall back to a fixed floor.
    let l_int = crate_l.or(own).unwrap_or(-70.0);
    let lra = ebu.loudness_range().unwrap_or(0.0);
    let mut tp = 0.0f64;
    let mut sp = 0.0f64;
    for c in 0..blocks.channels as u32 {
        tp = tp.max(ebu.true_peak(c).unwrap_or(0.0));
        sp = sp.max(ebu.sample_peak(c).unwrap_or(0.0));
    }

    let env = Envelopes::from_blocks(&blocks);
    let stored = env.to_stored();
    let wave: Vec<u8> = blocks.wave().iter().map(|&v| v as u8).collect();

    let mut base_flags = BTreeSet::new();
    if blocks.clipped {
        base_flags.insert(Flag::Clipped);
    }
    if mp3_lacks_gapless_header(path) {
        base_flags.insert(Flag::GaplessUnknown);
    }
    let tail = crate::tail::truncated_fade(&blocks);
    if tail.truncated {
        base_flags.insert(Flag::TruncatedFade);
    }

    let facts = FileFacts {
        file_ms,
        l_int,
        lra,
    };
    let auto = effective_markers(&EffectiveInput {
        facts,
        env: &env,
        profile,
        ovr: Overrides::default(),
        policy: LongIntroPolicy::Play,
        base_flags: &base_flags,
        l_w_exact: None,
    });
    if auto.flags.contains(&Flag::NoAudio) {
        return Err(AnalysisError::NoAudio);
    }

    let duration_s = blocks.frames as f64 / f64::from(blocks.rate);
    let kpow = env.kpow();
    let (rs, re) = compute_ramps(&kpow[..full.min(kpow.len())], l_int, 0, full, duration_s);

    Ok(FileAnalysis {
        decode_warnings: info.decode_errors,
        info,
        file_ms,
        l_int,
        lra,
        true_peak: to_db(tp),
        sample_peak: to_db(sp),
        env,
        stored,
        wave,
        ramp_start: format_ramp(&rs),
        ramp_end: format_ramp(&re),
        base_flags,
        auto,
        l_int_crosscheck: own.unwrap_or(-70.0),
        tail,
    })
}

/// Exact loudness `L_W` of the audio between `c_in` and `c_out` ms (BS.1770 gating over that
/// range only), by decoding the file again. The background job of 3.3.
pub fn window_loudness_exact(path: &Path, c_in: i64, c_out: i64) -> Result<f64, AnalysisError> {
    let mut ba: Option<BlockAnalyzer> = None;
    let mut pos: u64 = 0;
    let mut range: Option<(u64, u64)> = None;
    decode_file(path, |s, ch, rate| {
        let (a, b) = *range.get_or_insert_with(|| {
            (
                (c_in.max(0) as u64 * u64::from(rate)) / 1000,
                (c_out.max(0) as u64 * u64::from(rate)) / 1000,
            )
        });
        let n = (s.len() / ch) as u64;
        let lo = a.saturating_sub(pos).min(n);
        let hi = b.saturating_sub(pos).min(n);
        if hi > lo {
            ba.get_or_insert_with(|| BlockAnalyzer::new(rate, ch))
                .push(&s[(lo as usize) * ch..(hi as usize) * ch]);
        }
        pos += n;
    })?;
    let blocks = ba
        .ok_or_else(|| AnalysisError::Loudness("empty window".into()))?
        .finish();
    let full = blocks.full_blocks();
    integrated_from_blocks(&blocks.kpow[..full])
        .ok_or_else(|| AnalysisError::Loudness("window is silent".into()))
}
