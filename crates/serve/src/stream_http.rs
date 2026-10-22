//! The stream mount's HTTP side (architecture 1.3a): the listener loop behind `GET /stream/<mount>.ogg`,
//! and the JSON, health and metric views of the mount.
//!
//! The mount is read-only: `GET` (and `HEAD`) only, nothing the client sends is read, and the answer is
//! an unbounded body in the style of Icecast (HTTP/1.0, no `Content-Length`, ends when the connection
//! does). Admission, the burst, the lag rule and `max_listeners` live in [`stream::mount`]; this file only
//! moves pages to a socket.

use crate::http::{Reply, Response};
use serde_json::{json, Value};
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;
use stream::{CloseReason, Mount, Next};

/// Remove anything that could end a header line.
fn header_safe(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// The answer to `GET /stream/<file>`: a takeover that streams, `503` when the mount is full, `404`
/// for any other name.
pub fn listener_reply(
    mount: &Arc<Mount>,
    file: &str,
    product: &str,
    remote: &str,
    head_only: bool,
) -> Reply {
    if file != format!("{}.ogg", mount.cfg.name) {
        return Reply::Normal(Response::error(404, "not_found", "no such mount"));
    }
    if head_only {
        // The headers only; nobody is admitted.
        return Reply::Takeover(Box::new({
            let head = response_head(mount, product);
            move |mut s, _| {
                let _ = s.write_all(head.as_bytes());
            }
        }));
    }
    // The socket clone lets `POST /stream/disconnect` end a listener blocked in a write.
    let listener = match mount.join(remote, None) {
        Some(l) => l,
        None => {
            return Reply::Normal(
                Response::error(503, "busy", "the stream has as many listeners as it allows")
                    .with_header("retry-after", "5"),
            )
        }
    };
    let m = mount.clone();
    let head = response_head(mount, product);
    Reply::Takeover(Box::new(move |sock, _buffered| {
        serve_listener(sock, listener, &m, &head);
    }))
}

fn response_head(m: &Mount, product: &str) -> String {
    format!(
        "HTTP/1.0 200 OK\r\nContent-Type: audio/ogg\r\nCache-Control: no-store\r\nPragma: no-cache\r\n\
         X-Content-Type-Options: nosniff\r\nicy-name: {}\r\nicy-pub: 0\r\nServer: {}\r\n\r\n",
        header_safe(&m.cfg.display_name),
        header_safe(product)
    )
}

fn serve_listener(mut sock: TcpStream, mut l: stream::Listener, m: &Arc<Mount>, head: &str) {
    let _ = sock.set_nodelay(true);
    // Little kernel buffering behind a client, so that one that stops reading fills it within a second or
    // two and the write timeout (the lag limit) takes over.
    let _ = rustix::net::sockopt::set_socket_send_buffer_size(&sock, 64 * 1024);
    let _ = sock.set_write_timeout(Some(Duration::from_secs(u64::from(
        m.cfg.listener_lag_s.max(1),
    ))));
    if let Ok(clone) = sock.try_clone() {
        m.attach_socket(l.id, clone);
    }
    let headers = l.headers();
    if sock.write_all(head.as_bytes()).is_err() || sock.write_all(&headers).is_err() {
        l.end(CloseReason::Gone);
        return;
    }
    l.sent(head.len() + headers.len());
    loop {
        match l.next(Duration::from_millis(500)) {
            Next::Page(p) => match sock.write_all(&p.bytes) {
                Ok(()) => l.sent(p.bytes.len()),
                Err(e) => {
                    let slow = matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    );
                    l.end(if slow {
                        CloseReason::Slow
                    } else {
                        CloseReason::Gone
                    });
                    break;
                }
            },
            Next::Idle => {}
            Next::Closed(_) => break,
        }
    }
    let _ = sock.shutdown(std::net::Shutdown::Both);
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// `GET /api/v1/stream`.
pub fn stream_json(m: &Mount) -> Value {
    let st = &m.stats;
    let listeners: Vec<Value> = m
        .listeners()
        .iter()
        .map(|l| {
            json!({
                "id": l.id, "remote": l.remote, "connected_s": r1(l.connected_s),
                "bytes": l.bytes, "lag_ms": l.lag_ms,
            })
        })
        .collect();
    let tap = m.tap_stats();
    let up = m.uptime_s().max(0.001);
    let cpu = st.encode_ns.load(Relaxed) as f64 / 1e9 / up * 100.0;
    json!({
        "enabled": true,
        "mount": m.cfg.name,
        "path": format!("/stream/{}.ogg", m.cfg.name),
        "codec": m.cfg.codec,
        "listeners": listeners.len(),
        "max_listeners": m.cfg.max_listeners,
        "listener_lag_s": m.cfg.listener_lag_s,
        "burst_ms": m.cfg.burst_ms,
        "listener_list": listeners,
        "peak_listeners": st.peak_listeners.load(Relaxed),
        "connects": st.connects.load(Relaxed),
        "rejected": st.rejected.load(Relaxed),
        "disconnects": {
            "slow": st.closed_slow.load(Relaxed), "gone": st.closed_gone.load(Relaxed),
            "kicked": st.closed_kicked.load(Relaxed), "encoder_restart": st.closed_restart.load(Relaxed),
        },
        "no_listener_s": r1(m.no_listener_s()),
        "expect_listener": m.cfg.expect_listener,
        "encoder": {
            "state": m.encoder_state().name(),
            "lag_ms": st.encoder_lag_ms.load(Relaxed),
            "lag_max_ms": st.encoder_lag_max_ms.load(Relaxed),
            "restarts": st.encoder_restarts.load(Relaxed),
            "cpu_pct": (cpu * 100.0).round() / 100.0,
            "max_encode_ms": r1(st.max_encode_ns.load(Relaxed) as f64 / 1e6),
            "frames": st.frames_encoded.load(Relaxed),
            "pages": st.pages.load(Relaxed),
            "bytes": st.bytes_encoded.load(Relaxed),
        },
        "tap": {
            "dropped_periods": tap.map_or(0, |t| t.dropped_periods.load(Relaxed)),
            "dropped_frames": tap.map_or(0, |t| t.dropped_frames.load(Relaxed)),
        },
        "gaps": st.gaps.load(Relaxed),
        "gap_frames": st.gap_frames.load(Relaxed),
        "bytes_sent": st.bytes_sent.load(Relaxed),
        "buffered_ms": m.buffered_ms(),
    })
}

/// The `stream` object of `/health`.
pub fn health_object(m: &Mount) -> Value {
    let st = &m.stats;
    let tap = m.tap_stats();
    json!({
        "enabled": true, "mount": m.cfg.name, "codec": m.cfg.codec,
        "listeners": m.listener_count(), "max_listeners": m.cfg.max_listeners,
        "no_listener_s": r1(m.no_listener_s()),
        "encoder": m.encoder_state().name(),
        "encoder_lag_ms": st.encoder_lag_ms.load(Relaxed),
        "tap_drops": tap.map_or(0, |t| t.dropped_periods.load(Relaxed)),
        "dropped_frames": tap.map_or(0, |t| t.dropped_frames.load(Relaxed)),
        "gaps": st.gaps.load(Relaxed),
    })
}

/// Prometheus lines for the mount, as `(help, type, name, labels, value)`.
pub fn metric_rows(m: &Mount) -> Vec<(&'static str, &'static str, &'static str, String, f64)> {
    let st = &m.stats;
    let tap = m.tap_stats();
    let mut v: Vec<(&'static str, &'static str, &'static str, String, f64)> = vec![
        (
            "Listeners connected to the stream.",
            "gauge",
            "hotclock_stream_listeners",
            String::new(),
            m.listener_count() as f64,
        ),
        (
            "Most listeners connected at once.",
            "gauge",
            "hotclock_stream_listeners_peak",
            String::new(),
            st.peak_listeners.load(Relaxed) as f64,
        ),
        (
            "Listener connections accepted.",
            "counter",
            "hotclock_stream_connects_total",
            String::new(),
            st.connects.load(Relaxed) as f64,
        ),
        (
            "Listener connections refused because max_listeners were connected.",
            "counter",
            "hotclock_stream_rejected_total",
            String::new(),
            st.rejected.load(Relaxed) as f64,
        ),
        (
            "Seconds with no listener connected.",
            "gauge",
            "hotclock_stream_no_listener_seconds",
            String::new(),
            m.no_listener_s(),
        ),
        (
            "Audio queued for the encoder, in seconds.",
            "gauge",
            "hotclock_stream_encoder_lag_seconds",
            String::new(),
            st.encoder_lag_ms.load(Relaxed) as f64 / 1000.0,
        ),
        (
            "Periods rt-out dropped because the encoder was behind.",
            "counter",
            "hotclock_stream_tap_drops_total",
            String::new(),
            tap.map_or(0, |t| t.dropped_periods.load(Relaxed)) as f64,
        ),
        (
            "Frames in those periods.",
            "counter",
            "hotclock_stream_dropped_frames_total",
            String::new(),
            tap.map_or(0, |t| t.dropped_frames.load(Relaxed)) as f64,
        ),
        (
            "Gaps the encoder filled with silence.",
            "counter",
            "hotclock_stream_gaps_total",
            String::new(),
            st.gaps.load(Relaxed) as f64,
        ),
        (
            "Frames of silence the encoder filled gaps with.",
            "counter",
            "hotclock_stream_gap_frames_total",
            String::new(),
            st.gap_frames.load(Relaxed) as f64,
        ),
        (
            "Times the encoder restarted after a failure.",
            "counter",
            "hotclock_stream_encoder_restarts_total",
            String::new(),
            st.encoder_restarts.load(Relaxed) as f64,
        ),
        (
            "Frames encoded onto the stream's timeline.",
            "counter",
            "hotclock_stream_encoded_frames_total",
            String::new(),
            st.frames_encoded.load(Relaxed) as f64,
        ),
        (
            "Seconds the encoder spent encoding.",
            "counter",
            "hotclock_stream_encoder_busy_seconds_total",
            String::new(),
            st.encode_ns.load(Relaxed) as f64 / 1e9,
        ),
        (
            "Bytes sent to listeners.",
            "counter",
            "hotclock_stream_bytes_sent_total",
            String::new(),
            st.bytes_sent.load(Relaxed) as f64,
        ),
    ];
    for (reason, n) in [
        ("slow", st.closed_slow.load(Relaxed)),
        ("gone", st.closed_gone.load(Relaxed)),
        ("kicked", st.closed_kicked.load(Relaxed)),
        ("encoder_restart", st.closed_restart.load(Relaxed)),
    ] {
        v.push((
            "Listeners disconnected, by reason.",
            "counter",
            "hotclock_stream_disconnects_total",
            format!("reason=\"{reason}\""),
            n as f64,
        ));
    }
    v
}
