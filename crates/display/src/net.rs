//! The display's network side: a small HTTP/1.1 client (artwork, chain, history) and WebSocket client threads
//! (one per station, one for the library service's `display` topic). Plain `std::net`, like the servers.

use crate::frame::Frame;
use serde_json::Value;
use serve::ws::{self, Message};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// What the network threads tell the main loop.
#[derive(Debug)]
pub enum Event {
    Up(usize),
    Down(usize),
    Json(usize, Value),
    Frame(usize, Box<Frame>),
    Chain(usize, Value),
    History(usize, Value),
    Art(String, image::RgbaImage),
    ArtFailed(String),
    /// The library service's `display` payload: `{view, station, rotate_s, ...}`.
    Library(Value),
    /// The box's meter ballistics, from the library service.
    Meters(Value),
    /// `GET /api/v1/display/bar` of the library service (the top bar of `split`); `Null` when it did not answer.
    Bar(Value),
    /// A station's `GET /api/v1/loudness` (the controller's gain range).
    Loudness(usize, Value),
    LibraryDown,
}

pub fn resolve(addr: &str) -> Option<SocketAddr> {
    addr.to_socket_addrs().ok()?.next()
}

pub fn connect(addr: &str, timeout: Duration) -> std::io::Result<TcpStream> {
    let a = resolve(addr).ok_or_else(|| std::io::Error::other(format!("cannot resolve {addr}")))?;
    let s = TcpStream::connect_timeout(&a, timeout)?;
    s.set_nodelay(true).ok();
    Ok(s)
}

/// `GET path` from `addr`: the status and the body.
pub fn http_get(addr: &str, path: &str, timeout: Duration) -> std::io::Result<(u16, Vec<u8>)> {
    let mut s = connect(addr, timeout)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: */*\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    parse_response(&raw)
}

pub fn parse_response(raw: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    let bad = |m: &str| std::io::Error::other(m.to_string());
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no header end"))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| bad("no status"))?;
    let body = &raw[split + 4..];
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let eol = body[i..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| bad("bad chunk"))?
            + i;
        let n = usize::from_str_radix(
            String::from_utf8_lossy(&body[i..eol])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
        .map_err(|_| bad("bad chunk size"))?;
        if n == 0 {
            break;
        }
        let start = eol + 2;
        if start + n > body.len() {
            return Err(bad("short chunk"));
        }
        out.extend_from_slice(&body[start..start + n]);
        i = start + n + 2;
    }
    Ok((status, out))
}

pub fn http_get_json(addr: &str, path: &str, timeout: Duration) -> Option<Value> {
    let (st, body) = http_get(addr, path, timeout).ok()?;
    if st != 200 {
        return None;
    }
    serde_json::from_slice(&body).ok()
}

/// Open a WebSocket to `addr` at `path`. The read timeout stays set: a silent connection counts as dead.
pub fn ws_connect(
    addr: &str,
    path: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> std::io::Result<TcpStream> {
    let mut s = connect(addr, connect_timeout)?;
    s.set_write_timeout(Some(Duration::from_secs(3)))?;
    s.set_read_timeout(Some(connect_timeout.max(Duration::from_secs(2))))?;
    let mut key = [0u8; 16];
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    for (i, b) in key.iter_mut().enumerate() {
        *b = (t >> ((i % 8) * 8)) as u8 ^ (i as u8).wrapping_mul(37);
    }
    let key = ws::base64(&key);
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )?;
    // read the answer's header, byte by byte so no frame bytes are eaten
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b)?;
        head.push(b[0]);
        if head.len() > 8192 {
            return Err(std::io::Error::other("websocket header too long"));
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    if !text.starts_with("HTTP/1.1 101") {
        return Err(std::io::Error::other(format!(
            "websocket refused: {}",
            text.lines().next().unwrap_or("")
        )));
    }
    if !text.to_ascii_lowercase().contains(&format!(
        "sec-websocket-accept: {}",
        ws::accept_key(&key).to_ascii_lowercase()
    )) {
        return Err(std::io::Error::other("websocket accept key does not match"));
    }
    s.set_read_timeout(Some(read_timeout))?;
    Ok(s)
}

/// A masked client frame.
pub fn ws_send(s: &mut TcpStream, m: &Message) -> std::io::Result<()> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.subsec_nanos());
    ws::write_message(s, m, Some(t.to_le_bytes()))
}

/// Read messages until the connection fails: JSON text and binary frames go to `on_text` and `on_bin`.
pub fn ws_loop(
    s: &mut TcpStream,
    mut on_text: impl FnMut(Value),
    mut on_bin: impl FnMut(&[u8]),
) -> std::io::Error {
    loop {
        match ws::read_message(s, 1 << 20) {
            Ok(Message::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    on_text(v);
                }
            }
            Ok(Message::Binary(b)) => on_bin(&b),
            Ok(Message::Ping(p)) => {
                if let Err(e) = ws_send(s, &Message::Pong(p)) {
                    return e;
                }
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close) => return std::io::Error::other("closed by the station"),
            Err(e) => return e,
        }
    }
}

pub const BACKOFF: [u64; 4] = [1, 2, 3, 5];

/// One station: connect, send the events, reconnect with a short backoff. `stop` ends the thread.
pub fn station_thread(
    idx: usize,
    addr: String,
    tx: Sender<Event>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let mut failures = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let path = "/api/v1/ws?topics=state,now,queue,alerts,asrun,meters";
        match ws_connect(
            &addr,
            path,
            Duration::from_secs(2),
            Duration::from_millis(3000),
        ) {
            Ok(mut s) => {
                failures = 0;
                if tx.send(Event::Up(idx)).is_err() {
                    return;
                }
                let t2 = tx.clone();
                let t3 = tx.clone();
                let err = ws_loop(
                    &mut s,
                    |v| {
                        let _ = t2.send(Event::Json(idx, v));
                    },
                    |b| {
                        if let Some(f) = Frame::parse(b) {
                            let _ = t3.send(Event::Frame(idx, Box::new(f)));
                        }
                    },
                );
                tracing::info!("station {addr}: connection ended: {err}");
                if tx.send(Event::Down(idx)).is_err() {
                    return;
                }
            }
            Err(e) => {
                if failures == 0 {
                    tracing::info!("station {addr}: cannot connect: {e}");
                    let _ = tx.send(Event::Down(idx));
                }
                failures += 1;
            }
        }
        let wait = BACKOFF[failures.saturating_sub(1).min(BACKOFF.len() - 1)];
        for _ in 0..wait * 10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The library service's `display` topic.
pub fn library_thread(
    addr: String,
    tx: Sender<Event>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let mut failures = 0usize;
    while !stop.load(Ordering::Relaxed) {
        match ws_connect(
            &addr,
            "/api/v1/ws?topics=display",
            Duration::from_secs(2),
            Duration::from_secs(40),
        ) {
            Ok(mut s) => {
                failures = 0;
                let t2 = tx.clone();
                let err = ws_loop(
                    &mut s,
                    |v| match v["type"].as_str() {
                        Some("snapshot") => {
                            let _ = t2.send(Event::Library(v["data"]["display"].clone()));
                            if v["data"]["meters"].is_object() {
                                let _ = t2.send(Event::Meters(v["data"]["meters"].clone()));
                            }
                        }
                        Some("display_meters") => {
                            let _ = t2.send(Event::Meters(v["data"].clone()));
                        }
                        Some("display") => {
                            let _ = t2.send(Event::Library(v["data"].clone()));
                        }
                        _ => {}
                    },
                    |_| {},
                );
                tracing::info!("library {addr}: connection ended: {err}");
                let _ = tx.send(Event::LibraryDown);
            }
            Err(e) => {
                if failures == 0 {
                    tracing::info!("library {addr}: cannot connect: {e}");
                    let _ = tx.send(Event::LibraryDown);
                }
                failures += 1;
            }
        }
        let wait = BACKOFF[failures.saturating_sub(1).min(BACKOFF.len() - 1)];
        for _ in 0..wait * 10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// How often the top bar's numbers are asked for (the web display asks as often).
pub const BAR_POLL: Duration = Duration::from_secs(2);

/// The library service's `GET /api/v1/display/bar`, every [`BAR_POLL`]: the CPU use, the network state, the version
/// and which items the bar shows (read fresh from the global file there, so a change on the Settings page shows here
/// within 2 s).
pub fn bar_thread(
    addr: String,
    tx: Sender<Event>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    while !stop.load(Ordering::Relaxed) {
        let v = http_get_json(&addr, "/api/v1/display/bar", Duration::from_secs(2))
            .unwrap_or(Value::Null);
        if tx.send(Event::Bar(v)).is_err() {
            return;
        }
        for _ in 0..BAR_POLL.as_millis() / 100 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Jobs for the HTTP worker of one station.
#[derive(Debug)]
pub enum Job {
    Art(String),
    Chain,
    History,
    Loudness,
}

/// Artwork, chain and history fetches for one station, off the main loop.
pub fn worker_thread(
    idx: usize,
    addr: String,
    jobs: std::sync::mpsc::Receiver<Job>,
    tx: Sender<Event>,
) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Art(url) => {
                let ok = http_get(&addr, &url, Duration::from_secs(5))
                    .ok()
                    .filter(|(s, _)| *s == 200)
                    .and_then(|(_, b)| image::load_from_memory(&b).ok());
                let _ = tx.send(match ok {
                    Some(img) => Event::Art(url, img.to_rgba8()),
                    None => Event::ArtFailed(url),
                });
            }
            Job::Chain => {
                if let Some(v) = http_get_json(&addr, "/api/v1/chain", Duration::from_secs(3)) {
                    let _ = tx.send(Event::Chain(idx, v));
                }
            }
            Job::Loudness => {
                if let Some(v) = http_get_json(&addr, "/api/v1/loudness", Duration::from_secs(3)) {
                    let _ = tx.send(Event::Loudness(idx, v));
                }
            }
            Job::History => {
                if let Some(v) =
                    http_get_json(&addr, "/api/v1/history?limit=6", Duration::from_secs(3))
                {
                    let _ = tx.send(Event::History(idx, v));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked_bodies() {
        let (s, b) = parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi").unwrap();
        assert_eq!((s, b), (200, b"hi".to_vec()));
        let (s, b) = parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n").unwrap();
        assert_eq!((s, b), (200, b"abcde".to_vec()));
        assert!(parse_response(b"garbage").is_err());
    }
}
