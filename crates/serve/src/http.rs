//! A small HTTP/1.1 server on `std::net`: one thread per connection, keep-alive, bodies read through
//! the connection (an upload is streamed to disk by its handler, never held in memory), and a way for a
//! handler to take the socket over (WebSocket, and the library's WebSocket proxy).
//!
//! Why hand-written: the design asks for a light, dependency-minimal service (13, M1.7), and the whole
//! surface is a few dozen JSON routes, range reads of stored audio, uploads and a WebSocket. The security
//! rules of architecture 13 (Host allowlist, `Content-Type`, CORS) run here, before any handler, so no
//! route can forget them.

use crate::security::{Decision, Security};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Longest request line or header line accepted.
const MAX_LINE: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;
/// The request line and headers together may be this long.
const MAX_HEAD: usize = 64 * 1024;
/// Once the first byte of a request has arrived, its request line and headers must be complete within this
/// time: a client that drips a header a byte at a time (slowloris) holds a connection for no longer.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(20);
/// The most simultaneous connections; the next one is answered 503.
pub const MAX_CONNECTIONS: usize = 128;
/// A JSON body may be this long; uploads are streamed and have their own limit.
pub const MAX_JSON_BODY: u64 = 4 * 1024 * 1024;
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Request<'a> {
    pub method: String,
    /// The path, percent-decoded.
    pub path: String,
    /// The raw query string (after `?`), not decoded.
    pub raw_query: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub peer: SocketAddr,
    /// The bytes of the request line and headers as received (the proxy replays them).
    pub head_bytes: Vec<u8>,
    body: std::io::Take<&'a mut BufReader<TcpStream>>,
    pub content_length: u64,
}

impl Request<'_> {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The decoded query parameters, in order.
    pub fn query(&self) -> Vec<(String, String)> {
        parse_query(&self.raw_query)
    }

    pub fn query_get(&self, key: &str) -> Option<String> {
        self.query()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// The body as bytes, up to `max` (`413` when longer).
    pub fn body_bytes(&mut self, max: u64) -> Result<Vec<u8>, Response> {
        if self.content_length > max {
            return Err(Response::error(
                413,
                "too_large",
                "the request body is too large",
            ));
        }
        let mut v = Vec::with_capacity(self.content_length.min(1 << 20) as usize);
        self.body
            .read_to_end(&mut v)
            .map_err(|e| Response::error(400, "bad_request", &format!("reading the body: {e}")))?;
        Ok(v)
    }

    /// The body as a JSON value (an empty body is `{}`).
    pub fn json(&mut self) -> Result<Value, Response> {
        let b = self.body_bytes(MAX_JSON_BODY)?;
        if b.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_slice(&b)
            .map_err(|e| Response::error(400, "bad_json", &format!("the body is not JSON: {e}")))
    }

    /// The body as a stream (an upload).
    pub fn body_reader(&mut self) -> &mut dyn Read {
        &mut self.body
    }
}

pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k, true), percent_decode(v, true)),
            None => (percent_decode(p, true), String::new()),
        })
        .collect()
}

pub fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `HEAD` and 304 answers carry the headers of the full answer and no body.
    pub head_only: bool,
}

pub fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        507 => "Insufficient Storage",
        _ => "Unknown",
    }
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
            head_only: false,
        }
    }

    pub fn json(status: u16, v: &Value) -> Self {
        let mut r = Self::new(status);
        r.body = serde_json::to_vec(v).unwrap_or_default();
        r.headers
            .push(("content-type".into(), "application/json".into()));
        r.headers.push(("cache-control".into(), "no-store".into()));
        r
    }

    /// `{"error": {"code": ..., "message": ...}}` with a matching status (7.1).
    pub fn error(status: u16, code: &str, message: &str) -> Self {
        Self::json(
            status,
            &serde_json::json!({"error": {"code": code, "message": message}}),
        )
    }

    pub fn text(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        let mut r = Self::new(status);
        r.body = body.into();
        r.headers.push(("content-type".into(), content_type.into()));
        r
    }

    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_ascii_lowercase(), v.into()));
        self
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What a handler returns: an answer, or the socket itself.
pub enum Reply {
    Normal(Response),
    /// The handler takes the connection over: the function gets the stream and any bytes already
    /// buffered after the request. The server writes nothing first.
    Takeover(Box<dyn FnOnce(TcpStream, Vec<u8>) + Send>),
}

impl From<Response> for Reply {
    fn from(r: Response) -> Self {
        Reply::Normal(r)
    }
}

pub trait Handler: Send + Sync + 'static {
    fn handle(&self, req: &mut Request) -> Reply;
}

pub struct Server {
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    conns: Arc<AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Bind `listen` and serve `handler` on a background thread.
    pub fn start(
        listen: &str,
        handler: Arc<dyn Handler>,
        security: Arc<Security>,
    ) -> std::io::Result<Server> {
        Self::start_with(listen, handler, security, HEAD_TIMEOUT)
    }

    /// As [`Server::start`], with the request-head time limit given (tests use a short one).
    pub fn start_with(
        listen: &str,
        handler: Arc<dyn Handler>,
        security: Arc<Security>,
        head_timeout: Duration,
    ) -> std::io::Result<Server> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let conns = Arc::new(AtomicUsize::new(0));
        let (s2, c2) = (stop.clone(), conns.clone());
        let thread = std::thread::Builder::new()
            .name("http-accept".into())
            .spawn(move || accept_loop(listener, handler, security, s2, c2, head_timeout))?;
        Ok(Server {
            addr,
            stop,
            conns,
            thread: Some(thread),
        })
    }

    pub fn connections(&self) -> usize {
        self.conns.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(
    l: TcpListener,
    h: Arc<dyn Handler>,
    sec: Arc<Security>,
    stop: Arc<AtomicBool>,
    conns: Arc<AtomicUsize>,
    head_timeout: Duration,
) {
    while !stop.load(Ordering::SeqCst) {
        match l.accept() {
            Ok((s, peer)) => {
                let _ = s.set_nonblocking(false);
                if conns.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut s = s;
                    let r = Response::error(503, "busy", "too many connections");
                    let _ = write_response(&mut s, &r, false, false);
                    continue;
                }
                conns.fetch_add(1, Ordering::SeqCst);
                let (h, sec, c) = (h.clone(), sec.clone(), conns.clone());
                let spawned =
                    std::thread::Builder::new()
                        .name("http-conn".into())
                        .spawn(move || {
                            serve_connection(s, peer, &*h, &sec, head_timeout);
                            c.fetch_sub(1, Ordering::SeqCst);
                        });
                if spawned.is_err() {
                    conns.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Read one line (with its `\n`), at most [`MAX_LINE`] bytes, finishing before `deadline`.
fn read_line(
    r: &mut BufReader<TcpStream>,
    buf: &mut Vec<u8>,
    deadline: Instant,
) -> std::io::Result<usize> {
    buf.clear();
    let mut total = 0;
    loop {
        if r.buffer().is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request head too slow",
                ));
            }
            r.get_ref().set_read_timeout(Some(left))?;
        }
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(total);
        }
        let (take, done) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (avail.len(), false),
        };
        buf.extend_from_slice(&avail[..take]);
        r.consume(take);
        total += take;
        if buf.len() > MAX_LINE {
            return Err(std::io::Error::other("line too long"));
        }
        if done {
            return Ok(total);
        }
    }
}

fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    h: &dyn Handler,
    sec: &Security,
    head_timeout: Duration,
) {
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::with_capacity(16 * 1024, stream);
    loop {
        // Wait (idle, keep-alive) for the first byte of the next request, then give the whole head
        // HEAD_TIMEOUT.
        if reader.buffer().is_empty() {
            let _ = reader.get_ref().set_read_timeout(Some(IDLE_TIMEOUT));
            match reader.fill_buf() {
                Ok([]) | Err(_) => return,
                Ok(_) => {}
            }
        }
        let deadline = Instant::now() + head_timeout;
        let head = match read_head(&mut reader, deadline) {
            Ok(Some(h)) => h,
            Ok(None) => return,
            Err(msg) => {
                let r = Response::error(400, "bad_request", msg);
                let _ = write_response(&mut out, &r, false, false);
                return;
            }
        };
        let _ = reader.get_ref().set_read_timeout(Some(IDLE_TIMEOUT));
        let Head {
            method,
            target,
            http10,
            headers,
            head_bytes,
            content_length,
        } = head;
        let (path, raw_query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let path = percent_decode(&path, false);
        if path.bytes().any(|b| b < 0x20 || b == 0x7f) {
            let r = Response::error(400, "bad_request", "control characters in the path");
            let _ = write_response(&mut out, &r, false, false);
            return;
        }
        let wants_close = headers.iter().any(|(k, v)| {
            k == "connection" && v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close"))
        }) || http10;
        let mut req = Request {
            method,
            path,
            raw_query,
            headers,
            peer,
            head_bytes,
            body: (&mut reader).take(content_length),
            content_length,
        };
        let head_only = req.method == "HEAD";
        // The security rules first (13): Host, Origin and Content-Type.
        let (reply, cors_origin) = match sec.check(&req) {
            Decision::Refuse(r) => (Reply::Normal(r), None),
            Decision::Allow { cors_origin } => (h.handle(&mut req), cors_origin),
        };
        let leftover_body = req.body.limit();
        let _ = req;
        match reply {
            Reply::Takeover(f) => {
                let buffered = reader.buffer().to_vec();
                let Ok(s2) = out.try_clone() else { return };
                let _ = s2.set_read_timeout(None);
                f(s2, buffered);
                let _ = out.shutdown(Shutdown::Both);
                return;
            }
            Reply::Normal(mut r) => {
                if let Some(o) = cors_origin {
                    r.headers.push(("access-control-allow-origin".into(), o));
                    r.headers.push(("vary".into(), "Origin".into()));
                }
                // A request body nobody read is skipped when short; a long one ends the connection.
                let mut close = wants_close;
                if leftover_body > 0 {
                    if leftover_body <= 1 << 20 {
                        let mut sink = std::io::sink();
                        let _ = std::io::copy(&mut (&mut reader).take(leftover_body), &mut sink);
                    } else {
                        close = true;
                    }
                }
                r.head_only = r.head_only || head_only;
                if write_response(&mut out, &r, !close, head_only).is_err() || close {
                    return;
                }
            }
        }
    }
}

/// A parsed request head.
struct Head {
    method: String,
    target: String,
    http10: bool,
    headers: Vec<(String, String)>,
    head_bytes: Vec<u8>,
    content_length: u64,
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Read and check the request line and headers. `Ok(None)`: the connection ended or timed out (nothing to
/// answer); `Err`: a malformed or ambiguous request, answered 400 and the connection closed.
///
/// The framing rules are strict because the library replays requests to the stations (the proxy): a request
/// the two ends could frame differently must never get through. So: `Content-Length` must be digits and, if
/// repeated, the same; any `Transfer-Encoding` is refused (no client of ours sends one); a header name must be
/// a token directly followed by `:` (no white space, no obsolete line folding); one `Host`.
fn read_head(
    r: &mut BufReader<TcpStream>,
    deadline: Instant,
) -> Result<Option<Head>, &'static str> {
    let mut line = Vec::new();
    // Tolerate empty lines before a request (RFC 9112 2.2), a few of them.
    let mut blank = 0;
    let text = loop {
        match read_line(r, &mut line, deadline) {
            Ok(0) | Err(_) => return Ok(None),
            Ok(_) => {}
        }
        let t = String::from_utf8_lossy(&line).trim_end().to_string();
        if !t.is_empty() {
            break t;
        }
        blank += 1;
        if blank > 4 {
            return Err("empty request");
        }
    };
    let mut head_bytes = line.clone();
    let mut parts = text.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("malformed request line");
    };
    if !is_token(method) {
        return Err("malformed method");
    }
    if !(target.starts_with('/') || (target == "*" && method == "OPTIONS")) {
        return Err("the request target must be a path");
    }
    let http10 = match version {
        "HTTP/1.1" => false,
        "HTTP/1.0" => true,
        _ => return Err("unsupported HTTP version"),
    };
    let mut headers: Vec<(String, String)> = vec![];
    loop {
        let mut l = Vec::new();
        match read_line(r, &mut l, deadline) {
            Ok(0) | Err(_) => return Ok(None),
            Ok(_) => {}
        }
        head_bytes.extend_from_slice(&l);
        if head_bytes.len() > MAX_HEAD {
            return Err("the request head is too large");
        }
        let t = String::from_utf8_lossy(&l)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if t.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err("too many headers");
        }
        let Some((k, v)) = t.split_once(':') else {
            return Err("a header line without a colon");
        };
        if !is_token(k) {
            // Also refuses obsolete line folding (a line starting with white space) and `Name :`.
            return Err("a malformed header name");
        }
        headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
    }
    let mut content_length: Option<u64> = None;
    for (k, v) in &headers {
        match k.as_str() {
            "content-length" => {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("a malformed Content-Length");
                }
                let n: u64 = v.parse().map_err(|_| "a malformed Content-Length")?;
                if content_length.is_some_and(|c| c != n) {
                    return Err("conflicting Content-Length headers");
                }
                content_length = Some(n);
            }
            "transfer-encoding" => {
                return Err(
                    "Transfer-Encoding (chunked bodies) is not supported: send Content-Length",
                );
            }
            _ => {}
        }
    }
    if headers.iter().filter(|(k, _)| k == "host").count() > 1 {
        return Err("more than one Host header");
    }
    Ok(Some(Head {
        method: method.to_ascii_uppercase(),
        target: target.to_string(),
        http10,
        headers,
        head_bytes,
        content_length: content_length.unwrap_or(0),
    }))
}

pub fn write_response(
    w: &mut dyn Write,
    r: &Response,
    keep_alive: bool,
    head: bool,
) -> std::io::Result<()> {
    let mut buf = format!("HTTP/1.1 {} {}\r\n", r.status, reason(r.status)).into_bytes();
    let mut has_len = false;
    for (k, v) in &r.headers {
        if k == "content-length" {
            has_len = true;
        }
        buf.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    if !has_len && r.status != 101 && r.status != 204 && r.status != 304 {
        buf.extend_from_slice(format!("content-length: {}\r\n", r.body.len()).as_bytes());
    }
    buf.extend_from_slice(
        format!(
            "connection: {}\r\nx-content-type-options: nosniff\r\n\r\n",
            if keep_alive { "keep-alive" } else { "close" }
        )
        .as_bytes(),
    );
    if !head && !r.head_only {
        buf.extend_from_slice(&r.body);
    }
    w.write_all(&buf)?;
    w.flush()
}

/// A tiny blocking HTTP client for the proxy and the tests: one request, one response.
pub mod client {
    use super::*;

    pub struct Reply {
        pub status: u16,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Reply {
        pub fn header(&self, name: &str) -> Option<&str> {
            let n = name.to_ascii_lowercase();
            self.headers
                .iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.as_str())
        }
        pub fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
    }

    /// Send one request and read the whole answer (`Connection: close`).
    pub fn request(
        addr: &str,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        timeout: Duration,
    ) -> std::io::Result<Reply> {
        let mut s = TcpStream::connect(addr)?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        let mut req = format!("{method} {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: {addr}\r\n"));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
            req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req.push_str("Connection: close\r\n\r\n");
        // A server may refuse before it has read the body and close: the answer is still there to read.
        let sent = s.write_all(req.as_bytes()).and_then(|_| s.write_all(body));
        let _ = sent;
        read_reply(&mut BufReader::new(s))
    }

    pub fn read_reply<R: BufRead>(r: &mut R) -> std::io::Result<Reply> {
        let mut line = String::new();
        r.read_line(&mut line)?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| std::io::Error::other(format!("bad status line `{}`", line.trim())))?;
        let mut headers = vec![];
        loop {
            let mut l = String::new();
            if r.read_line(&mut l)? == 0 {
                break;
            }
            let t = l.trim_end();
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let len: Option<usize> = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse().ok());
        let mut body = vec![];
        if status == 101 || status == 204 || status == 304 {
            // no body
        } else if let Some(n) = len {
            body.resize(n, 0);
            r.read_exact(&mut body)?;
        } else {
            r.read_to_end(&mut body)?;
        }
        Ok(Reply {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Handler for Echo {
        fn handle(&self, req: &mut Request) -> Reply {
            let body = req.body_bytes(1 << 20).unwrap_or_default();
            Response::text(
                200,
                "text/plain",
                format!("{} {} {}", req.method, req.path, body.len()),
            )
            .into()
        }
    }

    fn server(head_timeout: Duration) -> Server {
        Server::start_with(
            "127.0.0.1:0",
            Arc::new(Echo),
            Arc::new(Security::default()),
            head_timeout,
        )
        .unwrap()
    }

    /// Send raw bytes and read everything the server answers until it closes (or 5 s pass).
    fn raw(srv: &Server, bytes: &[u8]) -> String {
        let mut s = TcpStream::connect(srv.addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(bytes).unwrap();
        let mut out = vec![];
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }

    fn statuses(answer: &str) -> Vec<u16> {
        answer
            .split("HTTP/1.1 ")
            .skip(1)
            .filter_map(|p| p.get(..3).and_then(|c| c.parse().ok()))
            .collect()
    }

    #[test]
    fn ambiguous_framing_is_refused_and_the_connection_closed() {
        let srv = server(HEAD_TIMEOUT);
        let host = "Host: 127.0.0.1\r\n";
        for (what, head) in [
            ("letters in Content-Length", "Content-Length: 5x\r\n"),
            ("a signed Content-Length", "Content-Length: +5\r\n"),
            (
                "two different lengths",
                "Content-Length: 5\r\nContent-Length: 6\r\n",
            ),
            ("a list of lengths", "Content-Length: 5, 5\r\n"),
            ("chunked", "Transfer-Encoding: chunked\r\n"),
            (
                "chunked beside a length",
                "Content-Length: 5\r\nTransfer-Encoding: chunked\r\n",
            ),
            ("gzip", "Transfer-Encoding: gzip\r\n"),
            ("space before the colon", "Content-Length : 5\r\n"),
            ("line folding", "X-A: 1\r\n  folded\r\n"),
            ("no colon", "Garbage\r\n"),
            ("two Host headers", "Host: 127.0.0.1\r\n"),
        ] {
            // The body would be a second request if the server framed it as having no body.
            let req =
                format!("POST /a HTTP/1.1\r\n{host}{head}\r\nGET /smuggled HTTP/1.1\r\n{host}\r\n");
            let a = raw(&srv, req.as_bytes());
            assert_eq!(statuses(&a), vec![400], "{what}: {a}");
            assert!(!a.contains("/smuggled"), "{what}");
        }
    }

    #[test]
    fn a_body_is_framed_by_its_length_and_keep_alive_serves_the_next_request() {
        let srv = server(HEAD_TIMEOUT);
        let a = raw(
            &srv,
            b"POST /a HTTP/1.1\r\nHost: x.localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}\
              GET /b HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(statuses(&a), vec![200, 200], "{a}");
        assert!(a.contains("POST /a 2") && a.contains("GET /b 0"), "{a}");
    }

    #[test]
    fn bad_request_lines_and_control_characters_are_refused() {
        let srv = server(HEAD_TIMEOUT);
        for line in [
            "GET /a HTTP/2.0",
            "GET http://evil/a HTTP/1.1",
            "GET /a  HTTP/1.1",
            "G(T /a HTTP/1.1",
            "GET /a%0d%0aX-Injected:%201 HTTP/1.1",
            "GET /a%00 HTTP/1.1",
        ] {
            let a = raw(
                &srv,
                format!("{line}\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
            );
            assert_eq!(statuses(&a), vec![400], "{line}: {a}");
        }
    }

    #[test]
    fn a_head_dripped_slowly_is_cut_off_at_the_head_time_limit() {
        let srv = server(Duration::from_millis(600));
        let mut s = TcpStream::connect(srv.addr).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let t = Instant::now();
        // One header byte every 100 ms: each read is quick, the head as a whole is not.
        let mut closed = false;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            if s.write_all(b"X").is_err() {
                closed = true;
                break;
            }
            s.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
            let mut b = [0u8; 256];
            if let Ok(0) = s.read(&mut b) {
                closed = true;
                break;
            }
        }
        assert!(closed, "the connection was never closed");
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        // Once the connection thread ends it gives its slot back.
        let t = Instant::now();
        while srv.connections() > 0 && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(srv.connections(), 0);
    }

    #[test]
    fn an_oversized_head_is_refused() {
        let srv = server(HEAD_TIMEOUT);
        let mut req = String::from("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        for i in 0..90 {
            req.push_str(&format!("X-{i}: {}\r\n", "a".repeat(1000)));
        }
        req.push_str("\r\n");
        let a = raw(&srv, req.as_bytes());
        assert_eq!(statuses(&a), vec![400], "{a}");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c", true), "a b c");
        assert_eq!(percent_decode("a%20b+c", false), "a b+c");
        assert_eq!(percent_decode("100%", true), "100%");
        assert_eq!(percent_decode("%zz", true), "%zz");
        assert_eq!(percent_decode("%e2%82%ac", true), "\u{20ac}");
        assert_eq!(
            parse_query("q=a+b&x=%26&flag"),
            vec![
                ("q".into(), "a b".into()),
                ("x".into(), "&".into()),
                ("flag".into(), "".into())
            ]
        );
    }
}
