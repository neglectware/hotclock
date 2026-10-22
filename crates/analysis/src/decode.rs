//! Decoding a file at its own sample rate and channel count, block by block. Used by the analysis
//! pass and by the exact window-loudness job. Probing goes by content, never by extension.

use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::errors::Error as SymError;

/// Encoder delay and padding from an iTunes `iTunSMPB` tag (see [`gapless::itunsmpb`], the shared
/// rule playout uses too).
pub use gapless::itunsmpb;

/// A frame shortfall smaller than this against the declared length (one MP3 frame plus decoder
/// delay, or AAC priming) is not truncation. Same figure as the census.
const TRUNCATION_TOLERANCE_FRAMES: u64 = 2_400;

/// More failed packets than this and the file is rejected (the playout engine tolerates the same).
pub const MAX_DECODE_ERRORS: u32 = 20;

#[derive(Debug, Clone, Default)]
pub struct StreamInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub bits_per_sample: Option<u32>,
    pub declared_frames: Option<u64>,
    pub frames: u64,
    pub decode_errors: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cannot open {0}: {1}")]
    Open(String, String),
    #[error("not recognised as audio: {0}")]
    NotAudio(String),
    #[error("no decodable audio track")]
    NoTrack,
    #[error("no decoder for this codec: {0}")]
    NoDecoder(String),
    #[error("decode failed after {frames} frames: {msg}")]
    Failed { frames: u64, msg: String },
    #[error("file is truncated: decoded {got} of {declared} declared frames ({pct:.1} percent)")]
    Truncated { got: u64, declared: u64, pct: f64 },
    #[error("file has no audio")]
    Empty,
    #[error("{0} packets failed to decode (limit {MAX_DECODE_ERRORS})")]
    TooManyErrors(u32),
}

/// Decode `path` completely, calling `sink(interleaved_f32, channels, rate)` for every decoded
/// packet, in order. A packet that fails to decode is replaced by silence of its duration (the
/// previous packet's length) and counted; more than [`MAX_DECODE_ERRORS`] is an error.
pub fn decode_file<F>(path: &Path, mut sink: F) -> Result<StreamInfo, DecodeError>
where
    F: FnMut(&[f32], usize, u32),
{
    let gapless::Opened {
        mut format,
        mut decoder,
        track_id,
        params,
        mut trim,
    } = gapless::open(path).map_err(|e| match e {
        gapless::OpenError::Open(p, m) => DecodeError::Open(p, m),
        gapless::OpenError::NotAudio(m) => DecodeError::NotAudio(m),
        gapless::OpenError::NoTrack => DecodeError::NoTrack,
        gapless::OpenError::NoDecoder(m) => DecodeError::NoDecoder(m),
    })?;
    let mut info = StreamInfo {
        codec: symphonia::default::get_codecs()
            .get_codec(params.codec)
            .map(|d| d.short_name.to_string())
            .unwrap_or_default(),
        sample_rate: params.sample_rate.unwrap_or(0),
        channels: params.channels.map_or(0, |c| c.count()),
        bits_per_sample: params.bits_per_sample.or(params.bits_per_coded_sample),
        declared_frames: params.n_frames,
        ..Default::default()
    };
    let trim0 = trim;
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    let mut last_packet_frames = 0usize;
    let mut fatal: Option<String> = None;
    let mut silence: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::DecodeError(_)) => {
                info.decode_errors += 1;
                continue;
            }
            Err(e) => {
                fatal = Some(format!("read error: {e}"));
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                if info.sample_rate == 0 {
                    info.sample_rate = spec.rate;
                }
                if info.channels == 0 {
                    info.channels = spec.channels.count();
                }
                let n = buf.frames();
                if n == 0 {
                    continue;
                }
                let need = n * spec.channels.count();
                if sbuf.as_ref().is_none_or(|s| s.capacity() < need) {
                    sbuf = Some(SampleBuffer::new(buf.capacity() as u64, spec));
                }
                let sb = sbuf.as_mut().unwrap();
                sb.copy_interleaved_ref(buf);
                let ch = spec.channels.count();
                // The shared trim rule (`gapless::Trim`): the same call the playout decoder makes.
                let keep = trim.window(n);
                let samples = &sb.samples()[keep.start * ch..keep.end * ch];
                let frames = (keep.end - keep.start) as u64;
                if frames > 0 {
                    sink(samples, ch, spec.rate);
                    info.frames += frames;
                }
                last_packet_frames = n;
            }
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                info.decode_errors += 1;
                if info.decode_errors > MAX_DECODE_ERRORS {
                    return Err(DecodeError::TooManyErrors(info.decode_errors));
                }
                if last_packet_frames > 0 && info.channels > 0 {
                    silence.clear();
                    silence.resize(last_packet_frames * info.channels, 0.0);
                    sink(&silence, info.channels, info.sample_rate);
                    info.frames += last_packet_frames as u64;
                }
            }
            Err(e) => {
                fatal = Some(format!("decode error: {e}"));
                break;
            }
        }
    }
    if let Some(msg) = fatal {
        return Err(DecodeError::Failed {
            frames: info.frames,
            msg,
        });
    }
    if info.frames == 0 {
        return Err(DecodeError::Empty);
    }
    if info.decode_errors > MAX_DECODE_ERRORS {
        return Err(DecodeError::TooManyErrors(info.decode_errors));
    }
    let declared = trim0.declared(info.declared_frames);
    if let Some(decl) = declared {
        if info.frames + TRUNCATION_TOLERANCE_FRAMES < decl {
            return Err(DecodeError::Truncated {
                got: info.frames,
                declared: decl,
                pct: info.frames as f64 * 100.0 / decl as f64,
            });
        }
    }
    Ok(info)
}
