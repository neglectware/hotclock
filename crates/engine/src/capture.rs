//! Sound-card capture for `hotclock loopcheck` (M1.8): the input side of the loop that measures the
//! output card. Linux only (ALSA); elsewhere `open` says so. Not used on air, so it may allocate and
//! block; it never runs in a station process.

use crate::negotiate::{Granted, HwRequest};
use common::config::SampleFormat;

/// What one `read` returned.
#[derive(Debug, PartialEq)]
pub enum ReadOutcome {
    /// This many frames were appended to the buffer (interleaved, `channels` per frame).
    Frames(usize),
    /// The card's buffer overran (`-EPIPE`): frames were lost. The stream was restarted.
    Overrun,
    /// The device is gone (`-ENODEV`, `-EIO`).
    Lost(String),
}

/// Convert little-endian PCM bytes to f32 in [-1, 1).
pub fn bytes_to_f32(fmt: SampleFormat, bytes: &[u8], out: &mut Vec<f32>) {
    match fmt {
        SampleFormat::S32Le => out.extend(
            bytes
                .chunks_exact(4)
                .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0),
        ),
        SampleFormat::S24_3Le => out.extend(bytes.chunks_exact(3).map(|b| {
            let v = i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8;
            v as f32 / 8_388_608.0
        })),
        SampleFormat::S16Le => out.extend(
            bytes
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0),
        ),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::fault::{classify_errno, Fault};
    use alsa::pcm::PCM;
    use alsa::Direction;

    pub struct Capture {
        pcm: PCM,
        granted: Granted,
        buf: Vec<u8>,
        frame_bytes: usize,
    }

    impl Capture {
        pub fn open(name: &str, req: &HwRequest) -> Result<Self, String> {
            let (pcm, granted) = crate::alsa_out::open_pcm_dir(name, req, Direction::Capture)?;
            let frame_bytes = granted.format.bytes_per_sample() * granted.channels as usize;
            let buf = vec![0u8; granted.period_frames as usize * frame_bytes];
            Ok(Self {
                pcm,
                granted,
                buf,
                frame_bytes,
            })
        }

        pub fn granted(&self) -> &Granted {
            &self.granted
        }

        /// Read one period (blocking), appending to `out`.
        pub fn read(&mut self, out: &mut Vec<f32>) -> ReadOutcome {
            let io = self.pcm.io_bytes();
            loop {
                match io.readi(&mut self.buf) {
                    Ok(frames) => {
                        bytes_to_f32(
                            self.granted.format,
                            &self.buf[..frames * self.frame_bytes],
                            out,
                        );
                        return ReadOutcome::Frames(frames);
                    }
                    Err(e) => match classify_errno(e.errno()) {
                        Fault::Retry => continue,
                        Fault::Xrun => {
                            let _ = self.pcm.prepare();
                            return ReadOutcome::Overrun;
                        }
                        Fault::Suspended => {
                            let _ = self.pcm.resume();
                            let _ = self.pcm.prepare();
                            return ReadOutcome::Overrun;
                        }
                        Fault::Lost => return ReadOutcome::Lost(e.to_string()),
                        Fault::Other => {
                            if self.pcm.prepare().is_err() {
                                return ReadOutcome::Lost(e.to_string());
                            }
                            return ReadOutcome::Overrun;
                        }
                    },
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub struct Capture {
        granted: Granted,
    }

    impl Capture {
        pub fn open(name: &str, _req: &HwRequest) -> Result<Self, String> {
            Err(format!(
                "cannot capture from `{name}`: sound-card capture needs ALSA (Linux)"
            ))
        }

        pub fn granted(&self) -> &Granted {
            &self.granted
        }

        pub fn read(&mut self, _out: &mut Vec<f32>) -> ReadOutcome {
            ReadOutcome::Lost("no capture on this platform".into())
        }
    }
}

pub use imp::Capture;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_convert() {
        let mut o = vec![];
        bytes_to_f32(SampleFormat::S16Le, &[0x00, 0x40, 0x00, 0xc0], &mut o);
        assert_eq!(o, vec![0.5, -0.5]);
        o.clear();
        bytes_to_f32(SampleFormat::S32Le, &[0, 0, 0, 0x40], &mut o);
        assert_eq!(o, vec![0.5]);
        o.clear();
        bytes_to_f32(SampleFormat::S24_3Le, &[0, 0, 0x40, 0, 0, 0xc0], &mut o);
        assert_eq!(o, vec![0.5, -0.5]);
    }
}
