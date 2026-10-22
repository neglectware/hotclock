//! File decoding to the device rate: `symphonia` decode, channel mapping to stereo, `rubato`
//! resampling (architecture 1.2, 1.5, 2.3). This is off-rt code: it allocates and does file I/O,
//! and it runs only on decoder-pool threads (or in tests and offline tools).
//!
//! Output is interleaved stereo `f32` at the target rate, exactly `round(source_frames * ratio)`
//! frames long: the resampler's delay is removed at the start and its tail is flushed at the end,
//! so a file's decoded length is exact and predictable (gapless continuity depends on it).
//! Files already at the target rate skip the resampler.
//!
//! Resampler: `rubato::Async::new_sinc` with the architecture's parameters (sinc length 128,
//! Blackman-Harris 2 window, cutoff 0.95, oversampling 256) and cubic interpolation, fixed input
//! chunks of 1,024 frames. Its delay is constant for a given ratio, so it is compensated exactly.
//!
//! Channel mapping (2.3): mono is duplicated to both channels at 0 dB; more than two channels are
//! downmixed with the ITU coefficients (centre and surrounds at -3 dB, LFE dropped).
//!
//! Errors: a bad packet is skipped and replaced by silence of the packet's duration, and counted
//! (`glitches`); more than [`GLITCH_LIMIT`] of them in one file is fatal (1.5).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use std::path::Path;
use symphonia::core::audio::{AudioBufferRef, Channels, SampleBuffer};
use symphonia::core::codecs::Decoder;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatReader, SeekMode, SeekTo};
use symphonia::core::units::Time;

/// More recoverable decode errors than this in one file make it fatal (architecture 1.5).
pub const GLITCH_LIMIT: u32 = 20;
/// Input chunk of the resampler, in frames.
pub const RESAMPLE_CHUNK: usize = 1024;
/// Samples smaller in magnitude than this (-600 dBFS) are output as exact zeros, and so are NaN
/// and infinities. Float sources can carry denormals (a DAW's reverb tail rendered to 32-bit
/// float), which cost an Intel core about a hundred cycles per operation in the mixer; nothing
/// an integer source produces, even after resampling, comes near this floor, so the output of
/// those is unchanged bit for bit.
pub const SAMPLE_FLOOR: f32 = 1e-30;

/// Replace non-finite and sub-floor samples by zero (see [`SAMPLE_FLOOR`]).
pub fn sanitize(samples: &mut [f32]) {
    for x in samples {
        if !x.is_finite() || x.abs() < SAMPLE_FLOOR {
            *x = 0.0;
        }
    }
}

/// A file this many frames (or 1 percent of its length) short of its declared length is
/// reported truncated (the census uses the same 2,400).
pub const TRUNCATION_SLACK: u64 = 2400;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cannot open `{path}`: {message}")]
    Open { path: String, message: String },
    #[error("decode failed: {0}")]
    Fatal(String),
    #[error("more than {GLITCH_LIMIT} decode errors in one file")]
    TooManyGlitches,
}

/// The resampling parameters (also used by the tests that measure them).
pub fn sinc_parameters() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: Some(0.95),
        oversampling_factor: 256,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    }
}

struct Resamp {
    rs: Async<f32>,
    ratio: f64,
    /// Input frames not yet given to the resampler (interleaved stereo).
    stage: Vec<f32>,
    out: Vec<f32>,
    out_cap: usize,
    delay_left: usize,
}

pub struct FileDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    dst_rate: u32,
    src_rate: Option<u32>,
    /// Declared length in source frames from the start offset, when the container says.
    declared_src: Option<u64>,
    /// Raw decoded frames to drop before the trim applies (the run-up after a seek).
    skip_frames: u64,
    /// The shared gapless rule (`gapless::Trim`): the same one the analysis decoder applies, so a
    /// marker in milliseconds means the same sample in both.
    trim: gapless::Trim,
    mapper: Mapper,
    /// Mapped source frames of the last packet (interleaved stereo), not yet fed on when
    /// `stereo_ready`.
    stereo: Vec<f32>,
    stereo_ready: bool,
    rs: Option<Resamp>,
    glitches: u32,
    in_total: u64,
    out_total: u64,
    finished: bool,
    truncated: bool,
}

enum Step {
    Frames,
    End,
}

impl FileDecoder {
    /// Open `path` for decoding to `dst_rate`, starting `start_s` seconds into the file.
    /// The container is found by content, not by extension.
    pub fn open(path: &Path, dst_rate: u32, start_s: f64) -> Result<Self, DecodeError> {
        let open_err = |m: String| DecodeError::Open {
            path: path.display().to_string(),
            message: m,
        };
        let gapless::Opened {
            mut format,
            decoder,
            track_id,
            params,
            trim,
        } = gapless::open(path).map_err(|e| match e {
            gapless::OpenError::Open(_, m) => open_err(m),
            gapless::OpenError::NotAudio(m) => open_err(format!("not recognised as audio: {m}")),
            gapless::OpenError::NoTrack => open_err("no decodable audio track".into()),
            gapless::OpenError::NoDecoder(m) => open_err(format!("no decoder: {m}")),
        })?;

        let mut skip_frames = 0;
        let mut trim = trim;
        let mut declared_src = trim.declared(params.n_frames);
        if start_s > 0.0 {
            let start_src = params
                .sample_rate
                .map(|r| (start_s * f64::from(r)).round() as u64);
            // The raw stream carries the encoder priming in front of frame 0 (iTunSMPB files),
            // so audible frame `n` is raw frame `n + priming`.
            let prime = trim.itunes().map_or(0, |(p, _)| p);
            // Half a frame is added because `symphonia` truncates the time to a timestamp: 2.5 s
            // plus 1,024 frames of priming can come out as 121,023.99999 frames and land one frame
            // early. With it the seek lands on the frame that rounds to `start_s`, the same rule
            // `start_src` uses.
            let seek_s = match params.sample_rate {
                Some(r) => start_s + (prime as f64 + 0.5) / f64::from(r),
                None => start_s,
            };
            match format.seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::from(seek_s),
                    track_id: Some(track_id),
                },
            ) {
                Ok(s) => skip_frames = s.required_ts.saturating_sub(s.actual_ts),
                Err(_) => skip_frames = start_src.unwrap_or(0) + prime,
            }
            trim = trim.after_seek(start_src.unwrap_or(0));
            declared_src = declared_src.map(|n| n.saturating_sub(start_src.unwrap_or(0)));
        }
        let mut d = Self {
            format,
            decoder,
            track_id,
            dst_rate,
            src_rate: params.sample_rate,
            declared_src,
            skip_frames,
            trim,
            mapper: Mapper::default(),
            stereo: Vec::new(),
            stereo_ready: false,
            rs: None,
            glitches: 0,
            in_total: 0,
            out_total: 0,
            finished: false,
            truncated: false,
        };
        if start_s > 0.0 {
            d.decoder.reset();
        }
        // The rate is needed to build the resampler and to state the length; some containers
        // (ALAC in MP4) do not carry it, so decode the first packet to learn it.
        if d.src_rate.is_none() {
            if let Step::Frames = d.read_source()? {
                d.stereo_ready = true;
            }
        }
        if let Some(r) = d.src_rate {
            d.ensure_resampler(r)?;
        }
        Ok(d)
    }

    pub fn source_rate(&self) -> Option<u32> {
        self.src_rate
    }

    fn ratio(&self) -> Option<f64> {
        self.src_rate
            .map(|r| f64::from(self.dst_rate) / f64::from(r))
    }

    /// The length in output frames the container declares, if it does (a hint for MP3 and AAC).
    pub fn expected_frames(&self) -> Option<u64> {
        let ratio = self.ratio()?;
        self.declared_src.map(|n| (n as f64 * ratio).round() as u64)
    }

    pub fn glitches(&self) -> u32 {
        self.glitches
    }

    /// Frames delivered so far (after the resampler's delay is removed).
    pub fn frames_out(&self) -> u64 {
        self.out_total
    }

    /// True once the end was reached and the file was clearly cut short.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    fn ensure_resampler(&mut self, src_rate: u32) -> Result<(), DecodeError> {
        self.src_rate = Some(src_rate);
        if src_rate == self.dst_rate || self.rs.is_some() {
            return Ok(());
        }
        let ratio = f64::from(self.dst_rate) / f64::from(src_rate);
        let rs = Async::<f32>::new_sinc(
            ratio,
            1.0,
            &sinc_parameters(),
            RESAMPLE_CHUNK,
            2,
            FixedAsync::Input,
        )
        .map_err(|e| DecodeError::Fatal(format!("resampler: {e}")))?;
        let out_cap = rs.output_frames_max();
        let delay_left = rs.output_delay();
        self.rs = Some(Resamp {
            rs,
            ratio,
            stage: Vec::with_capacity(RESAMPLE_CHUNK * 4),
            out: vec![0.0; out_cap * 2],
            out_cap,
            delay_left,
        });
        Ok(())
    }

    /// Decode one packet's worth and append the result, at the target rate, to `out`. Returns
    /// `Ok(true)` while there may be more, `Ok(false)` when the file is finished (everything,
    /// including the resampler's tail, has been appended). After `Ok(false)` further calls
    /// append nothing.
    pub fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<bool, DecodeError> {
        let from = out.len();
        let r = self.decode_next_raw(out);
        sanitize(&mut out[from..]);
        r
    }

    fn decode_next_raw(&mut self, out: &mut Vec<f32>) -> Result<bool, DecodeError> {
        if self.finished {
            return Ok(false);
        }
        if !self.stereo_ready {
            match self.read_source()? {
                Step::Frames => {}
                Step::End => {
                    self.finish(out)?;
                    return Ok(false);
                }
            }
        }
        self.stereo_ready = false;
        self.feed_source(out)?;
        Ok(true)
    }

    /// Read and decode one packet into `self.stereo` (source rate). Bad packets become silence.
    fn read_source(&mut self) -> Result<Step, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                // Symphonia signals the normal end of a stream as an unexpected EOF.
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(Step::End)
                }
                Err(SymError::ResetRequired) => return Ok(Step::End),
                Err(SymError::DecodeError(_)) => {
                    self.note_glitch()?;
                    continue;
                }
                Err(e) => return Err(DecodeError::Fatal(format!("read error: {e}"))),
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    if buf.frames() == 0 {
                        continue;
                    }
                    let rate = self.mapper.map(buf, &mut self.stereo);
                    self.src_rate.get_or_insert(rate);
                    return Ok(Step::Frames);
                }
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => {
                    self.note_glitch()?;
                    // Silence for the packet's duration keeps the timeline and the length.
                    let dur = packet.dur as usize;
                    self.stereo.clear();
                    self.stereo.resize(dur * 2, 0.0);
                    if dur == 0 {
                        continue;
                    }
                    return Ok(Step::Frames);
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Fatal(format!("decode error: {e}"))),
            }
        }
    }

    fn note_glitch(&mut self) -> Result<(), DecodeError> {
        self.glitches += 1;
        if self.glitches > GLITCH_LIMIT {
            Err(DecodeError::TooManyGlitches)
        } else {
            Ok(())
        }
    }

    /// Feed `self.stereo` (after the start skip) to the resampler or straight through.
    fn feed_source(&mut self, out: &mut Vec<f32>) -> Result<(), DecodeError> {
        let frames = self.stereo.len() / 2;
        let mut lo = 0usize;
        if self.skip_frames > 0 {
            let s = (self.skip_frames as usize).min(frames);
            self.skip_frames -= s as u64;
            lo = s;
        }
        // The shared trim (encoder priming and padding), after the seek run-up.
        let w = self.trim.window(frames - lo);
        let (start, end) = (lo + w.start, lo + w.end);
        let n = end - start;
        if n == 0 {
            return Ok(());
        }
        // Before the resampler, so that one NaN cannot spread over the filter's length.
        sanitize(&mut self.stereo[start * 2..end * 2]);
        self.in_total += n as u64;
        let src_rate = self.src_rate.unwrap_or(self.dst_rate);
        if src_rate == self.dst_rate {
            out.extend_from_slice(&self.stereo[start * 2..end * 2]);
            self.out_total += n as u64;
            return Ok(());
        }
        self.ensure_resampler(src_rate)?;
        let rs = self.rs.as_mut().expect("resampler");
        rs.stage.extend_from_slice(&self.stereo[start * 2..end * 2]);
        let mut emitted = 0u64;
        loop {
            let need = rs.rs.input_frames_next();
            if rs.stage.len() / 2 < need {
                break;
            }
            emitted += run_chunk(rs, need, None, out)?;
            rs.stage.drain(..need * 2);
        }
        self.out_total += emitted;
        Ok(())
    }

    /// End of file: flush the resampler and trim to the exact length.
    fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), DecodeError> {
        self.finished = true;
        if let Some(declared) = self.declared_src {
            let got = self.in_total;
            if declared.saturating_sub(got) > TRUNCATION_SLACK.max(declared / 100) {
                self.truncated = true;
            }
        }
        let Some(rs) = self.rs.as_mut() else {
            return Ok(());
        };
        let target = (self.in_total as f64 * rs.ratio).round() as u64;
        let start_len = out.len();
        let mut emitted = self.out_total;
        let left = rs.stage.len() / 2;
        if left > 0 {
            let need = rs.rs.input_frames_next();
            rs.stage.resize(need * 2, 0.0);
            emitted += run_chunk(rs, need, Some(left), out)?;
            rs.stage.clear();
        }
        let mut guard = 0;
        while emitted < target && guard < 10_000 {
            let need = rs.rs.input_frames_next();
            rs.stage.clear();
            rs.stage.resize(need * 2, 0.0);
            emitted += run_chunk(rs, need, Some(0), out)?;
            guard += 1;
        }
        if emitted > target {
            let excess = ((emitted - target) as usize) * 2;
            let keep = out.len().saturating_sub(excess).max(start_len);
            out.truncate(keep);
            emitted = target;
        }
        self.out_total = emitted;
        Ok(())
    }
}

/// Channel mapping to interleaved stereo, with its scratch buffers.
#[derive(Default)]
struct Mapper {
    weights: Vec<(f32, f32)>,
    weights_for: Option<Channels>,
    sbuf: Option<SampleBuffer<f32>>,
}

impl Mapper {
    /// Convert a decoded buffer to interleaved stereo `f32` in `stereo`. Returns the buffer's
    /// sample rate.
    fn map(&mut self, buf: AudioBufferRef<'_>, stereo: &mut Vec<f32>) -> u32 {
        let spec = *buf.spec();
        let frames = buf.frames();
        let need = frames * spec.channels.count();
        if self.sbuf.as_ref().is_none_or(|s| s.capacity() < need) {
            self.sbuf = Some(SampleBuffer::<f32>::new(frames as u64, spec));
        }
        let sb = self.sbuf.as_mut().expect("just made");
        sb.copy_planar_ref(buf);
        let planar = sb.samples();
        if self.weights_for != Some(spec.channels) {
            self.weights = channel_weights(spec.channels);
            self.weights_for = Some(spec.channels);
        }
        stereo.clear();
        stereo.resize(frames * 2, 0.0);
        let nch = self.weights.len();
        if nch == 1 {
            for (o, &s) in stereo.chunks_exact_mut(2).zip(&planar[..frames]) {
                o[0] = s;
                o[1] = s;
            }
        } else if nch == 2 {
            let (l, r) = planar.split_at(frames);
            for ((o, &a), &b) in stereo.chunks_exact_mut(2).zip(l).zip(&r[..frames]) {
                o[0] = a;
                o[1] = b;
            }
        } else {
            for (c, &(wl, wr)) in self.weights.iter().enumerate() {
                let ch = &planar[c * frames..(c + 1) * frames];
                if wl != 0.0 || wr != 0.0 {
                    for (o, &s) in stereo.chunks_exact_mut(2).zip(ch) {
                        o[0] += s * wl;
                        o[1] += s * wr;
                    }
                }
            }
        }
        spec.rate
    }
}

/// Run the resampler on the first `need` frames of its stage; append the output (less the
/// resampler's delay, which is dropped from the very first output). Returns frames appended.
fn run_chunk(
    rs: &mut Resamp,
    need: usize,
    partial: Option<usize>,
    out: &mut Vec<f32>,
) -> Result<u64, DecodeError> {
    let indexing = Indexing {
        input_offset: 0,
        output_offset: 0,
        partial_len: partial,
        active_channels_mask: None,
    };
    let input = InterleavedSlice::new(&rs.stage[..need * 2], 2, need)
        .map_err(|e| DecodeError::Fatal(format!("resampler input: {e}")))?;
    let mut output = InterleavedSlice::new_mut(&mut rs.out, 2, rs.out_cap)
        .map_err(|e| DecodeError::Fatal(format!("resampler output: {e}")))?;
    let (_nin, nout) = rs
        .rs
        .process_into_buffer(&input, &mut output, Some(&indexing))
        .map_err(|e| DecodeError::Fatal(format!("resampler: {e}")))?;
    let drop = rs.delay_left.min(nout);
    rs.delay_left -= drop;
    out.extend_from_slice(&rs.out[drop * 2..nout * 2]);
    Ok((nout - drop) as u64)
}

/// Per-channel contributions to (left, right), in the order the decoder lays channels out.
/// Mono is handled by the caller as a duplicate at 0 dB.
fn channel_weights(ch: Channels) -> Vec<(f32, f32)> {
    const M3: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let n = ch.count();
    if n <= 2 {
        return vec![(1.0, 1.0); n.max(1)];
    }
    let mut v = Vec::with_capacity(n);
    for (i, c) in ch.iter().enumerate() {
        v.push(if c == Channels::FRONT_LEFT {
            (1.0, 0.0)
        } else if c == Channels::FRONT_RIGHT {
            (0.0, 1.0)
        } else if c == Channels::FRONT_CENTRE || c == Channels::REAR_CENTRE {
            (M3, M3)
        } else if c == Channels::LFE1 || c == Channels::LFE2 {
            (0.0, 0.0)
        } else if c == Channels::REAR_LEFT
            || c == Channels::SIDE_LEFT
            || c == Channels::FRONT_LEFT_CENTRE
            || c == Channels::FRONT_LEFT_WIDE
            || c == Channels::TOP_FRONT_LEFT
            || c == Channels::TOP_REAR_LEFT
        {
            (M3, 0.0)
        } else if c == Channels::REAR_RIGHT
            || c == Channels::SIDE_RIGHT
            || c == Channels::FRONT_RIGHT_CENTRE
            || c == Channels::FRONT_RIGHT_WIDE
            || c == Channels::TOP_FRONT_RIGHT
            || c == Channels::TOP_REAR_RIGHT
        {
            (0.0, M3)
        } else if i % 2 == 0 {
            (M3, 0.0)
        } else {
            (0.0, M3)
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_stereo_weights() {
        assert_eq!(channel_weights(Channels::FRONT_LEFT), vec![(1.0, 1.0)]);
        assert_eq!(
            channel_weights(Channels::FRONT_LEFT | Channels::FRONT_RIGHT).len(),
            2
        );
    }

    #[test]
    fn itu_downmix_weights() {
        let five_one = Channels::FRONT_LEFT
            | Channels::FRONT_RIGHT
            | Channels::FRONT_CENTRE
            | Channels::LFE1
            | Channels::REAR_LEFT
            | Channels::REAR_RIGHT;
        let w = channel_weights(five_one);
        let m3 = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(
            w,
            vec![
                (1.0, 0.0),
                (0.0, 1.0),
                (m3, m3),
                (0.0, 0.0),
                (m3, 0.0),
                (0.0, m3)
            ]
        );
    }
}
