//! The equalizer analyzer's thread: while a WebSocket client is subscribed to `eqspec_in` or `eqspec_out`, it
//! switches the chain's taps on, runs the FFT here (never on the audio thread) about 12 times a second and sends
//! the frames to the clients that asked. With nobody subscribed the taps are off and the thread only looks at the
//! hub every 150 ms. See `engine::chain::spectrum` for the analysis and the wire format.

use crate::station_api::StationApi;
use engine::chain::spectrum::{SpectrumWorker, TAP_IN, TAP_OUT};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The WebSocket topic of a tap (0 in, 1 out).
pub fn topic(tap: u8) -> &'static str {
    if tap == 0 {
        "eqspec_in"
    } else {
        "eqspec_out"
    }
}

/// Which taps the clients of `api` want now.
pub fn wanted(api: &StationApi) -> u32 {
    (if api.hub.wants("eqspec_in") {
        TAP_IN
    } else {
        0
    }) | (if api.hub.wants("eqspec_out") {
        TAP_OUT
    } else {
        0
    })
}

pub fn spawn(api: Arc<StationApi>) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("eq-analyzer".into())
        .spawn(move || {
            let mut worker: Option<SpectrumWorker> = None;
            while !api.meter.stop.load(Ordering::Relaxed) {
                let want = wanted(&api);
                if worker.is_none() {
                    // The chain may start a little after the thread does.
                    if let Some(c) = api.chain.get() {
                        worker = Some(SpectrumWorker::new(c.ctl.meters.clone(), c.ctl.rate()));
                    }
                }
                let Some(w) = worker.as_mut() else {
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                };
                for (tap, frame) in w.tick(want, Instant::now()) {
                    api.hub.send_binary(topic(tap), &frame);
                }
                std::thread::sleep(Duration::from_millis(if want == 0 { 150 } else { 80 }));
            }
        })
}
