//! What the rt-out thread renders. M1.1 has one source, a sine tone; M1.2 adds the mixer.

use crate::rt::CommandKind;

pub trait Source: Send {
    /// Fill `out` (interleaved stereo, `out.len() / 2` frames). Must not allocate.
    fn render(&mut self, out: &mut [f32]);
    /// Apply a command that reached the rt thread. Commands the source does not know are ignored.
    fn command(&mut self, _kind: &CommandKind) {}
}

/// A stereo sine. `level_rms_dbfs` is the RMS level of each channel (a full-scale sine is -3.01).
pub struct ToneSource {
    rate: f64,
    hz: f64,
    phase: f64,
    amp: f32,
    gain: f32,
    target_gain: f32,
}

impl ToneSource {
    pub fn new(rate: u32, hz: f64, level_rms_dbfs: f64) -> Self {
        Self {
            rate: rate as f64,
            hz,
            phase: 0.0,
            amp: rms_dbfs_to_peak(level_rms_dbfs),
            gain: 1.0,
            target_gain: 1.0,
        }
    }
}

/// Peak amplitude of a sine with the given RMS level in dBFS.
pub fn rms_dbfs_to_peak(level_rms_dbfs: f64) -> f32 {
    (10f64.powf(level_rms_dbfs / 20.0) * std::f64::consts::SQRT_2) as f32
}

impl Source for ToneSource {
    fn render(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        let step = std::f64::consts::TAU * self.hz / self.rate;
        // Gain changes ramp linearly across the block, so a SetGain command cannot click.
        let g0 = self.gain;
        let dg = if frames > 0 {
            (self.target_gain - self.gain) / frames as f32
        } else {
            0.0
        };
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let s = (self.phase.sin() as f32) * self.amp * (g0 + dg * (i + 1) as f32);
            frame[0] = s;
            frame[1] = s;
            self.phase += step;
            if self.phase >= std::f64::consts::TAU {
                self.phase -= std::f64::consts::TAU;
            }
        }
        self.gain = self.target_gain;
    }

    fn command(&mut self, kind: &CommandKind) {
        match *kind {
            CommandKind::SetGain(g) => self.target_gain = g,
            CommandKind::SetToneHz(h) => self.hz = h as f64,
            CommandKind::Stall { .. } | CommandKind::Mixer(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_rms_matches_request() {
        let mut t = ToneSource::new(48_000, 1000.0, -20.0);
        let mut buf = vec![0.0f32; 2 * 48_000];
        t.render(&mut buf);
        let ms: f64 = buf
            .iter()
            .step_by(2)
            .map(|&s| (s as f64).powi(2))
            .sum::<f64>()
            / 48_000.0;
        let db = 10.0 * ms.log10();
        assert!((db + 20.0).abs() < 0.001, "{db}");
        assert_eq!(buf[0], 0.0);
        assert_eq!(buf[2 * 100], buf[2 * 100 + 1]);
    }

    #[test]
    fn phase_is_continuous_across_blocks() {
        let mut a = ToneSource::new(48_000, 997.0, -10.0);
        let mut b = ToneSource::new(48_000, 997.0, -10.0);
        let mut one = vec![0.0f32; 2 * 3000];
        a.render(&mut one);
        let mut two = vec![0.0f32; 2 * 3000];
        b.render(&mut two[..2 * 1024]);
        b.render(&mut two[2 * 1024..]);
        assert_eq!(one, two);
    }
}
