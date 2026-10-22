//! The `Output` trait and the `Null` and `File` backends (architecture 1.3). The `Alsa` backend
//! lives in `alsa_out.rs` (Linux only).
//!
//! An output takes interleaved stereo `f32` frames and blocks the caller until the device has
//! room, exactly as a blocking `snd_pcm_writei` does. `Null` and `File` model the device buffer
//! with [`Pacer`] so that the rest of the engine sees the same behaviour, including underruns.

use crate::convert::Converter;
use crate::fault::Fault;
use crate::negotiate::{Granted, HwRequest};
use crate::wav::{WavFormat, WavWriter};
use common::config::SampleFormat;
use common::Clock;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

/// A device error. Carries only `&'static str` and integers so that creating one on the rt-out
/// thread never allocates; the recovery thread and loggers format it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{what} ({fault:?}, errno {errno})")]
pub struct OutputError {
    pub fault: Fault,
    pub what: &'static str,
    pub errno: i32,
    /// Frames of the slice the device had already taken before the error (a partial
    /// `snd_pcm_writei` followed by a failing one). The caller must not send them again.
    pub frames_done: u32,
}

impl OutputError {
    pub fn new(fault: Fault, what: &'static str) -> Self {
        Self {
            fault,
            what,
            errno: 0,
            frames_done: 0,
        }
    }
    pub fn with_errno(fault: Fault, what: &'static str, errno: i32) -> Self {
        Self {
            fault,
            what,
            errno,
            frames_done: 0,
        }
    }
    pub fn with_frames_done(mut self, frames: u32) -> Self {
        self.frames_done = frames;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("cannot open output `{device}`: {message}")]
    Failed { device: String, message: String },
    #[error("bad output spec `{0}`")]
    BadSpec(String),
    #[error("ALSA output is only available on Linux (device `{0}`)")]
    AlsaUnavailable(String),
}

pub trait Output: Send {
    fn name(&self) -> &str;
    fn granted(&self) -> &Granted;
    /// Write interleaved stereo frames (`samples.len()` is even). Blocks until the device has
    /// taken them. Must not allocate.
    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError>;
    /// Frames queued in the device but not yet audible, or `None` if the device cannot say
    /// right now (the caller keeps its previous value rather than publishing a wrong one).
    fn delay_frames(&mut self) -> Option<i64>;
    /// Recover from an `Xrun`, `Suspended` or `Other` fault (prepare/resume).
    fn recover(&mut self, fault: Fault) -> Result<(), OutputError>;
    /// Try to open the device again after `Lost`. Called from the recovery thread only.
    fn reopen(&mut self) -> Result<(), String>;
    /// Flush and close. Called by the owner after the rt thread has returned the output.
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The device model shared by Null and paced File

/// Models a PCM buffer of `buffer` frames drained at `rate` by the clock. Starts draining when
/// full (start_threshold = buffer); an empty buffer while running is an underrun, reported by
/// the next write as `Xrun` until `recover` (the equivalent of `snd_pcm_prepare`).
pub struct Pacer {
    clock: Arc<dyn Clock>,
    rate: f64,
    buffer: f64,
    fill: f64,
    started: bool,
    last_ns: u64,
    xrun_pending: bool,
}

impl Pacer {
    pub fn new(clock: Arc<dyn Clock>, rate: u32, buffer_frames: u32) -> Self {
        Self::with_trim(clock, rate, buffer_frames, 0.0)
    }

    /// A pacer that drains at `rate` trimmed by `trim_ppm` parts per million (`rate_trim_ppm`,
    /// 1.3a): a positive value runs the station, and so the stream, that much faster than the
    /// system clock says. It cancels a measured constant offset between this box's clock and the
    /// receiver's.
    pub fn with_trim(clock: Arc<dyn Clock>, rate: u32, buffer_frames: u32, trim_ppm: f64) -> Self {
        let last_ns = clock.monotonic_ns();
        Self {
            clock,
            rate: rate as f64 * (1.0 + trim_ppm * 1e-6),
            buffer: buffer_frames as f64,
            fill: 0.0,
            started: false,
            last_ns,
            xrun_pending: false,
        }
    }

    fn update(&mut self, now: u64) {
        if self.started {
            let consumed = now.saturating_sub(self.last_ns) as f64 * self.rate / 1e9;
            if consumed > self.fill {
                self.fill = 0.0;
                self.started = false;
                self.xrun_pending = true;
            } else {
                self.fill -= consumed;
            }
        }
        self.last_ns = now;
    }

    pub fn write(&mut self, frames: usize) -> Result<(), OutputError> {
        let n = frames as f64;
        self.update(self.clock.monotonic_ns());
        if self.xrun_pending {
            return Err(OutputError::new(Fault::Xrun, "underrun"));
        }
        let space = self.buffer - self.fill;
        if space < n {
            // Blocking write: wait until enough has drained.
            let wait_ns = ((n - space) / self.rate * 1e9).ceil() as u64;
            self.clock.sleep_until_ns(self.last_ns + wait_ns);
            self.update(self.clock.monotonic_ns());
            if self.xrun_pending {
                return Err(OutputError::new(Fault::Xrun, "underrun"));
            }
        }
        self.fill += n;
        if !self.started && self.fill >= self.buffer - 0.5 {
            self.started = true;
            self.last_ns = self.clock.monotonic_ns();
        }
        Ok(())
    }

    pub fn delay(&mut self) -> i64 {
        self.update(self.clock.monotonic_ns());
        self.fill.round() as i64
    }

    pub fn recover(&mut self) {
        self.fill = 0.0;
        self.started = false;
        self.xrun_pending = false;
        self.last_ns = self.clock.monotonic_ns();
    }
}

// ---------------------------------------------------------------------------------------------
// Null

/// Discards audio. Paced by the clock like a real device, so timing tests are meaningful.
pub struct NullOutput {
    granted: Granted,
    pacer: Pacer,
}

impl NullOutput {
    pub fn new(req: &HwRequest, clock: Arc<dyn Clock>) -> Self {
        Self::with_trim(req, clock, 0.0)
    }

    pub fn with_trim(req: &HwRequest, clock: Arc<dyn Clock>, trim_ppm: f64) -> Self {
        let granted = nominal_grant(req);
        let pacer = Pacer::with_trim(clock, granted.rate, granted.buffer_frames, trim_ppm);
        Self { granted, pacer }
    }
}

impl Output for NullOutput {
    fn name(&self) -> &str {
        "null"
    }
    fn granted(&self) -> &Granted {
        &self.granted
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError> {
        self.pacer.write(samples.len() / 2)
    }
    fn delay_frames(&mut self) -> Option<i64> {
        Some(self.pacer.delay())
    }
    fn recover(&mut self, _fault: Fault) -> Result<(), OutputError> {
        self.pacer.recover();
        Ok(())
    }
    fn reopen(&mut self) -> Result<(), String> {
        self.pacer.recover();
        Ok(())
    }
}

fn nominal_grant(req: &HwRequest) -> Granted {
    Granted {
        format: req.formats.first().copied().unwrap_or(SampleFormat::S32Le),
        rate: req.rate,
        channels: req.channels,
        period_frames: req.period_frames,
        periods: req.periods,
        buffer_frames: req.period_frames * req.periods,
    }
}

// ---------------------------------------------------------------------------------------------
// File

/// Writes a WAV file. Unpaced by default ("as fast as possible", for offline rendering);
/// paced mode behaves like a real device.
pub struct FileOutput {
    granted: Granted,
    pacer: Option<Pacer>,
    wav: Option<WavWriter>,
    fmt: WavFormat,
    conv: Option<Converter>,
    f32_bytes: Vec<u8>,
    path: PathBuf,
}

impl FileOutput {
    pub fn create(
        path: &Path,
        req: &HwRequest,
        fmt: WavFormat,
        paced: Option<Arc<dyn Clock>>,
    ) -> Result<Self, OpenError> {
        Self::create_trimmed(path, req, fmt, paced, 0.0)
    }

    pub fn create_trimmed(
        path: &Path,
        req: &HwRequest,
        fmt: WavFormat,
        paced: Option<Arc<dyn Clock>>,
        trim_ppm: f64,
    ) -> Result<Self, OpenError> {
        let mut granted = nominal_grant(req);
        if let WavFormat::Int(f) = fmt {
            granted.format = f;
        }
        let wav =
            WavWriter::create(path, granted.rate, granted.channels as u16, fmt).map_err(|e| {
                OpenError::Failed {
                    device: path.display().to_string(),
                    message: e.to_string(),
                }
            })?;
        let max_samples = (granted.buffer_frames * granted.channels) as usize;
        let (conv, f32_bytes) = match fmt {
            WavFormat::F32 => (None, vec![0u8; max_samples * 4]),
            WavFormat::Int(f) => (Some(Converter::new(f, max_samples)), Vec::new()),
        };
        let pacer =
            paced.map(|c| Pacer::with_trim(c, granted.rate, granted.buffer_frames, trim_ppm));
        Ok(Self {
            granted,
            pacer,
            wav: Some(wav),
            fmt,
            conv,
            f32_bytes,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Output for FileOutput {
    fn name(&self) -> &str {
        "file"
    }
    fn granted(&self) -> &Granted {
        &self.granted
    }
    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError> {
        if let Some(p) = &mut self.pacer {
            p.write(samples.len() / 2)?;
        }
        let io_err = |e: std::io::Error| {
            OutputError::with_errno(
                Fault::Other,
                "wav write failed",
                e.raw_os_error().unwrap_or(0),
            )
        };
        let Some(wav) = self.wav.as_mut() else {
            return Err(OutputError::new(Fault::Other, "file already finished"));
        };
        match self.fmt {
            WavFormat::F32 => {
                let out = &mut self.f32_bytes[..samples.len() * 4];
                for (chunk, s) in out.chunks_exact_mut(4).zip(samples) {
                    chunk.copy_from_slice(&s.to_le_bytes());
                }
                wav.write_bytes(out).map_err(io_err)
            }
            WavFormat::Int(_) => {
                let bytes = self.conv.as_mut().expect("converter").convert(samples);
                wav.write_bytes(bytes).map_err(io_err)
            }
        }
    }
    fn delay_frames(&mut self) -> Option<i64> {
        Some(self.pacer.as_mut().map_or(0, |p| p.delay()))
    }
    fn recover(&mut self, _fault: Fault) -> Result<(), OutputError> {
        if let Some(p) = &mut self.pacer {
            p.recover();
        }
        Ok(())
    }
    fn reopen(&mut self) -> Result<(), String> {
        self.recover(Fault::Lost).map_err(|e| e.to_string())
    }
    fn finish(&mut self) -> Result<(), String> {
        match self.wav.take() {
            Some(w) => w.finish().map(|_| ()).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Opening by name

/// Where audio goes, from a device string in a station config or on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputSpec {
    /// `null`
    Null,
    /// `file:PATH` (as fast as possible) or `file-paced:PATH`
    File { path: PathBuf, paced: bool },
    /// Anything else: an ALSA device name such as `hw:CARD=PCH,DEV=0`.
    Alsa(String),
}

impl FromStr for OutputSpec {
    type Err = OpenError;
    fn from_str(s: &str) -> Result<Self, OpenError> {
        if s == "null" {
            Ok(OutputSpec::Null)
        } else if let Some(p) = s.strip_prefix("file-paced:") {
            nonempty(s, p).map(|p| OutputSpec::File {
                path: p.into(),
                paced: true,
            })
        } else if let Some(p) = s.strip_prefix("file:") {
            nonempty(s, p).map(|p| OutputSpec::File {
                path: p.into(),
                paced: false,
            })
        } else if s.trim().is_empty() {
            Err(OpenError::BadSpec(s.into()))
        } else {
            Ok(OutputSpec::Alsa(s.into()))
        }
    }
}

fn nonempty<'a>(whole: &str, p: &'a str) -> Result<&'a str, OpenError> {
    if p.is_empty() {
        Err(OpenError::BadSpec(whole.into()))
    } else {
        Ok(p)
    }
}

pub fn open_output(
    spec: &OutputSpec,
    req: &HwRequest,
    clock: Arc<dyn Clock>,
) -> Result<Box<dyn Output>, OpenError> {
    open_output_trimmed(spec, req, clock, 0.0)
}

/// [`open_output`] with `rate_trim_ppm` applied to the paced backends (`null`, `file-paced:`). An
/// ALSA device is paced by its own crystal and ignores it.
pub fn open_output_trimmed(
    spec: &OutputSpec,
    req: &HwRequest,
    clock: Arc<dyn Clock>,
    trim_ppm: f64,
) -> Result<Box<dyn Output>, OpenError> {
    match spec {
        OutputSpec::Null => Ok(Box::new(NullOutput::with_trim(req, clock, trim_ppm))),
        OutputSpec::File { path, paced } => Ok(Box::new(FileOutput::create_trimmed(
            path,
            req,
            WavFormat::F32,
            paced.then_some(clock),
            trim_ppm,
        )?)),
        #[cfg(target_os = "linux")]
        OutputSpec::Alsa(name) => Ok(Box::new(crate::alsa_out::AlsaOutput::open(name, req)?)),
        #[cfg(not(target_os = "linux"))]
        OutputSpec::Alsa(name) => Err(OpenError::AlsaUnavailable(name.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::VirtualClock;

    #[test]
    fn spec_parsing() {
        assert_eq!("null".parse::<OutputSpec>().unwrap(), OutputSpec::Null);
        assert_eq!(
            "file:/tmp/a.wav".parse::<OutputSpec>().unwrap(),
            OutputSpec::File {
                path: "/tmp/a.wav".into(),
                paced: false
            }
        );
        assert_eq!(
            "file-paced:x.wav".parse::<OutputSpec>().unwrap(),
            OutputSpec::File {
                path: "x.wav".into(),
                paced: true
            }
        );
        assert_eq!(
            "hw:CARD=PCH,DEV=0".parse::<OutputSpec>().unwrap(),
            OutputSpec::Alsa("hw:CARD=PCH,DEV=0".into())
        );
        assert!("file:".parse::<OutputSpec>().is_err());
        assert!("".parse::<OutputSpec>().is_err());
    }

    fn pacer() -> (Arc<VirtualClock>, Pacer) {
        let c = Arc::new(VirtualClock::new(0));
        let p = Pacer::new(c.clone(), 48_000, 8192);
        (c, p)
    }

    #[test]
    fn pacer_fills_then_paces() {
        let (clock, mut p) = pacer();
        // Filling the buffer does not block.
        for _ in 0..8 {
            p.write(1024).unwrap();
        }
        assert_eq!(clock.monotonic_ns(), 0);
        assert_eq!(p.delay(), 8192);
        // The ninth period must wait one period time (21.333 ms).
        p.write(1024).unwrap();
        let ms = clock.monotonic_ns() as f64 / 1e6;
        assert!((ms - 21.333).abs() < 0.01, "{ms} ms");
    }

    #[test]
    fn pacer_underrun_reports_xrun_until_recovered() {
        let (clock, mut p) = pacer();
        for _ in 0..8 {
            p.write(1024).unwrap();
        }
        clock.advance_ns(500_000_000); // a 500 ms stall: more than the 170 ms buffer
        assert_eq!(p.write(1024).unwrap_err().fault, Fault::Xrun);
        assert_eq!(p.write(1024).unwrap_err().fault, Fault::Xrun);
        p.recover();
        p.write(1024).unwrap();
    }

    #[test]
    fn pacer_short_stall_is_no_underrun() {
        let (clock, mut p) = pacer();
        for _ in 0..8 {
            p.write(1024).unwrap();
        }
        clock.advance_ns(100_000_000); // 100 ms < 170.7 ms of buffer
        p.write(1024).unwrap();
    }

    #[test]
    fn rate_trim_runs_the_pacer_faster_or_slower_by_that_many_ppm() {
        for ppm in [100.0, -100.0, 0.0] {
            let c = Arc::new(VirtualClock::new(0));
            let mut p = Pacer::with_trim(c.clone(), 48_000, 8192, ppm);
            // Fill the buffer, then write 1000 s of audio: the clock must advance by the frames over the
            // trimmed rate (the first 8,192 frames cost nothing).
            let frames = 1000 * 48_000 + 8192;
            for _ in 0..frames / 1024 {
                p.write(1024).unwrap();
            }
            let written = (frames / 1024 * 1024 - 8192) as f64;
            let want_ns = written / (48_000.0 * (1.0 + ppm * 1e-6)) * 1e9;
            let got_ns = c.monotonic_ns() as f64;
            // Within one period of rounding.
            assert!(
                (got_ns - want_ns).abs() < 25e6,
                "{ppm}: {got_ns} vs {want_ns}"
            );
            // 100 ppm over 1000 s is 100 ms, four times the tolerance.
        }
    }
}
