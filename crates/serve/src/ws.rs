//! WebSocket (RFC 6455): the handshake, frames in both directions, and the [`Hub`] the station uses to
//! fan events out to its clients with a bounded queue per client.

use crate::http::{Reply, Request, Response};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// ---- SHA-1 and base64 (the handshake needs both and nothing else does) --------------------------------------

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, c) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

pub fn accept_key(client_key: &str) -> String {
    base64(&sha1(format!("{client_key}{GUID}").as_bytes()))
}

// ---- frames --------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

fn write_frame(
    w: &mut dyn Write,
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
) -> std::io::Result<()> {
    let mut head = vec![0x80 | opcode];
    let mbit = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => head.push(mbit | n as u8),
        n if n < 65_536 => {
            head.push(mbit | 126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(mbit | 127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    let mut out = head;
    match mask {
        Some(m) => {
            out.extend_from_slice(&m);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    w.write_all(&out)?;
    w.flush()
}

pub fn write_message(w: &mut dyn Write, m: &Message, mask: Option<[u8; 4]>) -> std::io::Result<()> {
    match m {
        Message::Text(t) => write_frame(w, 0x1, t.as_bytes(), mask),
        Message::Binary(b) => write_frame(w, 0x2, b, mask),
        Message::Ping(b) => write_frame(w, 0x9, b, mask),
        Message::Pong(b) => write_frame(w, 0xA, b, mask),
        Message::Close => write_frame(w, 0x8, &[], mask),
    }
}

/// Read one message (control frames included; fragments are joined). `Err` on a closed or broken stream.
pub fn read_message(r: &mut dyn Read, max_len: usize) -> std::io::Result<Message> {
    let mut acc: Vec<u8> = vec![];
    let mut first_op = 0u8;
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let fin = h[0] & 0x80 != 0;
        let op = h[0] & 0x0f;
        let masked = h[1] & 0x80 != 0;
        let mut len = u64::from(h[1] & 0x7f);
        if len == 126 {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            len = u64::from(u16::from_be_bytes(b));
        } else if len == 127 {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            len = u64::from_be_bytes(b);
        }
        if len as usize > max_len || acc.len() + len as usize > max_len {
            return Err(std::io::Error::other("websocket frame too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            r.read_exact(&mut mask)?;
        }
        let mut p = vec![0u8; len as usize];
        r.read_exact(&mut p)?;
        if masked {
            for (i, b) in p.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
        }
        match op {
            0x8 => return Ok(Message::Close),
            0x9 => return Ok(Message::Ping(p)),
            0xA => return Ok(Message::Pong(p)),
            0x0..=0x2 => {
                if op != 0 {
                    first_op = op;
                }
                acc.extend_from_slice(&p);
                if fin {
                    return Ok(if first_op == 1 {
                        Message::Text(String::from_utf8_lossy(&acc).into_owned())
                    } else {
                        Message::Binary(acc)
                    });
                }
            }
            _ => return Err(std::io::Error::other("unknown websocket opcode")),
        }
    }
}

/// The handshake answer for a request that asks to upgrade, or the refusal.
pub fn handshake(req: &Request) -> Result<Response, Response> {
    let is_upgrade = req
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let key = req.header("sec-websocket-key");
    if !is_upgrade || key.is_none() || req.method != "GET" {
        return Err(Response::error(
            400,
            "bad_request",
            "this endpoint is a WebSocket: send an Upgrade request",
        ));
    }
    let mut r = Response::new(101)
        .with_header("upgrade", "websocket")
        .with_header("connection", "Upgrade")
        .with_header("sec-websocket-accept", &accept_key(key.unwrap_or("")));
    r.headers.retain(|(k, _)| k != "content-length");
    Ok(r)
}

/// Write the 101 answer by hand (a `Takeover` handler does its own writing).
pub fn write_handshake(s: &mut TcpStream, req_key: &str) -> std::io::Result<()> {
    let text = format!(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
        accept_key(req_key)
    );
    s.write_all(text.as_bytes())?;
    s.flush()
}

/// A takeover reply that completes the handshake and gives `f` the connection.
pub fn upgrade(req: &Request, f: impl FnOnce(TcpStream, Vec<u8>) + Send + 'static) -> Reply {
    match handshake(req) {
        Err(r) => Reply::Normal(r),
        Ok(_) => {
            let key = req
                .header("sec-websocket-key")
                .unwrap_or_default()
                .to_string();
            Reply::Takeover(Box::new(move |mut s, buffered| {
                if write_handshake(&mut s, &key).is_ok() {
                    f(s, buffered);
                }
            }))
        }
    }
}

// ---- the hub -------------------------------------------------------------------------------------------------

/// What a client is sent.
#[derive(Debug, Clone)]
pub enum Out {
    Text(Arc<str>),
    Binary(Arc<[u8]>),
}

/// One connected client's queue. Meter frames go in a queue of at most 4 and the oldest is dropped
/// (11.3); JSON events go in a longer one, and a client that falls further behind than that is cut off.
pub struct Client {
    pub topics: Mutex<Vec<String>>,
    q: Mutex<ClientQ>,
    cv: Condvar,
    pub json_dropped: AtomicU64,
    pub meter_dropped: AtomicU64,
    /// The number of the last JSON event sent to this client: `seq` is per client, so a client that
    /// subscribes to some topics sees consecutive numbers, and a gap means a message was lost.
    pub seq: AtomicU64,
    /// The client's socket, shut down when the client is cut off, so that a writer blocked on a client that
    /// stopped reading (and the reader waiting on it) return at once instead of after the write timeout.
    sock: Mutex<Option<TcpStream>>,
}

#[derive(Default)]
struct ClientQ {
    items: VecDeque<(bool, Out)>, // (is_meter, message)
    meters: usize,
    closed: bool,
}

/// The client's queue was closed.
#[derive(Debug)]
pub struct Closed;

pub const METER_QUEUE: usize = 4;
pub const JSON_QUEUE: usize = 512;

impl Client {
    pub fn new(topics: Vec<String>) -> Arc<Client> {
        Arc::new(Client {
            topics: Mutex::new(topics),
            q: Mutex::new(ClientQ::default()),
            cv: Condvar::new(),
            json_dropped: AtomicU64::new(0),
            meter_dropped: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            sock: Mutex::new(None),
        })
    }

    /// Give the client its socket (a clone), to be shut down when it is cut off or closed.
    pub fn attach(&self, s: TcpStream) {
        *self.sock.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }

    fn shut(&self) {
        if let Some(s) = self.sock.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    pub fn wants(&self, topic: &str) -> bool {
        self.topics
            .lock()
            .map(|t| t.iter().any(|x| x == topic))
            .unwrap_or(false)
    }

    /// Queue a message. A meter frame past the queue limit replaces the oldest meter frame; a JSON event
    /// past its limit closes the client (it will reconnect and get a snapshot).
    pub fn push(&self, m: Out, meter: bool) {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        if q.closed {
            return;
        }
        if meter {
            if q.meters >= METER_QUEUE {
                if let Some(pos) = q.items.iter().position(|(is_m, _)| *is_m) {
                    q.items.remove(pos);
                    q.meters -= 1;
                    self.meter_dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            q.meters += 1;
        } else if q.items.len() - q.meters >= JSON_QUEUE {
            q.closed = true;
            self.json_dropped.fetch_add(1, Ordering::Relaxed);
            self.cv.notify_all();
            drop(q);
            // `shutdown` does not block: the caller (the station loop, via the hub) is not held up.
            self.shut();
            return;
        }
        q.items.push_back((meter, m));
        self.cv.notify_one();
    }

    pub fn close(&self) {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        q.closed = true;
        self.cv.notify_all();
        drop(q);
        self.shut();
    }

    pub fn is_closed(&self) -> bool {
        self.q.lock().map(|q| q.closed).unwrap_or(true)
    }

    /// The next message, waiting up to `wait`; `Err(Closed)` once closed.
    pub fn pop(&self, wait: Duration) -> Result<Option<Out>, Closed> {
        let mut q = self.q.lock().unwrap_or_else(|e| e.into_inner());
        if q.items.is_empty() && !q.closed {
            q = self
                .cv
                .wait_timeout(q, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if q.closed {
            return Err(Closed);
        }
        Ok(q.items.pop_front().map(|(is_m, o)| {
            if is_m {
                q.meters -= 1;
            }
            o
        }))
    }
}

#[derive(Default)]
pub struct Hub {
    clients: Mutex<Vec<Arc<Client>>>,
    pub total_meter_dropped: AtomicU64,
}

impl Hub {
    pub fn add(&self, c: Arc<Client>) {
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(c);
    }

    pub fn remove(&self, c: &Arc<Client>) {
        let mut g = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|x| !Arc::ptr_eq(x, c));
        self.total_meter_dropped
            .fetch_add(c.meter_dropped.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    pub fn len(&self) -> usize {
        self.clients.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn wants(&self, topic: &str) -> bool {
        self.clients
            .lock()
            .map(|g| g.iter().any(|c| c.wants(topic)))
            .unwrap_or(false)
    }

    pub fn send_json(&self, topic: &str, text: &str) {
        let msg: Arc<str> = Arc::from(text);
        for c in self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if c.wants(topic) {
                c.push(Out::Text(msg.clone()), false);
            }
        }
    }

    /// Send an event to every client that subscribed to `topic`, numbering it in the client's own sequence.
    pub fn send_with(&self, topic: &str, mk: &dyn Fn(u64) -> String) {
        for c in self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if c.wants(topic) {
                let seq = c.seq.fetch_add(1, Ordering::SeqCst) + 1;
                c.push(Out::Text(Arc::from(mk(seq))), false);
            }
        }
    }

    pub fn send_binary(&self, topic: &str, data: &[u8]) {
        let msg: Arc<[u8]> = Arc::from(data);
        for c in self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if c.wants(topic) {
                // meter frames and analyzer frames are lossy: a client that falls behind loses the oldest, it is not cut off
                c.push(
                    Out::Binary(msg.clone()),
                    topic == "meters" || topic.starts_with("eqspec"),
                );
            }
        }
    }

    /// Dropped meter frames over every client that ever connected.
    pub fn meter_drops(&self) -> u64 {
        let live: u64 = self
            .clients
            .lock()
            .map(|g| {
                g.iter()
                    .map(|c| c.meter_dropped.load(Ordering::Relaxed))
                    .sum()
            })
            .unwrap_or(0);
        live + self.total_meter_dropped.load(Ordering::Relaxed)
    }
}

/// A WebSocket client for tests and tools: connects, sends masked frames, reads frames.
pub mod client {
    use super::*;
    use std::io::{BufRead, BufReader};

    pub struct Conn {
        r: BufReader<TcpStream>,
        w: TcpStream,
        n: u32,
    }

    impl Conn {
        pub fn connect(addr: &str, path: &str, extra: &[(&str, &str)]) -> std::io::Result<Conn> {
            Self::connect_with(addr, path, extra, None)
        }

        /// As `connect`, with a small kernel receive buffer (for a client that will stop reading).
        pub fn connect_with(
            addr: &str,
            path: &str,
            extra: &[(&str, &str)],
            recv_buf: Option<usize>,
        ) -> std::io::Result<Conn> {
            let mut s = match recv_buf {
                None => TcpStream::connect(addr)?,
                Some(n) => {
                    // The receive buffer must be set before the connection is made to take effect.
                    use std::net::ToSocketAddrs;
                    let sa = addr
                        .to_socket_addrs()?
                        .next()
                        .ok_or_else(|| std::io::Error::other("no address"))?;
                    let fd = rustix::net::socket(
                        if sa.is_ipv4() {
                            rustix::net::AddressFamily::INET
                        } else {
                            rustix::net::AddressFamily::INET6
                        },
                        rustix::net::SocketType::STREAM,
                        None,
                    )?;
                    rustix::net::sockopt::set_socket_recv_buffer_size(&fd, n)?;
                    rustix::net::connect(&fd, &sa)?;
                    TcpStream::from(fd)
                }
            };
            s.set_nodelay(true)?;
            let mut req = format!(
                "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
            );
            for (k, v) in extra {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            req.push_str("\r\n");
            s.write_all(req.as_bytes())?;
            let mut r = BufReader::new(s.try_clone()?);
            let mut line = String::new();
            r.read_line(&mut line)?;
            if !line.contains(" 101 ") {
                return Err(std::io::Error::other(format!(
                    "handshake refused: {}",
                    line.trim()
                )));
            }
            loop {
                let mut l = String::new();
                r.read_line(&mut l)?;
                if l.trim().is_empty() {
                    break;
                }
            }
            Ok(Conn { r, w: s, n: 1 })
        }

        pub fn send_text(&mut self, t: &str) -> std::io::Result<()> {
            self.n = self.n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            write_message(
                &mut self.w,
                &Message::Text(t.into()),
                Some(self.n.to_be_bytes()),
            )
        }

        pub fn set_timeout(&mut self, d: Option<Duration>) {
            let _ = self.r.get_ref().set_read_timeout(d);
        }

        pub fn recv(&mut self) -> std::io::Result<Message> {
            read_message(&mut self.r, 4 << 20)
        }

        pub fn socket(&self) -> &TcpStream {
            &self.w
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6455_handshake_example() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn sha1_vectors() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(sha1(&[b'a'; 1000])),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
    }

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn frames_round_trip_masked_and_unmasked() {
        for len in [0usize, 5, 125, 126, 300, 70_000] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            for mask in [None, Some([1u8, 2, 3, 4])] {
                let mut buf = vec![];
                write_message(&mut buf, &Message::Binary(payload.clone()), mask).unwrap();
                let m = read_message(&mut &buf[..], 1 << 20).unwrap();
                assert_eq!(m, Message::Binary(payload.clone()), "len {len}");
            }
        }
        let mut buf = vec![];
        write_message(&mut buf, &Message::Text("h\u{e9}llo".into()), None).unwrap();
        assert_eq!(
            read_message(&mut &buf[..], 100).unwrap(),
            Message::Text("h\u{e9}llo".into())
        );
    }

    #[test]
    fn meter_queue_drops_the_oldest_and_never_grows() {
        let c = Client::new(vec!["meters".into()]);
        for i in 0..100u8 {
            c.push(Out::Binary(Arc::from(vec![i])), true);
        }
        assert_eq!(c.meter_dropped.load(Ordering::Relaxed), 96);
        let mut got = vec![];
        while let Ok(Some(Out::Binary(b))) = c.pop(Duration::from_millis(1)) {
            got.push(b[0]);
        }
        assert_eq!(got, vec![96, 97, 98, 99]);
    }

    #[test]
    fn a_json_client_that_falls_far_behind_is_cut_off() {
        let c = Client::new(vec!["now".into()]);
        for _ in 0..JSON_QUEUE + 5 {
            c.push(Out::Text(Arc::from("x")), false);
        }
        assert!(c.is_closed());
    }
}
