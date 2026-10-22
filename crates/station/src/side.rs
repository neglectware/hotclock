//! Side effects of blocks: one OSC message over UDP, one MIDI message through a raw MIDI device, one HTTP
//! request (architecture 4.5, 4.9; milestone M5).
//!
//! The station never does network or device I/O on a thread that feeds audio. A block that fires a side
//! effect hands a [`SideJob`] to a [`SideSink`]; the production sink is a [`SideWorker`] (one thread, one
//! queue). The worker holds a job until its wall time (the station hands jobs over up to
//! [`SIDE_AHEAD_MS`] early), sends it, retries **once** after [`RETRY_MS`] when the first attempt fails,
//! and reports the outcome on a channel, so the block log says `fired` or `failed` for every message.
//! Nothing here can block the audio: the queue is bounded and a full queue drops the job and says so.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

/// Jobs are handed to the worker this long before their time; the worker waits for the exact moment.
pub const SIDE_AHEAD_MS: i64 = 250;
/// The pause before the one retry.
pub const RETRY_MS: u64 = 250;
/// Most jobs waiting at once; more are dropped and counted as failures.
pub const QUEUE_MAX: usize = 256;

/// One typed OSC argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum OscArg {
    #[serde(rename = "i")]
    Int(i32),
    #[serde(rename = "f")]
    Float(f32),
    #[serde(rename = "s")]
    Str(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiKind {
    Cc,
    NoteOn,
    NoteOff,
    Pc,
    Sysex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MidiMsg {
    pub kind: MidiKind,
    /// 1 to 16.
    pub channel: u8,
    pub data1: u8,
    pub data2: u8,
    /// The bytes between `F0` and `F7` of a system-exclusive message.
    pub data: Vec<u8>,
}

/// The thing a block does outside the station.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SideAction {
    Osc {
        host: String,
        port: u16,
        address: String,
        args: Vec<OscArg>,
    },
    Midi {
        port: String,
        msg: MidiMsg,
    },
    Http {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: Option<String>,
        timeout_s: u64,
    },
}

impl SideAction {
    pub fn type_name(&self) -> &'static str {
        match self {
            SideAction::Osc { .. } => "osc_out",
            SideAction::Midi { .. } => "midi_out",
            SideAction::Http { .. } => "http_request",
        }
    }

    /// A short line for logs and the block log's detail.
    pub fn describe(&self) -> String {
        match self {
            SideAction::Osc {
                host,
                port,
                address,
                args,
            } => format!(
                "osc {host}:{port} {address} {}",
                args.iter()
                    .map(|a| match a {
                        OscArg::Int(i) => format!("i:{i}"),
                        OscArg::Float(f) => format!("f:{f}"),
                        OscArg::Str(s) => format!("s:{s}"),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            SideAction::Midi { port, msg } => {
                format!("midi {port} {}", hex(&midi_bytes(msg).unwrap_or_default()))
            }
            SideAction::Http { method, url, .. } => format!("http {method} {url}"),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- OSC ---------------------------------------------------------------------------------------------------

fn osc_pad(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(s);
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// An OSC 1.0 message: address, type tags and arguments, every string null-terminated and padded to four
/// bytes, numbers big-endian. (Bundles are not used; the WING does not accept them.)
pub fn osc_packet(address: &str, args: &[OscArg]) -> Vec<u8> {
    let mut out = Vec::new();
    osc_pad(&mut out, address.as_bytes());
    let mut tags = String::from(",");
    for a in args {
        tags.push(match a {
            OscArg::Int(_) => 'i',
            OscArg::Float(_) => 'f',
            OscArg::Str(_) => 's',
        });
    }
    osc_pad(&mut out, tags.as_bytes());
    for a in args {
        match a {
            OscArg::Int(i) => out.extend_from_slice(&i.to_be_bytes()),
            OscArg::Float(f) => out.extend_from_slice(&f.to_be_bytes()),
            OscArg::Str(s) => osc_pad(&mut out, s.as_bytes()),
        }
    }
    out
}

/// Parse a packet made by [`osc_packet`] (for tests and the receiver of the loopback checks).
pub fn osc_parse(buf: &[u8]) -> Option<(String, Vec<OscArg>)> {
    fn cstr(buf: &[u8], at: &mut usize) -> Option<String> {
        let end = buf[*at..].iter().position(|b| *b == 0)? + *at;
        let s = String::from_utf8(buf[*at..end].to_vec()).ok()?;
        *at = (end + 1).div_ceil(4) * 4;
        Some(s)
    }
    let mut at = 0;
    let address = cstr(buf, &mut at)?;
    let tags = cstr(buf, &mut at)?;
    let mut args = Vec::new();
    for t in tags.strip_prefix(',')?.chars() {
        match t {
            'i' => {
                args.push(OscArg::Int(i32::from_be_bytes(
                    buf.get(at..at + 4)?.try_into().ok()?,
                )));
                at += 4;
            }
            'f' => {
                args.push(OscArg::Float(f32::from_be_bytes(
                    buf.get(at..at + 4)?.try_into().ok()?,
                )));
                at += 4;
            }
            's' => args.push(OscArg::Str(cstr(buf, &mut at)?)),
            _ => return None,
        }
    }
    Some((address, args))
}

// ---- MIDI --------------------------------------------------------------------------------------------------

/// The wire bytes of a MIDI message, or why it cannot be sent.
pub fn midi_bytes(m: &MidiMsg) -> Result<Vec<u8>, String> {
    if !(1..=16).contains(&m.channel) && m.kind != MidiKind::Sysex {
        return Err("the MIDI channel is 1 to 16".into());
    }
    let ch = m.channel.wrapping_sub(1) & 0x0f;
    if m.kind != MidiKind::Sysex && (m.data1 > 127 || m.data2 > 127) {
        return Err("MIDI data bytes are 0 to 127".into());
    }
    Ok(match m.kind {
        MidiKind::Cc => vec![0xB0 | ch, m.data1, m.data2],
        MidiKind::NoteOn => vec![0x90 | ch, m.data1, m.data2],
        MidiKind::NoteOff => vec![0x80 | ch, m.data1, m.data2],
        MidiKind::Pc => vec![0xC0 | ch, m.data1],
        MidiKind::Sysex => {
            if m.data.iter().any(|b| *b > 127) {
                return Err("system-exclusive data bytes are 0 to 127".into());
            }
            let mut v = vec![0xF0];
            v.extend_from_slice(&m.data);
            v.push(0xF7);
            v
        }
    })
}

/// The device file of a MIDI port: a path (`/dev/snd/midiC1D0`, or any file or FIFO) or an ALSA name
/// (`hw:1,0` or `hw:1,0,0`, which is `/dev/snd/midiC1D0`).
pub fn midi_device_path(port: &str) -> String {
    if let Some(rest) = port.strip_prefix("hw:") {
        let mut p = rest.split(',');
        if let (Some(c), Some(d)) = (p.next(), p.next()) {
            if let (Ok(c), Ok(d)) = (c.trim().parse::<u32>(), d.trim().parse::<u32>()) {
                return format!("/dev/snd/midiC{c}D{d}");
            }
        }
    }
    port.to_string()
}

// ---- execution ---------------------------------------------------------------------------------------------

fn send_once(a: &SideAction) -> Result<(), String> {
    match a {
        SideAction::Osc {
            host,
            port,
            address,
            args,
        } => {
            let addr = (host.as_str(), *port)
                .to_socket_addrs()
                .map_err(|e| format!("cannot resolve {host}: {e}"))?
                .next()
                .ok_or_else(|| format!("no address for {host}"))?;
            let bind = if addr.is_ipv6() {
                "[::]:0"
            } else {
                "0.0.0.0:0"
            };
            let sock = UdpSocket::bind(bind).map_err(|e| e.to_string())?;
            let pkt = osc_packet(address, args);
            sock.send_to(&pkt, addr).map_err(|e| e.to_string())?;
            Ok(())
        }
        SideAction::Midi { port, msg } => {
            let bytes = midi_bytes(msg)?;
            let path = midi_device_path(port);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| format!("cannot open MIDI port {path}: {e}"))?;
            f.write_all(&bytes).map_err(|e| e.to_string())?;
            f.flush().map_err(|e| e.to_string())
        }
        SideAction::Http {
            method,
            url,
            headers,
            body,
            timeout_s,
        } => http_request(method, url, headers, body.as_deref(), *timeout_s).map(|_| ()),
    }
}

/// Send `a`; on failure wait [`RETRY_MS`] and try once more. Returns the attempt count that worked, or the
/// last error.
pub fn send_with_retry(a: &SideAction) -> Result<u32, String> {
    match send_once(a) {
        Ok(()) => Ok(1),
        Err(first) => {
            std::thread::sleep(Duration::from_millis(RETRY_MS));
            send_once(a)
                .map(|()| 2)
                .map_err(|e| format!("{first}; retry: {e}"))
        }
    }
}

/// A plain `http://` request. TLS is not built in: use a local bridge for an `https://` service.
pub fn http_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout_s: u64,
) -> Result<u16, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        "only http:// addresses are supported (no TLS is built in); use a local bridge for https"
            .to_string()
    })?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port in the address")?),
        None => (hostport, 80),
    };
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .next()
        .ok_or_else(|| format!("no address for {host}"))?;
    let t = Duration::from_secs(timeout_s.clamp(1, 30));
    let mut s = TcpStream::connect_timeout(&addr, t).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(t)).ok();
    s.set_write_timeout(Some(t)).ok();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
    for (k, v) in headers {
        if !k.contains(['\r', '\n', ':']) && !v.contains(['\r', '\n']) {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut buf = [0u8; 512];
    while head.len() < 4096 {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                head.extend_from_slice(&buf[..n]);
                if head.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let line = String::from_utf8_lossy(&head);
    let code = line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("no HTTP status in the answer")?;
    if (200..400).contains(&code) {
        Ok(code)
    } else {
        Err(format!("the server answered {code}"))
    }
}

// ---- the sink and the worker -------------------------------------------------------------------------------

/// One thing to send, with the wall time it is meant for.
#[derive(Debug, Clone)]
pub struct SideJob {
    pub id: u64,
    pub block_id: i64,
    pub name: String,
    pub at_wall_ms: i64,
    /// The station's wall time when it handed the job over.
    pub handed_ms: i64,
    pub action: SideAction,
}

#[derive(Debug, Clone)]
pub struct SideResult {
    pub job: SideJob,
    pub ok: bool,
    pub attempts: u32,
    pub error: Option<String>,
    /// Wall time (unix ms) at which it was sent, by the system clock.
    pub sent_wall_ms: i64,
}

/// Where the station hands side jobs.
pub trait SideSink: Send {
    /// Queue a job; never blocks. `false` when it was dropped.
    fn submit(&mut self, job: SideJob) -> bool;
    /// Outcomes since the last call.
    fn results(&mut self) -> Vec<SideResult>;
}

pub struct SideWorker {
    tx: SyncSender<SideJob>,
    rx: Receiver<SideResult>,
}

fn system_wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl SideWorker {
    pub fn spawn() -> SideWorker {
        let (tx, jrx) = sync_channel::<SideJob>(QUEUE_MAX);
        let (rtx, rx) = std::sync::mpsc::channel::<SideResult>();
        std::thread::Builder::new()
            .name("side-effects".into())
            .spawn(move || {
                let mut pending: Vec<SideJob> = Vec::new();
                loop {
                    // Take what is waiting, or sleep until the next job is due.
                    let wait = pending
                        .iter()
                        .map(|j| j.at_wall_ms - system_wall_ms())
                        .min()
                        .map_or(Duration::from_secs(3600), |ms| {
                            Duration::from_millis(ms.max(0) as u64)
                        });
                    match jrx.recv_timeout(wait) {
                        Ok(j) => pending.push(j),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            if pending.is_empty() {
                                return;
                            }
                        }
                    }
                    let now = system_wall_ms();
                    let (due, later): (Vec<_>, Vec<_>) =
                        pending.drain(..).partition(|j| j.at_wall_ms <= now);
                    pending = later;
                    let mut due = due;
                    due.sort_by_key(|j| (j.at_wall_ms, j.id));
                    for job in due {
                        let r = send_with_retry(&job.action);
                        let sent = system_wall_ms();
                        let res = match r {
                            Ok(n) => SideResult {
                                job,
                                ok: true,
                                attempts: n,
                                error: None,
                                sent_wall_ms: sent,
                            },
                            Err(e) => SideResult {
                                job,
                                ok: false,
                                attempts: 2,
                                error: Some(e),
                                sent_wall_ms: sent,
                            },
                        };
                        if rtx.send(res).is_err() {
                            return;
                        }
                    }
                }
            })
            .expect("the side-effects thread starts");
        SideWorker { tx, rx }
    }
}

impl SideSink for SideWorker {
    fn submit(&mut self, job: SideJob) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn results(&mut self) -> Vec<SideResult> {
        self.rx.try_iter().collect()
    }
}

/// A sink for the offline renderer and the tests: it records every job with the wall time the station
/// handed it over. The "arrival" of a job is its own time, or the handover when that was later (what the
/// worker does: it holds an early job until its time). It can be told to fail the first attempts.
#[derive(Default)]
pub struct RecordingSink {
    pub jobs: Vec<(i64, SideJob)>,
    pub fail_next: u32,
    pub done: Vec<SideResult>,
}

impl RecordingSink {
    /// `(arrival wall ms, action)` for every job.
    pub fn arrivals(&self) -> Vec<(i64, SideAction)> {
        self.jobs
            .iter()
            .map(|(h, j)| ((*h).max(j.at_wall_ms), j.action.clone()))
            .collect()
    }
}

impl SideSink for RecordingSink {
    fn submit(&mut self, job: SideJob) -> bool {
        let (ok, attempts, error) = if self.fail_next > 0 {
            self.fail_next -= 1;
            (false, 2, Some("simulated failure".to_string()))
        } else {
            (true, 1, None)
        };
        self.jobs.push((job.handed_ms, job.clone()));
        let sent = job.handed_ms.max(job.at_wall_ms);
        self.done.push(SideResult {
            job,
            ok,
            attempts,
            error,
            sent_wall_ms: sent,
        });
        true
    }

    fn results(&mut self) -> Vec<SideResult> {
        std::mem::take(&mut self.done)
    }
}

// ---- templates ---------------------------------------------------------------------------------------------

/// Ready-made block parameters for the editor (the "Behringer WING channel mute" template and friends).
/// Each entry is `{id, title, kind, params, notes}`; the WING facts are in architecture 4.9 with their
/// sources and what is unconfirmed.
pub fn templates() -> Value {
    json!([
        {
            "id": "wing_channel_mute",
            "title": "Behringer WING channel mute",
            "kind": "osc_out",
            "params": {
                "host": "192.0.2.100", "port": 2223, "address": "/ch/2/mute",
                "args": [{"type": "i", "value": 1}]
            },
            "notes": "Port 2223 is the WING's OSC port. The channel number has no leading zero. 1 mutes, 0 unmutes. Set the console's address, the channel, and make sure Setup > Remote > Remote Lock for OSC is off, or the console ignores writes."
        },
        {
            "id": "wing_channel_unmute",
            "title": "Behringer WING channel unmute",
            "kind": "osc_out",
            "params": {
                "host": "192.0.2.100", "port": 2223, "address": "/ch/2/mute",
                "args": [{"type": "i", "value": 0}]
            },
            "notes": "The same address with 0."
        },
        {
            "id": "midi_cc",
            "title": "MIDI control change",
            "kind": "midi_out",
            "params": {"port": "hw:1,0", "message": {"type": "cc", "channel": 1, "data1": 7, "data2": 100}},
            "notes": "Port is a rawmidi name (hw:card,device) or a device path."
        },
        {
            "id": "webhook",
            "title": "Web request",
            "kind": "http_request",
            "params": {"method": "POST", "url": "http://127.0.0.1:8080/hook", "headers": {}, "body": "", "timeout_s": 5},
            "notes": "Plain http:// only."
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_packet_is_padded_and_round_trips() {
        let p = osc_packet("/ch/2/mute", &[OscArg::Int(1)]);
        assert_eq!(p.len() % 4, 0);
        // "/ch/2/mute\0\0" (12) + ",i\0\0" (4) + 4 bytes.
        assert_eq!(p.len(), 20);
        assert_eq!(&p[..10], b"/ch/2/mute");
        assert_eq!(&p[12..14], b",i");
        assert_eq!(&p[16..], &[0, 0, 0, 1]);
        let (a, args) = osc_parse(&p).unwrap();
        assert_eq!(a, "/ch/2/mute");
        assert_eq!(args, vec![OscArg::Int(1)]);
        let p = osc_packet(
            "/x",
            &[
                OscArg::Float(0.5),
                OscArg::Str("hello".into()),
                OscArg::Int(-3),
            ],
        );
        let (_, args) = osc_parse(&p).unwrap();
        assert_eq!(
            args,
            vec![
                OscArg::Float(0.5),
                OscArg::Str("hello".into()),
                OscArg::Int(-3)
            ]
        );
    }

    #[test]
    fn midi_bytes_are_right() {
        let m = |kind, channel, data1, data2| MidiMsg {
            kind,
            channel,
            data1,
            data2,
            data: vec![],
        };
        assert_eq!(
            midi_bytes(&m(MidiKind::Cc, 1, 7, 100)).unwrap(),
            vec![0xB0, 7, 100]
        );
        assert_eq!(
            midi_bytes(&m(MidiKind::NoteOn, 16, 60, 127)).unwrap(),
            vec![0x9F, 60, 127]
        );
        assert_eq!(
            midi_bytes(&m(MidiKind::NoteOff, 2, 60, 0)).unwrap(),
            vec![0x81, 60, 0]
        );
        assert_eq!(
            midi_bytes(&m(MidiKind::Pc, 3, 5, 0)).unwrap(),
            vec![0xC2, 5]
        );
        assert!(midi_bytes(&m(MidiKind::Cc, 0, 7, 100)).is_err());
        assert!(midi_bytes(&m(MidiKind::Cc, 1, 200, 0)).is_err());
        let mut s = m(MidiKind::Sysex, 1, 0, 0);
        s.data = vec![0x7E, 0x7F];
        assert_eq!(midi_bytes(&s).unwrap(), vec![0xF0, 0x7E, 0x7F, 0xF7]);
        assert_eq!(midi_device_path("hw:1,0"), "/dev/snd/midiC1D0");
        assert_eq!(midi_device_path("hw:2,3,0"), "/dev/snd/midiC2D3");
        assert_eq!(midi_device_path("/tmp/x"), "/tmp/x");
    }
}

/// A sink that sends nothing: it logs what it was given and reports success. Used when
/// `HOTCLOCK_SIDE_DRYRUN` is set (the browser checks, a rehearsal with no devices attached).
#[derive(Default)]
pub struct LogSink {
    done: Vec<SideResult>,
}

impl SideSink for LogSink {
    fn submit(&mut self, job: SideJob) -> bool {
        tracing::info!(block = %job.name, "side effect (dry run, not sent): {}", job.action.describe());
        self.done.push(SideResult {
            sent_wall_ms: job.at_wall_ms,
            job,
            ok: true,
            attempts: 1,
            error: None,
        });
        true
    }

    fn results(&mut self) -> Vec<SideResult> {
        std::mem::take(&mut self.done)
    }
}

/// A recorder that the test keeps a handle to.
#[derive(Clone, Default)]
pub struct SharedRecording(pub std::sync::Arc<std::sync::Mutex<RecordingSink>>);

impl SideSink for SharedRecording {
    fn submit(&mut self, job: SideJob) -> bool {
        self.0.lock().expect("recorder").submit(job)
    }

    fn results(&mut self) -> Vec<SideResult> {
        self.0.lock().expect("recorder").results()
    }
}
