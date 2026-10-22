//! The ALSA backend (Linux only). Opens the device by name (`hw:CARD=PCH,DEV=0`), negotiates the
//! hardware parameters per architecture 1.3 (the decisions are in `negotiate.rs`; this file only
//! makes the calls), and writes through `snd_pcm_writei`. Never `plughw:`: all conversion is in
//! the engine.

use crate::convert::Converter;
use crate::fault::{classify_errno, Fault, EAGAIN};
use crate::negotiate::{negotiate, Granted, HwProbe, HwRequest};
use crate::output::{OpenError, Output, OutputError};
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use common::config::SampleFormat;

pub struct AlsaOutput {
    name: String,
    req: HwRequest,
    pcm: Option<PCM>,
    granted: Granted,
    conv: Converter,
    frame_bytes: usize,
}

fn alsa_format(f: SampleFormat) -> Format {
    match f {
        SampleFormat::S32Le => Format::S32LE,
        SampleFormat::S24_3Le => Format::S243LE,
        SampleFormat::S16Le => Format::S16LE,
    }
}

struct AlsaProbe<'a> {
    pcm: &'a PCM,
    hwp: &'a HwParams<'a>,
}

fn e2s(e: alsa::Error) -> String {
    e.to_string()
}

impl HwProbe for AlsaProbe<'_> {
    fn accepts_channels(&mut self, c: u32) -> bool {
        self.hwp.test_channels(c).is_ok()
    }
    fn accepts_format(&mut self, f: SampleFormat) -> bool {
        self.hwp.test_format(alsa_format(f)).is_ok()
    }
    fn accepts_rate(&mut self, r: u32) -> bool {
        self.hwp.test_rate(r).is_ok()
    }
    fn set_channels(&mut self, c: u32) -> Result<(), String> {
        self.hwp.set_channels(c).map_err(e2s)
    }
    fn set_format(&mut self, f: SampleFormat) -> Result<(), String> {
        self.hwp.set_format(alsa_format(f)).map_err(e2s)
    }
    fn set_rate(&mut self, r: u32) -> Result<(), String> {
        self.hwp.set_rate(r, ValueOr::Nearest).map_err(e2s)
    }
    fn set_period_frames_near(&mut self, frames: u32) -> Result<u32, String> {
        self.hwp
            .set_period_size_near(i64::from(frames), ValueOr::Nearest)
            .map(|v| v as u32)
            .map_err(e2s)
    }
    fn set_periods_near(&mut self, periods: u32) -> Result<u32, String> {
        self.hwp
            .set_periods_near(periods, ValueOr::Nearest)
            .map_err(e2s)
    }
    fn commit(&mut self) -> Result<(u32, u32, u32), String> {
        self.pcm.hw_params(self.hwp).map_err(e2s)?;
        let cur = self.pcm.hw_params_current().map_err(e2s)?;
        Ok((
            cur.get_period_size().map_err(e2s)? as u32,
            cur.get_periods().map_err(e2s)?,
            cur.get_buffer_size().map_err(e2s)? as u32,
        ))
    }
}

fn open_pcm(name: &str, req: &HwRequest) -> Result<(PCM, Granted), String> {
    open_pcm_dir(name, req, Direction::Playback)
}

/// The start threshold, in frames: the whole buffer for playback, one frame for capture. Kept apart
/// from the syscalls so a test can pin it.
pub(crate) fn start_threshold(capture: bool, buffer_frames: u32) -> i64 {
    if capture {
        1
    } else {
        i64::from(buffer_frames)
    }
}

/// Open `name` for playback or capture and negotiate the parameters (1.3). Capture is for `hotclock
/// loopcheck` (M1.8): the same negotiation, the same refusal of plugin conversion.
pub(crate) fn open_pcm_dir(
    name: &str,
    req: &HwRequest,
    dir: Direction,
) -> Result<(PCM, Granted), String> {
    // A blocking `snd_pcm_open` of a `hw:` device that another process holds does not fail: the
    // kernel waits (without timeout) until the substream is free. That would hang start-up, or
    // the recovery thread for good after a replug if something else grabbed the card. So probe
    // with a non-blocking open first (EBUSY at once), close it, then open blocking for the
    // blocking `writei` the rt thread relies on. (The `alsa` crate has no safe
    // `snd_pcm_nonblock`, and this crate forbids `unsafe`.)
    drop(PCM::new(name, dir, true).map_err(|e| format!("snd_pcm_open: {e}"))?);
    let pcm = PCM::new(name, dir, false).map_err(|e| format!("snd_pcm_open: {e}"))?;
    let granted = {
        let hwp = HwParams::any(&pcm).map_err(|e| format!("hw_params_any: {e}"))?;
        hwp.set_access(Access::RWInterleaved)
            .map_err(|e| format!("interleaved read/write access not supported: {e}"))?;
        // No plugin resampling: the engine owns all rate conversion.
        let _ = hwp.set_rate_resample(false);
        let mut probe = AlsaProbe {
            pcm: &pcm,
            hwp: &hwp,
        };
        negotiate(&mut probe, req).map_err(|e| e.to_string())?
    };
    {
        let swp = pcm
            .sw_params_current()
            .map_err(|e| format!("sw_params_current: {e}"))?;
        // Playback starts once the buffer is full. Capture must start on the first read: a capture
        // stream only auto-starts when one read asks for at least `start_threshold` frames, and
        // `Capture::read` asks for one period, so a threshold of the whole buffer never starts it
        // and `snd_pcm_readi` fails with EIO (found on Box A's PCH, 2026-10-01).
        let start = start_threshold(dir == Direction::Capture, granted.buffer_frames);
        swp.set_start_threshold(start)
            .map_err(|e| format!("start_threshold: {e}"))?;
        swp.set_avail_min(i64::from(granted.period_frames))
            .map_err(|e| format!("avail_min: {e}"))?;
        pcm.sw_params(&swp).map_err(|e| format!("sw_params: {e}"))?;
    }
    Ok((pcm, granted))
}

impl AlsaOutput {
    pub fn open(name: &str, req: &HwRequest) -> Result<Self, OpenError> {
        let (pcm, granted) = open_pcm(name, req).map_err(|message| OpenError::Failed {
            device: name.to_string(),
            message,
        })?;
        let conv = Converter::new(
            granted.format,
            (granted.buffer_frames * granted.channels) as usize,
        );
        let frame_bytes = granted.format.bytes_per_sample() * granted.channels as usize;
        Ok(Self {
            name: name.to_string(),
            req: req.clone(),
            pcm: Some(pcm),
            granted,
            conv,
            frame_bytes,
        })
    }
}

impl Output for AlsaOutput {
    fn name(&self) -> &str {
        &self.name
    }

    fn granted(&self) -> &Granted {
        &self.granted
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), OutputError> {
        let pcm = self
            .pcm
            .as_ref()
            .ok_or(OutputError::new(Fault::Lost, "device closed"))?;
        let bytes = self.conv.convert(samples);
        let io = pcm.io_bytes();
        let mut off = 0;
        while off < bytes.len() {
            match io.writei(&bytes[off..]) {
                Ok(frames) => off += frames * self.frame_bytes,
                Err(e) => match classify_errno(e.errno()) {
                    Fault::Retry => continue,
                    fault => {
                        // Frames taken by earlier partial writes of this slice are in the device
                        // (and after an underrun, already played): report them so the caller
                        // resends only the rest.
                        let done = (off / self.frame_bytes) as u32;
                        return Err(OutputError::with_errno(fault, "snd_pcm_writei", e.errno())
                            .with_frames_done(done));
                    }
                },
            }
        }
        Ok(())
    }

    fn delay_frames(&mut self) -> Option<i64> {
        self.pcm.as_ref().and_then(|p| p.delay().ok())
    }

    fn recover(&mut self, fault: Fault) -> Result<(), OutputError> {
        let pcm = self
            .pcm
            .as_ref()
            .ok_or(OutputError::new(Fault::Lost, "device closed"))?;
        if fault == Fault::Suspended {
            // As `snd_pcm_recover` does: resume while it says EAGAIN (the hardware is waking; up
            // to 10 s here), and prepare only if resume did not succeed (ENOSYS: the driver cannot
            // resume). A successful resume puts the stream back where it was, with its buffered
            // audio; preparing after it would throw that audio away.
            let mut resumed = false;
            for _ in 0..100 {
                match pcm.resume() {
                    Ok(()) => {
                        resumed = true;
                        break;
                    }
                    Err(e) if e.errno() == EAGAIN => {
                        std::thread::sleep(std::time::Duration::from_millis(100))
                    }
                    Err(_) => break,
                }
            }
            if resumed {
                return Ok(());
            }
        }
        pcm.prepare().map_err(|e| {
            OutputError::with_errno(classify_errno(e.errno()), "snd_pcm_prepare", e.errno())
        })
    }

    fn reopen(&mut self) -> Result<(), String> {
        self.pcm = None; // close the old handle first
        let (pcm, granted) = open_pcm(&self.name, &self.req)?;
        if granted.rate != self.granted.rate || granted.period_frames != self.granted.period_frames
        {
            return Err(format!(
                "device came back with different parameters ({}), expected {} Hz / {} frames",
                granted.describe(),
                self.granted.rate,
                self.granted.period_frames
            ));
        }
        if granted.format != self.granted.format {
            self.conv = Converter::new(
                granted.format,
                (granted.buffer_frames * granted.channels) as usize,
            );
            self.frame_bytes = granted.format.bytes_per_sample() * granted.channels as usize;
        }
        self.granted = granted;
        self.pcm = Some(pcm);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (Box A, 2026-10-01): capture with the playback threshold never started and every
    /// read failed with EIO.
    #[test]
    fn capture_starts_on_the_first_read_and_playback_when_the_buffer_is_full() {
        assert_eq!(start_threshold(true, 8192), 1);
        assert_eq!(start_threshold(false, 8192), 8192);
    }
}
