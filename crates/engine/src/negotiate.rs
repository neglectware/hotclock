//! Hardware-parameter negotiation (architecture 1.3), separated from the ALSA calls so the
//! decisions are unit-tested on every host. The ALSA backend implements [`HwProbe`] over a real
//! `snd_pcm_hw_params_t`; the tests implement it over a table.
//!
//! Order of decisions: try each requested sample format in order (`S32_LE`, `S24_3LE`,
//! `S16_LE`), at the requested rate first and then, if the card refuses it, at 44,100 Hz; ask for
//! the period size and period count "near" the request; install the parameters; then **read back**
//! what the driver granted and use that.

use common::config::SampleFormat;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwRequest {
    pub rate: u32,
    pub channels: u32,
    pub period_frames: u32,
    pub periods: u32,
    /// Formats in order of preference.
    pub formats: Vec<SampleFormat>,
}

impl HwRequest {
    /// The 1.3 defaults: 48 kHz stereo, 1,024-frame periods, 8 periods.
    pub fn defaults() -> Self {
        Self {
            rate: 48_000,
            channels: 2,
            period_frames: 1024,
            periods: 8,
            formats: SampleFormat::DEFAULT_ORDER.to_vec(),
        }
    }

    /// Rates to try, in order: the requested one, then the other of 48,000 / 44,100.
    pub fn rate_candidates(&self) -> Vec<u32> {
        let mut v = vec![self.rate];
        match self.rate {
            48_000 => v.push(44_100),
            44_100 => v.push(48_000),
            _ => {}
        }
        v
    }
}

/// What the device actually gave us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Granted {
    pub format: SampleFormat,
    pub rate: u32,
    pub channels: u32,
    pub period_frames: u32,
    pub periods: u32,
    pub buffer_frames: u32,
}

impl Granted {
    pub fn describe(&self) -> String {
        format!(
            "{} {} Hz {} ch, period {} frames ({:.1} ms) x {} periods, buffer {} frames ({:.1} ms)",
            self.format,
            self.rate,
            self.channels,
            self.period_frames,
            self.period_frames as f64 * 1000.0 / self.rate as f64,
            self.periods,
            self.buffer_frames,
            self.buffer_frames as f64 * 1000.0 / self.rate as f64,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiateError {
    #[error("the device supports none of the formats {0:?}")]
    NoFormat(Vec<String>),
    #[error("the device supports none of the rates {0:?}")]
    NoRate(Vec<u32>),
    #[error("the device does not support {0} channels")]
    Channels(u32),
    #[error("setting {what} failed: {detail}")]
    Set { what: &'static str, detail: String },
}

/// The questions negotiation asks a device. Every method may fail with a description.
pub trait HwProbe {
    fn accepts_channels(&mut self, channels: u32) -> bool;
    fn accepts_format(&mut self, format: SampleFormat) -> bool;
    fn accepts_rate(&mut self, rate: u32) -> bool;
    fn set_channels(&mut self, channels: u32) -> Result<(), String>;
    fn set_format(&mut self, format: SampleFormat) -> Result<(), String>;
    fn set_rate(&mut self, rate: u32) -> Result<(), String>;
    /// Returns the value the driver picked.
    fn set_period_frames_near(&mut self, frames: u32) -> Result<u32, String>;
    fn set_periods_near(&mut self, periods: u32) -> Result<u32, String>;
    /// Install the parameters and read back `(period_frames, periods, buffer_frames)`.
    fn commit(&mut self) -> Result<(u32, u32, u32), String>;
}

pub fn negotiate<P: HwProbe>(probe: &mut P, req: &HwRequest) -> Result<Granted, NegotiateError> {
    let set = |what: &'static str, r: Result<(), String>| {
        r.map_err(|detail| NegotiateError::Set { what, detail })
    };

    if !probe.accepts_channels(req.channels) {
        return Err(NegotiateError::Channels(req.channels));
    }
    set("channels", probe.set_channels(req.channels))?;

    let format = req
        .formats
        .iter()
        .copied()
        .find(|&f| probe.accepts_format(f))
        .ok_or_else(|| {
            NegotiateError::NoFormat(req.formats.iter().map(|f| f.name().to_string()).collect())
        })?;
    set("format", probe.set_format(format))?;

    let candidates = req.rate_candidates();
    let rate = candidates
        .iter()
        .copied()
        .find(|&r| probe.accepts_rate(r))
        .ok_or_else(|| NegotiateError::NoRate(candidates.clone()))?;
    set("rate", probe.set_rate(rate))?;

    probe
        .set_period_frames_near(req.period_frames)
        .map_err(|detail| NegotiateError::Set {
            what: "period size",
            detail,
        })?;
    probe
        .set_periods_near(req.periods)
        .map_err(|detail| NegotiateError::Set {
            what: "period count",
            detail,
        })?;

    let (period_frames, periods, buffer_frames) =
        probe.commit().map_err(|detail| NegotiateError::Set {
            what: "hw params (commit)",
            detail,
        })?;

    Ok(Granted {
        format,
        rate,
        channels: req.channels,
        period_frames,
        periods,
        buffer_frames,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A table-driven fake device.
    pub struct FakeDevice {
        pub formats: Vec<SampleFormat>,
        pub rates: Vec<u32>,
        pub channels: Vec<u32>,
        /// The device rounds the period size to a multiple of this.
        pub period_quantum: u32,
        pub max_buffer: u32,
        pub chosen_period: u32,
        pub chosen_periods: u32,
        pub log: Vec<String>,
    }

    impl FakeDevice {
        pub fn new(formats: &[SampleFormat], rates: &[u32]) -> Self {
            Self {
                formats: formats.to_vec(),
                rates: rates.to_vec(),
                channels: vec![2],
                period_quantum: 1,
                max_buffer: 1 << 20,
                chosen_period: 0,
                chosen_periods: 0,
                log: vec![],
            }
        }
    }

    impl HwProbe for FakeDevice {
        fn accepts_channels(&mut self, c: u32) -> bool {
            self.channels.contains(&c)
        }
        fn accepts_format(&mut self, f: SampleFormat) -> bool {
            self.formats.contains(&f)
        }
        fn accepts_rate(&mut self, r: u32) -> bool {
            self.rates.contains(&r)
        }
        fn set_channels(&mut self, c: u32) -> Result<(), String> {
            self.log.push(format!("channels {c}"));
            Ok(())
        }
        fn set_format(&mut self, f: SampleFormat) -> Result<(), String> {
            self.log.push(format!("format {f}"));
            Ok(())
        }
        fn set_rate(&mut self, r: u32) -> Result<(), String> {
            self.log.push(format!("rate {r}"));
            Ok(())
        }
        fn set_period_frames_near(&mut self, frames: u32) -> Result<u32, String> {
            let q = self.period_quantum;
            self.chosen_period = frames.div_ceil(q) * q;
            Ok(self.chosen_period)
        }
        fn set_periods_near(&mut self, periods: u32) -> Result<u32, String> {
            let mut p = periods;
            while self.chosen_period * p > self.max_buffer && p > 2 {
                p -= 1;
            }
            self.chosen_periods = p;
            Ok(p)
        }
        fn commit(&mut self) -> Result<(u32, u32, u32), String> {
            Ok((
                self.chosen_period,
                self.chosen_periods,
                self.chosen_period * self.chosen_periods,
            ))
        }
    }

    use SampleFormat::*;

    #[test]
    fn prefers_s32_at_48k() {
        let mut d = FakeDevice::new(&[S16Le, S24_3Le, S32Le], &[44_100, 48_000]);
        let g = negotiate(&mut d, &HwRequest::defaults()).unwrap();
        assert_eq!(g.format, S32Le);
        assert_eq!(g.rate, 48_000);
        assert_eq!(
            (g.period_frames, g.periods, g.buffer_frames),
            (1024, 8, 8192)
        );
    }

    #[test]
    fn falls_back_to_24_then_16_bit() {
        let mut d = FakeDevice::new(&[S16Le, S24_3Le], &[48_000]);
        assert_eq!(
            negotiate(&mut d, &HwRequest::defaults()).unwrap().format,
            S24_3Le
        );
        let mut d = FakeDevice::new(&[S16Le], &[48_000]);
        assert_eq!(
            negotiate(&mut d, &HwRequest::defaults()).unwrap().format,
            S16Le
        );
    }

    #[test]
    fn falls_back_to_44100_when_48000_refused() {
        let mut d = FakeDevice::new(&[S16Le], &[44_100]);
        let g = negotiate(&mut d, &HwRequest::defaults()).unwrap();
        assert_eq!(g.rate, 44_100);
        assert!(d.log.contains(&"rate 44100".to_string()));
    }

    #[test]
    fn requested_44100_prefers_44100_then_48000() {
        let mut req = HwRequest::defaults();
        req.rate = 44_100;
        let mut d = FakeDevice::new(&[S16Le], &[44_100, 48_000]);
        assert_eq!(negotiate(&mut d, &req).unwrap().rate, 44_100);
        let mut d = FakeDevice::new(&[S16Le], &[48_000]);
        assert_eq!(negotiate(&mut d, &req).unwrap().rate, 48_000);
    }

    #[test]
    fn no_common_rate_is_an_error_naming_the_candidates() {
        let mut d = FakeDevice::new(&[S16Le], &[96_000]);
        let e = negotiate(&mut d, &HwRequest::defaults()).unwrap_err();
        assert_eq!(e, NegotiateError::NoRate(vec![48_000, 44_100]));
        assert!(e.to_string().contains("48000"));
    }

    #[test]
    fn no_common_format_is_an_error() {
        let mut d = FakeDevice::new(&[], &[48_000]);
        assert!(matches!(
            negotiate(&mut d, &HwRequest::defaults()),
            Err(NegotiateError::NoFormat(_))
        ));
    }

    #[test]
    fn wrong_channel_count_is_an_error() {
        let mut d = FakeDevice::new(&[S16Le], &[48_000]);
        d.channels = vec![1];
        assert_eq!(
            negotiate(&mut d, &HwRequest::defaults()),
            Err(NegotiateError::Channels(2))
        );
    }

    #[test]
    fn uses_what_the_driver_granted_not_what_was_asked() {
        let mut d = FakeDevice::new(&[S32Le], &[48_000]);
        d.period_quantum = 480; // a USB device that only does 10 ms multiples
        d.max_buffer = 4096;
        let g = negotiate(&mut d, &HwRequest::defaults()).unwrap();
        assert_eq!(g.period_frames, 1440);
        assert_eq!(g.periods, 2);
        assert_eq!(g.buffer_frames, 2880);
    }

    #[test]
    fn describe_is_readable() {
        let g = Granted {
            format: S32Le,
            rate: 48_000,
            channels: 2,
            period_frames: 1024,
            periods: 8,
            buffer_frames: 8192,
        };
        let s = g.describe();
        assert!(
            s.contains("S32_LE") && s.contains("48000 Hz") && s.contains("21.3 ms"),
            "{s}"
        );
    }
}
