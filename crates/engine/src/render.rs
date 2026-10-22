//! The offline tone renderer behind `hotclock render --tone` (architecture 15). It runs the real
//! rt-out thread against the `File` backend, unpaced, so a 10 s render takes milliseconds. M1.4
//! adds the scenario renderer on the same path.

use crate::negotiate::HwRequest;
use crate::output::FileOutput;
use crate::rt::{RtHandle, RtOptions};
use crate::source::ToneSource;
use crate::wav::WavFormat;
use common::config::SampleFormat;
use common::RealClock;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ToneRender {
    pub out: PathBuf,
    pub rate: u32,
    pub seconds: f64,
    pub hz: f64,
    /// RMS level in dBFS of each channel.
    pub level_dbfs: f64,
    pub format: WavFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderReport {
    pub frames: u64,
    pub rate: u32,
    pub bytes: u64,
}

pub fn parse_wav_format(s: &str) -> Option<WavFormat> {
    match s {
        "f32" => Some(WavFormat::F32),
        "s32" => Some(WavFormat::Int(SampleFormat::S32Le)),
        "s24" => Some(WavFormat::Int(SampleFormat::S24_3Le)),
        "s16" => Some(WavFormat::Int(SampleFormat::S16Le)),
        _ => None,
    }
}

pub fn render_tone(r: &ToneRender) -> Result<RenderReport, String> {
    if !r.seconds.is_finite() || r.seconds <= 0.0 {
        return Err(format!("--seconds must be positive, got {}", r.seconds));
    }
    if r.hz <= 0.0 || r.hz >= r.rate as f64 / 2.0 {
        return Err(format!("--tone must be between 0 and {} Hz", r.rate / 2));
    }
    if r.level_dbfs > -3.02 {
        return Err(format!(
            "--level {} dBFS RMS would clip: a full-scale sine is -3.01 dBFS RMS",
            r.level_dbfs
        ));
    }
    let frames = (r.seconds * r.rate as f64).round() as u64;
    let req = HwRequest {
        rate: r.rate,
        ..HwRequest::defaults()
    };
    let clock = Arc::new(RealClock::new());
    let out = FileOutput::create(&r.out, &req, r.format, None).map_err(|e| e.to_string())?;
    let source = ToneSource::new(r.rate, r.hz, r.level_dbfs);
    let opts = RtOptions {
        max_frames: Some(frames),
        thread_name: "rt-out-render".into(),
        ..RtOptions::default()
    };
    let handle =
        RtHandle::start(Box::new(out), Box::new(source), clock, opts).map_err(|e| e.to_string())?;
    let mut out = handle.wait();
    out.finish()?;
    let bytes = std::fs::metadata(&r.out).map_err(|e| e.to_string())?.len();
    Ok(RenderReport {
        frames,
        rate: r.rate,
        bytes,
    })
}
