//! `GET /health` and `GET /metrics` (architecture 12).

use crate::meters::MeterShared;
use crate::station_api::{AudioInfo, Counters, NowView, StationApi};
use serde_json::{json, Value};
use station::failure::Health;
use station::runtime::Station;
use station::state::iso_ms;
use std::fmt::Write as _;

fn lufs(v: i16) -> Value {
    if v == crate::meters::NONE {
        Value::Null
    } else {
        json!(f64::from(v) / 100.0)
    }
}

/// The body of `/health` for a station (12).
pub fn health_json(
    st: &Station,
    api: &StationApi,
    audio: &AudioInfo,
    now: &Option<NowView>,
    h: &Health,
    xruns_1h: u32,
    meter: &MeterShared,
) -> Value {
    let wall = crate::station_api::wall_ms();
    let loud = meter
        .loud
        .lock()
        .map(|l| *l)
        .unwrap_or([crate::meters::NONE; 4]);
    let program = st.sched.st.program.as_ref().map(|p| {
        json!({
            "playlist": st.sched.prog.playlists.get(&p.playlist_id).map(|pl| pl.name.clone()),
            "t0": iso_ms(p.t0_ms), "next_block": null,
        })
    });
    let queue_minutes: f64 = st
        .sched
        .st
        .queue
        .iter()
        .filter_map(|e| st.cat.meta(e.item_id))
        .map(|m| m.effective_ms as f64 / 60_000.0)
        .sum();
    json!({
        "status": h.status.name(),
        "product": api.opts.product,
        "station": api.opts.name,
        "version": api.opts.version,
        "uptime_s": api.started.elapsed().as_secs(),
        "state": if st.fail.safe_mode { "safe" } else { st.transport.name() },
        "mode": if st.assist { "assist" } else { "auto" },
        "operating_mode": h.mode,
        "operating_mode_reason": h.mode_reason,
        "now": now.as_ref().map(|n| json!({
            "title": n.json["title"], "remaining_ms": (n.ends_ms - wall).max(0),
        })),
        "audio": {
            "device": api.opts.device, "open": audio.open, "format": audio.format, "rate": audio.rate,
            "frames_advancing": audio.rt_state == "playing", "xruns_1h": xruns_1h,
            "out_rms_dbfs": (h.out_rms_dbfs * 10.0).round() / 10.0, "silence_s": (h.silence_s * 10.0).round() / 10.0,
        },
        "stream": api.stream.get().map_or_else(|| json!({"enabled": false}), |m| crate::stream_http::health_object(m)),
        "queue": {"depth": h.queue_depth, "minutes": (queue_minutes * 10.0).round() / 10.0},
        "loudness": {
            "enabled": false, "target": null, "window": lufs(loud[2]), "integrated": lufs(loud[3]),
            "gain_db": 0.0, "state": "off",
        },
        "program": program,
        "db": {
            "state_ok": h.db_ok, "backlog": h.db_backlog,
            "last_snapshot": h.snapshot.as_ref().map(|i| iso_ms(i.taken_ms)),
            "snapshot_check": h.snapshot.as_ref().map(|i| i.check.clone()),
        },
        "disk": {"free_gb": h.disk_free_bytes.map(|b| (b as f64 / 1e9 * 10.0).round() / 10.0)},
        "last_event": null,
        "quarantined": h.quarantined,
        "alerts": h.alerts.iter().map(|a| json!({
            "id": a.id, "code": a.code, "severity": a.severity.name(), "message": a.message,
        })).collect::<Vec<_>>(),
    })
}

/// Prometheus text for a station (12).
pub fn station_metrics(
    api: &StationApi,
    c: &Counters,
    meter: &MeterShared,
    audio: &AudioInfo,
) -> String {
    let mut o = String::new();
    let name = &api.opts.name;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut line = |help: &str, typ: &str, metric: &str, labels: &str, v: f64| {
        if seen.insert(metric.to_string()) {
            let _ = writeln!(o, "# HELP {metric} {help}");
            let _ = writeln!(o, "# TYPE {metric} {typ}");
        }
        if labels.is_empty() {
            let _ = writeln!(o, "{metric}{{station=\"{name}\"}} {v}");
        } else {
            let _ = writeln!(o, "{metric}{{station=\"{name}\",{labels}}} {v}");
        }
    };
    line(
        "Frames handed to the sound device.",
        "counter",
        "hotclock_frames_written_total",
        "",
        c.frames_written as f64,
    );
    line(
        "Underruns recovered.",
        "counter",
        "hotclock_xruns_total",
        "",
        c.xruns as f64,
    );
    line(
        "Times the device came back after being lost.",
        "counter",
        "hotclock_device_reopens_total",
        "",
        c.device_reopens as f64,
    );
    line(
        "Seconds of program silence in the current run.",
        "counter",
        "hotclock_silence_seconds_total",
        "",
        c.silence_seconds,
    );
    line(
        "RMS level of the last output block, dBFS.",
        "gauge",
        "hotclock_output_rms_dbfs",
        "",
        c.out_rms_dbfs,
    );
    line(
        "Seconds the safety limiter reduced gain.",
        "counter",
        "hotclock_limiter_active_seconds_total",
        "",
        c.limiter_active_seconds,
    );
    line(
        "Picks that needed a relaxed rule.",
        "counter",
        "hotclock_rule_relaxed_total",
        "",
        c.rule_relaxed as f64,
    );
    for (how, n) in &c.blocks_by_how {
        line(
            "Timed blocks in the block log of the last 72 hours, by outcome.",
            "gauge",
            "hotclock_blocks_logged",
            &format!("how=\"{how}\""),
            *n as f64,
        );
    }
    for (i, le) in ["1", "5", "10", "30", "60", "+Inf"].iter().enumerate() {
        line(
            "Distance of timed blocks that fired from their time (absolute, seconds), last 72 hours.",
            "histogram",
            "hotclock_block_deviation_seconds_bucket",
            &format!("le=\"{le}\""),
            c.block_deviation_buckets[i] as f64,
        );
    }
    line(
        "Sum of the absolute deviations of timed blocks (seconds).",
        "histogram",
        "hotclock_block_deviation_seconds_sum",
        "",
        c.block_deviation_sum_s,
    );
    line(
        "Timed blocks that fired.",
        "histogram",
        "hotclock_block_deviation_seconds_count",
        "",
        c.block_deviation_count as f64,
    );
    if let Some(d) = c.block_last_deviation_s {
        line(
            "Deviation of the last timed block that fired (seconds, negative is early).",
            "gauge",
            "hotclock_block_last_deviation_seconds",
            "",
            d,
        );
    }
    line(
        "Items in the queue.",
        "gauge",
        "hotclock_queue_depth_items",
        "",
        c.queue_depth as f64,
    );
    line(
        "Rows buffered for state.db.",
        "gauge",
        "hotclock_db_write_backlog",
        "",
        c.db_backlog as f64,
    );
    line(
        "Times this station process started since boot.",
        "counter",
        "hotclock_restarts_total",
        "",
        c.restarts as f64,
    );
    line(
        "Meter-tap blocks the mixer dropped because the meter thread was behind.",
        "counter",
        "hotclock_meter_tap_drops_total",
        "",
        c.tap_drops as f64,
    );
    line(
        "Meter frames dropped for slow WebSocket clients.",
        "counter",
        "hotclock_meter_client_drops_total",
        "",
        api.hub.meter_drops() as f64,
    );
    line(
        "Meter frames built.",
        "counter",
        "hotclock_meter_frames_total",
        "",
        meter
            .frames_emitted
            .load(std::sync::atomic::Ordering::Relaxed) as f64,
    );
    line(
        "Items in quarantine.",
        "gauge",
        "hotclock_items_quarantined",
        "",
        c.quarantined as f64,
    );
    line(
        "Connected WebSocket clients.",
        "gauge",
        "hotclock_ws_clients",
        "",
        api.hub.len() as f64,
    );
    let win = meter
        .loud
        .lock()
        .map(|l| l[2])
        .unwrap_or(crate::meters::NONE);
    if win != crate::meters::NONE {
        line(
            "Gated loudness over the measurement window, LUFS.",
            "gauge",
            "hotclock_loudness_window_lufs",
            "",
            f64::from(win) / 100.0,
        );
    }
    line(
        "Loudness controller gain, dB (0: the controller arrives in M6a).",
        "gauge",
        "hotclock_loudness_gain_db",
        "",
        0.0,
    );
    if let Some(b) = c.disk_free_bytes {
        line(
            "Free bytes where state.db lives.",
            "gauge",
            "hotclock_disk_free_bytes",
            "",
            b as f64,
        );
    }
    for (k, v) in &c.items_played {
        line(
            "Items played since the process started.",
            "counter",
            "hotclock_items_played_total",
            &format!("origin=\"{k}\""),
            *v as f64,
        );
    }
    for (k, v) in &c.item_errors {
        line(
            "Item playback failures by kind.",
            "counter",
            "hotclock_item_errors_total",
            &format!("kind=\"{k}\""),
            *v as f64,
        );
    }
    for (k, v) in &c.alerts_raised {
        line(
            "Alerts raised by code.",
            "counter",
            "hotclock_alerts_raised_total",
            &format!("code=\"{k}\""),
            *v as f64,
        );
    }
    let cpu = meter.cpu_ns.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9;
    line(
        "CPU seconds used by the meter thread.",
        "counter",
        "hotclock_meter_thread_cpu_seconds_total",
        "",
        cpu,
    );
    line(
        "Sound output frame rate.",
        "gauge",
        "hotclock_output_rate_hz",
        "",
        f64::from(audio.rate),
    );
    if let Some(m) = api.stream.get() {
        for (help, typ, metric, labels, v) in crate::stream_http::metric_rows(m) {
            line(help, typ, metric, &labels, v);
        }
    }
    o
}
