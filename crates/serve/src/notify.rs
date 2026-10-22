//! Notifications (architecture 12.1): an SMTP channel, the per-event rules, immediate messages with a rate
//! limit, hourly and daily digests, and the notifier that watches every local station's alerts.
//!
//! The notifier reads each station's `state.db` (`alerts`) read-only, so an alert raised while the library
//! service was down is sent when it comes back: a cursor per station, kept in `library.db`, says how far it
//! has read. The SMTP password is write-only: it lives in `secrets.toml` (mode 0600), never in the database,
//! a log line or an API answer.

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---- SMTP --------------------------------------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    /// `starttls` (default), `tls` or `none`.
    pub tls: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub to: Vec<String>,
    /// An extra root certificate (PEM) to trust: for a mail server with a private certificate.
    pub ca_pem: Option<String>,
}

/// The password is never printed, not even by `{:?}`.
impl std::fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<set>"))
            .field("from", &self.from)
            .field("to", &self.to)
            .field("ca_pem", &self.ca_pem.as_ref().map(|_| "<set>"))
            .finish()
    }
}

/// A value that goes into an SMTP command or a mail header must be one line.
fn one_line(what: &str, v: &str) -> Result<(), String> {
    if v.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{what} must not contain line breaks or control characters"
        ));
    }
    Ok(())
}

impl SmtpConfig {
    pub fn from_channel(config: &Value, password: Option<String>) -> Result<SmtpConfig, String> {
        let s = |k: &str| config[k].as_str().map(str::to_string);
        let host = s("host")
            .filter(|h| !h.is_empty())
            .ok_or("the channel needs a host")?;
        let from = s("from")
            .filter(|h| !h.is_empty())
            .ok_or("the channel needs a from address")?;
        let to: Vec<String> = config["to"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if to.is_empty() {
            return Err("the channel needs at least one to address".into());
        }
        one_line("the host", &host)?;
        one_line("the from address", &from)?;
        for t in &to {
            one_line("a to address", t)?;
        }
        if let Some(u) = s("username") {
            one_line("the username", &u)?;
        }
        let tls = s("tls").unwrap_or_else(|| "starttls".into());
        if !matches!(tls.as_str(), "starttls" | "tls" | "none") {
            return Err(format!("tls must be starttls, tls or none, not `{tls}`"));
        }
        let port = config["port"]
            .as_u64()
            .map(|p| p as u16)
            .unwrap_or(match tls.as_str() {
                "tls" => 465,
                "none" => 25,
                _ => 587,
            });
        Ok(SmtpConfig {
            host,
            port,
            tls,
            username: s("username").filter(|u| !u.is_empty()),
            password,
            from,
            to,
            ca_pem: s("ca_pem"),
        })
    }
}

enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Conn {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(b),
            Conn::Tls(s) => s.read(b),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(b),
            Conn::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

fn tls_wrap(tcp: TcpStream, cfg: &SmtpConfig) -> Result<Conn, String> {
    use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName};
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = &cfg.ca_pem {
        for c in CertificateDer::pem_slice_iter(pem.as_bytes()).flatten() {
            roots.add(c).map_err(|e| format!("ca_pem: {e}"))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let conf = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(cfg.host.clone()).map_err(|e| format!("server name: {e}"))?;
    let conn = rustls::ClientConnection::new(Arc::new(conf), name).map_err(|e| e.to_string())?;
    Ok(Conn::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// One reply: the last line's text and its code. Multi-line replies (`250-`) are read to the end.
fn reply(r: &mut BufReader<&mut Conn>) -> Result<(u16, String), String> {
    loop {
        let mut l = String::new();
        let n = r
            .read_line(&mut l)
            .map_err(|e| format!("reading the server's reply: {e}"))?;
        if n == 0 {
            return Err("the server closed the connection".into());
        }
        let t = l.trim_end().to_string();
        let code: u16 = t
            .get(..3)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("bad reply `{t}`"))?;
        let more = t.as_bytes().get(3) == Some(&b'-');
        if !more {
            return Ok((code, t));
        }
    }
}

fn expect(r: &mut BufReader<&mut Conn>, ok: &[u16], what: &str) -> Result<String, String> {
    let (code, line) = reply(r)?;
    if ok.contains(&code) {
        Ok(line)
    } else {
        Err(format!("{what}: {line}"))
    }
}

fn cmd(c: &mut Conn, s: &str) -> Result<(), String> {
    c.write_all(format!("{s}\r\n").as_bytes())
        .and_then(|_| c.flush())
        .map_err(|e| format!("writing to the server: {e}"))
}

fn b64(s: &[u8]) -> String {
    crate::ws::base64(s)
}

/// Send one message. `Ok` carries the server's last reply (the transcript's last line) for the test button.
pub fn send_mail(cfg: &SmtpConfig, subject: &str, body: &str) -> Result<String, String> {
    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {}: {e}", cfg.host))?
        .next()
        .ok_or_else(|| format!("no address for {}", cfg.host))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(15))
        .map_err(|e| format!("cannot connect to {}:{}: {e}", cfg.host, cfg.port))?;
    tcp.set_read_timeout(Some(Duration::from_secs(30))).ok();
    tcp.set_write_timeout(Some(Duration::from_secs(30))).ok();
    let mut conn = if cfg.tls == "tls" {
        tls_wrap(tcp, cfg)?
    } else {
        Conn::Plain(tcp)
    };
    let helo = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into());
    let mut caps = String::new();
    {
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[220], "greeting")?;
    }
    for round in 0..2 {
        cmd(&mut conn, &format!("EHLO {helo}"))?;
        {
            let mut r = BufReader::new(&mut conn);
            // Read the whole capability list.
            loop {
                let mut l = String::new();
                r.read_line(&mut l).map_err(|e| e.to_string())?;
                caps.push_str(&l);
                let t = l.trim_end();
                if t.len() < 4 || t.as_bytes()[3] != b'-' {
                    if !t.starts_with("250") {
                        return Err(format!("EHLO: {t}"));
                    }
                    break;
                }
            }
        }
        if round == 0 && cfg.tls == "starttls" {
            if !caps.to_ascii_uppercase().contains("STARTTLS") {
                return Err("the server does not offer STARTTLS (set tls to none only for a trusted local relay)".into());
            }
            cmd(&mut conn, "STARTTLS")?;
            {
                let mut r = BufReader::new(&mut conn);
                expect(&mut r, &[220], "STARTTLS")?;
            }
            let tcp = match conn {
                Conn::Plain(t) => t,
                Conn::Tls(_) => unreachable!(),
            };
            conn = tls_wrap(tcp, cfg)?;
            caps.clear();
            continue;
        }
        break;
    }
    if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
        // PLAIN unless the server lists AUTH mechanisms without it and offers LOGIN (some relays do).
        let mechs: Vec<String> = caps
            .lines()
            .filter_map(|l| l.get(4..))
            .filter(|l| l.to_ascii_uppercase().starts_with("AUTH"))
            .flat_map(|l| {
                l.split_whitespace()
                    .skip(1)
                    .map(str::to_ascii_uppercase)
                    .collect::<Vec<_>>()
            })
            .collect();
        if !mechs.is_empty()
            && !mechs.iter().any(|m| m == "PLAIN")
            && mechs.iter().any(|m| m == "LOGIN")
        {
            cmd(&mut conn, "AUTH LOGIN")?;
            {
                let mut r = BufReader::new(&mut conn);
                expect(&mut r, &[334], "authentication")?;
            }
            cmd(&mut conn, &b64(u.as_bytes()))?;
            {
                let mut r = BufReader::new(&mut conn);
                expect(&mut r, &[334], "authentication")?;
            }
            cmd(&mut conn, &b64(p.as_bytes()))?;
        } else {
            let token = b64(format!("\0{u}\0{p}").as_bytes());
            cmd(&mut conn, &format!("AUTH PLAIN {token}"))?;
        }
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[235], "authentication")?;
    }
    let bare = |a: &str| -> String {
        match (a.rfind('<'), a.rfind('>')) {
            (Some(i), Some(j)) if j > i => a[i + 1..j].to_string(),
            _ => a.trim().to_string(),
        }
    };
    cmd(&mut conn, &format!("MAIL FROM:<{}>", bare(&cfg.from)))?;
    {
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[250], "MAIL FROM")?;
    }
    for t in &cfg.to {
        cmd(&mut conn, &format!("RCPT TO:<{}>", bare(t)))?;
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[250, 251], "RCPT TO")?;
    }
    cmd(&mut conn, "DATA")?;
    {
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[354], "DATA")?;
    }
    let mut msg = String::new();
    msg.push_str(&format!("From: {}\r\n", cfg.from));
    msg.push_str(&format!("To: {}\r\n", cfg.to.join(", ")));
    // Header values are one line; a subject with a newline in it would be a header injection.
    msg.push_str(&format!(
        "Subject: {}\r\n",
        subject.replace(['\r', '\n'], " ")
    ));
    let now = crate::station_api::wall_ms();
    msg.push_str(&format!("Date: {}\r\n", rfc5322_date(now)));
    msg.push_str(&format!(
        "Message-ID: <{now}.{}@hotclock>\r\n",
        std::process::id()
    ));
    msg.push_str("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n");
    for line in body.replace("\r\n", "\n").split('\n') {
        // Dot-stuffing.
        if line.starts_with('.') {
            msg.push('.');
        }
        msg.push_str(line);
        msg.push_str("\r\n");
    }
    msg.push_str(".\r\n");
    conn.write_all(msg.as_bytes())
        .map_err(|e| format!("sending the message: {e}"))?;
    conn.flush().map_err(|e| e.to_string())?;
    let last = {
        let mut r = BufReader::new(&mut conn);
        expect(&mut r, &[250], "the message was refused")?
    };
    let _ = cmd(&mut conn, "QUIT");
    Ok(last)
}

/// `Wed, 30 Sep 2026 07:00:00 +0000`: the date format mail headers require (RFC 5322 3.3).
pub fn rfc5322_date(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    const WD: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {d:02} {} {y} {:02}:{:02}:{:02} +0000",
        WD[days.rem_euclid(7) as usize],
        MO[(m - 1) as usize],
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
}

// ---- secrets ---------------------------------------------------------------------------------------------------

pub fn read_secret(path: &Path, channel_id: i64) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: toml::Value = toml::from_str(&text).ok()?;
    v.get("channels")?
        .get(channel_id.to_string())?
        .get("password")?
        .as_str()
        .map(str::to_string)
}

pub fn secret_is_set(path: &Path, channel_id: i64) -> bool {
    read_secret(path, channel_id).is_some()
}

pub fn write_secret(path: &Path, channel_id: i64, password: &str) -> Result<(), String> {
    let mut root: toml::Table = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    let channels = root
        .entry("channels")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(t) = channels.as_table_mut() {
        let mut ch = toml::Table::new();
        ch.insert("password".into(), toml::Value::String(password.into()));
        t.insert(channel_id.to_string(), toml::Value::Table(ch));
    }
    write_secrets_file(path, &root)
}

/// Remove a channel's password (the channel was deleted).
pub fn remove_secret(path: &Path, channel_id: i64) -> Result<(), String> {
    let Some(mut root) = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str::<toml::Table>(&t).ok())
    else {
        return Ok(());
    };
    let removed = root
        .get_mut("channels")
        .and_then(|c| c.as_table_mut())
        .and_then(|t| t.remove(&channel_id.to_string()))
        .is_some();
    if removed {
        write_secrets_file(path, &root)?;
    }
    Ok(())
}

/// Write the file whole and atomically, `0600` from its first byte: the temporary file is created with that
/// mode, so the password is never readable by others, not even for a moment.
fn write_secrets_file(path: &Path, root: &toml::Table) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let text = toml::to_string(root).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    // The umask can only take bits away; set the mode exactly anyway.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

// ---- rules -----------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub code: String,
    pub enabled: bool,
    pub severity: Option<String>,
    pub channels: Vec<i64>,
    pub delivery: String,
    pub rate_limit_min: i64,
    pub notify_on_clear: bool,
}

/// The table of 12.1: the default delivery of each alert code.
pub fn default_rule(code: &str) -> Rule {
    let (delivery, clear) = match code {
        "silence"
        | "safe_mode"
        | "queue_starved"
        | "device_lost"
        | "engine_stall"
        | "media_store_failing"
        | "state_db_corrupt"
        | "library_db_unavailable"
        | "db_unavailable"
        | "disk_full"
        | "service_failed"
        // Critical on a stream-only station (it is off the air): sent at once, and its clearing too.
        | "stream_encoder_failed" => ("immediate", true),
        "item_quarantined" | "playlist_depleted" => ("immediate", false),
        "item_missing"
        | "item_truncated"
        | "item_silent"
        | "item_decode_failed"
        | "analysis_failed"
        | "item_marker_invalid" => ("hourly_digest", false),
        "restarted" | "scheduler_restart" | "xrun_burst" | "clock_jump" | "db_backlog" => {
            ("hourly_digest", false)
        }
        "stream_no_listener" | "block_failed" => ("immediate", false),
        "stream_encoder_lag"
        | "stream_dropped"
        | "stream_encoder_restart"
        | "block_missed"
        | "block_source_missing"
        | "fallback" => ("hourly_digest", false),
        "loudness_at_limit"
        | "loudness_frozen"
        | "disk_low"
        | "backup_check_failed"
        | "media_verify_mismatch"
        | "import_failed" => ("daily_digest", false),
        "decode_glitch" => ("off", false),
        _ => ("hourly_digest", false),
    };
    Rule {
        code: code.into(),
        enabled: true,
        severity: None,
        channels: vec![],
        delivery: delivery.into(),
        rate_limit_min: 60,
        notify_on_clear: clear,
    }
}

pub fn rule_for(conn: &Connection, code: &str) -> Rule {
    let row: Option<(i64, Option<String>, String, String, i64, i64)> = conn
        .query_row(
            "SELECT enabled, severity, channels, delivery, rate_limit_min, notify_on_clear FROM notify_rules WHERE code = ?1",
            [code],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .unwrap_or(None);
    match row {
        None => default_rule(code),
        Some((en, sev, ch, del, rl, nc)) => Rule {
            code: code.into(),
            enabled: en != 0,
            severity: sev,
            channels: serde_json::from_str(&ch).unwrap_or_default(),
            delivery: del,
            rate_limit_min: rl,
            notify_on_clear: nc != 0,
        },
    }
}

/// Every alert code the design names, for the Notifications page.
pub const CODES: &[&str] = &[
    "silence",
    "safe_mode",
    "queue_starved",
    "device_lost",
    "engine_stall",
    "media_store_failing",
    "state_db_corrupt",
    "library_db_unavailable",
    "db_unavailable",
    "disk_full",
    "service_failed",
    "item_quarantined",
    "item_missing",
    "item_truncated",
    "item_silent",
    "item_decode_failed",
    "analysis_failed",
    "item_marker_invalid",
    "playlist_depleted",
    "restarted",
    "scheduler_restart",
    "xrun_burst",
    "clock_jump",
    "db_backlog",
    "stream_no_listener",
    "stream_encoder_lag",
    "stream_dropped",
    "stream_encoder_restart",
    "stream_encoder_failed",
    "block_failed",
    "block_missed",
    "block_source_missing",
    "fallback",
    "loudness_at_limit",
    "loudness_frozen",
    "disk_low",
    "backup_check_failed",
    "media_verify_mismatch",
    "import_failed",
    "decode_glitch",
];

pub fn rule_json(r: &Rule) -> Value {
    json!({"code": r.code, "enabled": r.enabled, "severity": r.severity, "channels": r.channels,
           "delivery": r.delivery, "rate_limit_min": r.rate_limit_min, "notify_on_clear": r.notify_on_clear})
}

// ---- the notifier ----------------------------------------------------------------------------------------------

/// One thing that happened to an alert.
#[derive(Debug, Clone)]
struct AlertEv {
    station: String,
    code: String,
    severity: String,
    message: String,
    cleared: bool,
}

pub struct Notifier {
    pub library_db: PathBuf,
    pub stations_dir: PathBuf,
    pub secrets: PathBuf,
    pub product: String,
    pub sent: Arc<AtomicU64>,
    tracked: HashMap<(String, i64), String>,
    last_hour: Option<i64>,
    last_day: Option<i64>,
}

fn open_ro(p: &Path) -> Option<Connection> {
    Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

fn open_rw(p: &Path) -> Option<Connection> {
    let c = Connection::open(p).ok()?;
    c.busy_timeout(Duration::from_secs(10)).ok()?;
    Some(c)
}

fn now_iso() -> String {
    station::state::iso_ms(crate::station_api::wall_ms())
}

impl Notifier {
    pub fn new(
        library_db: PathBuf,
        stations_dir: PathBuf,
        secrets: PathBuf,
        product: String,
    ) -> Notifier {
        Notifier {
            library_db,
            stations_dir,
            secrets,
            product,
            sent: Arc::new(AtomicU64::new(0)),
            tracked: HashMap::new(),
            last_hour: None,
            last_day: None,
        }
    }

    /// The channels a rule reaches: its own list, or every enabled one.
    fn channels(&self, conn: &Connection, rule: &Rule) -> Vec<(i64, String, SmtpConfig)> {
        let mut out = vec![];
        let Ok(mut st) =
            conn.prepare("SELECT id, name, kind, config, enabled FROM notify_channels ORDER BY id")
        else {
            return out;
        };
        let rows: Vec<(i64, String, String, String, i64)> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map(|r| r.flatten().collect())
            .unwrap_or_default();
        for (id, name, kind, config, enabled) in rows {
            if enabled == 0
                || kind != "smtp"
                || (!rule.channels.is_empty() && !rule.channels.contains(&id))
            {
                continue;
            }
            let cfg: Value = serde_json::from_str(&config).unwrap_or(Value::Null);
            if let Ok(c) = SmtpConfig::from_channel(&cfg, read_secret(&self.secrets, id)) {
                out.push((id, name, c));
            }
        }
        out
    }

    fn log(
        &self,
        conn: &Connection,
        station: &str,
        code: &str,
        channel: Option<i64>,
        result: &str,
        kind: &str,
    ) {
        let _ = conn.execute(
            "INSERT INTO notify_log(at, station, code, channel_id, result, kind) VALUES(?1,?2,?3,?4,?5,?6)",
            params![now_iso(), station, code, channel, result, kind],
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn deliver(
        &self,
        conn: &Connection,
        rule: &Rule,
        station: &str,
        code: &str,
        subject: &str,
        body: &str,
        kind: &str,
    ) -> bool {
        let chans = self.channels(conn, rule);
        if chans.is_empty() {
            self.log(conn, station, code, None, "no channel", kind);
            return false;
        }
        let mut ok = false;
        for (id, _, cfg) in chans {
            match send_mail(&cfg, subject, body) {
                Ok(reply) => {
                    ok = true;
                    self.sent.fetch_add(1, Ordering::Relaxed);
                    self.log(
                        conn,
                        station,
                        code,
                        Some(id),
                        &format!("sent: {reply}"),
                        kind,
                    );
                }
                Err(e) => self.log(conn, station, code, Some(id), &format!("failed: {e}"), kind),
            }
        }
        ok
    }

    fn subject(&self, station: &str, ev: &AlertEv) -> String {
        if ev.cleared {
            format!("[{} {station}] cleared: {}", self.product, ev.code)
        } else {
            format!(
                "[{} {station}] {} {}",
                self.product,
                ev.severity.to_uppercase(),
                ev.code
            )
        }
    }

    fn handle(&mut self, conn: &Connection, ev: AlertEv) {
        let rule = rule_for(conn, &ev.code);
        if !rule.enabled || rule.delivery == "off" {
            return;
        }
        if ev.cleared && !rule.notify_on_clear {
            return;
        }
        let severity = rule.severity.clone().unwrap_or_else(|| ev.severity.clone());
        let ev = AlertEv { severity, ..ev };
        let queue = |bucket: &str, suppressed: bool| {
            let _ = conn.execute(
                "INSERT INTO notify_queue(at, station, code, severity, message, cleared, bucket) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![now_iso(), ev.station, ev.code, ev.severity, ev.message, i64::from(ev.cleared), bucket],
            );
            if suppressed {
                self.log(
                    conn,
                    &ev.station,
                    &ev.code,
                    None,
                    "rate limited: goes into the next digest",
                    "suppressed",
                );
            }
        };
        match rule.delivery.as_str() {
            "hourly_digest" => queue("hourly_digest", false),
            "daily_digest" => queue("daily_digest", false),
            _ => {
                // Immediate, at most one raise and one clear per code per station per rate-limit window: an
                // alert that flaps would otherwise mail every clear (the raises being held back) without end.
                let kind = if ev.cleared {
                    "immediate_clear"
                } else {
                    "immediate"
                };
                let recent: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM notify_log WHERE station = ?1 AND code = ?2 AND kind = ?4
                         AND result LIKE 'sent%' AND at > ?3",
                        params![
                            ev.station,
                            ev.code,
                            station::state::iso_ms(crate::station_api::wall_ms() - rule.rate_limit_min * 60_000),
                            kind
                        ],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if recent > 0 {
                    queue("hourly_digest", true);
                    return;
                }
                let body = format!(
                    "{}\n\nstation: {}\ncode: {}\nseverity: {}\nat: {}\n",
                    ev.message,
                    ev.station,
                    ev.code,
                    ev.severity,
                    now_iso()
                );
                if !self.deliver(
                    conn,
                    &rule,
                    &ev.station,
                    &ev.code,
                    &self.subject(&ev.station, &ev),
                    &body,
                    kind,
                ) {
                    // Not lost: the next digest carries it.
                    queue("hourly_digest", false);
                }
            }
        }
    }

    /// Read the stations' alerts and turn what changed into events.
    fn poll_stations(&mut self, conn: &Connection) -> Vec<AlertEv> {
        let mut out = vec![];
        let Ok(rd) = std::fs::read_dir(&self.stations_dir) else {
            return out;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        for station in names {
            let Some(sdb) = open_ro(&self.stations_dir.join(&station).join("state.db")) else {
                continue;
            };
            let cursor: i64 = conn
                .query_row(
                    "SELECT last_alert_id FROM notify_cursor WHERE station = ?1",
                    [&station],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let tracked_ids: Vec<i64> = self
                .tracked
                .keys()
                .filter(|(s, _)| *s == station)
                .map(|(_, id)| *id)
                .collect();
            let list = tracked_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id, code, severity, message, raised_ms, cleared_ms FROM alerts
                 WHERE id > ?1 {} ORDER BY id",
                if list.is_empty() {
                    String::new()
                } else {
                    format!("OR id IN ({list})")
                }
            );
            let Ok(mut st) = sdb.prepare(&sql) else {
                continue;
            };
            let rows: Vec<(i64, String, String, String, i64, Option<i64>)> = st
                .query_map([cursor], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get(5)?,
                    ))
                })
                .map(|r| r.flatten().collect())
                .unwrap_or_default();
            let mut max_id = cursor;
            let stale_ms = crate::station_api::wall_ms() - 24 * 3_600_000;
            for (id, code, sev, msg, raised, cleared) in rows {
                max_id = max_id.max(id);
                let key = (station.clone(), id);
                let known = self.tracked.get(&key).cloned();
                let ev = |cleared: bool| AlertEv {
                    station: station.clone(),
                    code: code.clone(),
                    severity: sev.clone(),
                    message: msg.clone(),
                    cleared,
                };
                match (known, cleared) {
                    (None, None) => {
                        if raised >= stale_ms {
                            out.push(ev(false));
                        }
                        self.tracked.insert(key, sev.clone());
                    }
                    (None, Some(_)) => {
                        // Raised and cleared while nobody was looking: one message, the clear too when asked.
                        if raised >= stale_ms {
                            out.push(ev(false));
                            out.push(ev(true));
                        }
                    }
                    (Some(prev), None) => {
                        if prev != sev {
                            out.push(ev(false));
                            self.tracked.insert(key, sev.clone());
                        }
                    }
                    (Some(_), Some(_)) => {
                        out.push(ev(true));
                        self.tracked.remove(&key);
                    }
                }
            }
            if max_id > cursor {
                let _ = conn.execute(
                    "INSERT INTO notify_cursor(station, last_alert_id) VALUES(?1, ?2)
                     ON CONFLICT(station) DO UPDATE SET last_alert_id = ?2",
                    params![station, max_id],
                );
            }
        }
        out
    }

    /// Minutes east of UTC now, from the system (`date +%z`).
    fn local_offset_min() -> i64 {
        let z = std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let z = z.trim();
        if z.len() == 5 {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let h: i64 = z[1..3].parse().unwrap_or(0);
            let m: i64 = z[3..5].parse().unwrap_or(0);
            return sign * (h * 60 + m);
        }
        0
    }

    /// Send the digests that are due: hourly at the top of each hour, daily at 07:00 local.
    fn digests(&mut self, conn: &Connection) {
        let now = crate::station_api::wall_ms();
        let hour = now.div_euclid(3_600_000);
        let local_min = (now.div_euclid(60_000) + Self::local_offset_min()).rem_euclid(1440);
        let day = (now.div_euclid(60_000) + Self::local_offset_min()).div_euclid(1440);
        let mut due = vec![];
        if self.last_hour.is_none() {
            self.last_hour = Some(hour);
        } else if self.last_hour != Some(hour) {
            self.last_hour = Some(hour);
            due.push("hourly_digest");
        }
        if local_min >= 7 * 60 && self.last_day != Some(day) {
            if self.last_day.is_some() {
                due.push("daily_digest");
            }
            self.last_day = Some(day);
        }
        for bucket in due {
            self.send_digest(conn, bucket);
        }
    }

    pub fn send_digest(&self, conn: &Connection, bucket: &str) {
        type Row = (String, String, String, String, i64, String);
        let rows: Vec<Row> = conn
            .prepare("SELECT station, code, severity, message, cleared, at FROM notify_queue WHERE bucket = ?1 ORDER BY id")
            .and_then(|mut st| {
                st.query_map([bucket], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })
                .map(|r| r.flatten().collect())
            })
            .unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        // One line per station and code: first and last time and a count, so a flapping condition is one line.
        // The count is of raises; a code that also cleared says how often (`cleared x10`).
        struct Group {
            station: String,
            code: String,
            first: String,
            last: String,
            raised: usize,
            cleared: usize,
            msg: String,
        }
        let mut groups: Vec<Group> = vec![];
        for (station, code, sev, msg, cleared, at) in &rows {
            let i = match groups
                .iter()
                .position(|g| g.station == *station && g.code == *code)
            {
                Some(i) => i,
                None => {
                    groups.push(Group {
                        station: station.clone(),
                        code: code.clone(),
                        first: at.clone(),
                        last: at.clone(),
                        raised: 0,
                        cleared: 0,
                        msg: String::new(),
                    });
                    groups.len() - 1
                }
            };
            let g = &mut groups[i];
            g.last = at.clone();
            if *cleared != 0 {
                g.cleared += 1;
            } else {
                g.raised += 1;
                g.msg = format!("{sev}: {msg}");
            }
        }
        let mut body = String::new();
        for g in &groups {
            let mut counts = String::new();
            if g.raised > 0 || g.cleared == 0 {
                counts.push_str(&format!("x{}", g.raised));
            }
            if g.cleared > 0 {
                if !counts.is_empty() {
                    counts.push_str("  ");
                }
                counts.push_str(&format!("cleared x{}", g.cleared));
            }
            body.push_str(&format!(
                "{}  {}  {counts}  first {}  last {}\n",
                g.station, g.code, g.first, g.last
            ));
            if !g.msg.is_empty() {
                body.push_str(&format!("    {}\n", g.msg));
            }
        }
        let label = if bucket == "daily_digest" {
            "daily digest"
        } else {
            "hourly digest"
        };
        let subject = format!("[{}] {label}: {} event(s)", self.product, rows.len());
        let rule = Rule {
            channels: vec![],
            ..default_rule("digest")
        };
        if self.deliver(conn, &rule, "-", "digest", &subject, &body, "digest") {
            let _ = conn.execute("DELETE FROM notify_queue WHERE bucket = ?1", [bucket]);
        }
    }

    /// The notifier thread's loop.
    pub fn run(mut self, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            if let Some(conn) = open_rw(&self.library_db) {
                let evs = self.poll_stations(&conn);
                for ev in evs {
                    self.handle(&conn, ev);
                }
                self.digests(&conn);
            }
            for _ in 0..20 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// The Send-test button: a short message through the channel, and the server's reply or error.
pub fn test_channel(
    conn: &Connection,
    secrets: &Path,
    product: &str,
    channel_id: i64,
) -> Result<String, String> {
    let (config, name): (String, String) = conn
        .query_row(
            "SELECT config, name FROM notify_channels WHERE id = ?1",
            [channel_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("no such channel")?;
    let cfg = SmtpConfig::from_channel(
        &serde_json::from_str(&config).unwrap_or(Value::Null),
        read_secret(secrets, channel_id),
    )?;
    let r = send_mail(
        &cfg,
        &format!("[{product}] test message"),
        &format!("This is a test message from channel `{name}`. If you can read it, the channel works.\n"),
    );
    let result = match &r {
        Ok(reply) => format!("sent: {reply}"),
        Err(e) => format!("failed: {e}"),
    };
    let _ = conn.execute(
        "UPDATE notify_channels SET last_test_at = ?2, last_test_result = ?3 WHERE id = ?1",
        params![channel_id, now_iso(), result],
    );
    let _ = conn.execute(
        "INSERT INTO notify_log(at, station, code, channel_id, result, kind) VALUES(?1,'-','test',?2,?3,'test')",
        params![now_iso(), channel_id, result],
    );
    r
}

/// What `hotclock notify` does (12.1): send one event through the same channels and rules the notifier uses,
/// without the library service. `OnFailure=` of the units runs it with `service_failed`. A rule that is off, a
/// digest rule or the rate limit decide as they do for the notifier; the result says what happened.
pub fn one_shot(
    library_db: &Path,
    secrets: &Path,
    product: &str,
    code: &str,
    station: &str,
    severity: &str,
    message: &str,
) -> Result<String, String> {
    let conn = open_rw(library_db).ok_or_else(|| {
        format!(
            "cannot open {}: the channels live there",
            library_db.display()
        )
    })?;
    let mut n = Notifier::new(
        library_db.to_path_buf(),
        PathBuf::new(),
        secrets.to_path_buf(),
        product.to_string(),
    );
    n.handle(
        &conn,
        AlertEv {
            station: station.to_string(),
            code: code.to_string(),
            severity: severity.to_string(),
            message: message.to_string(),
            cleared: false,
        },
    );
    // `handle` logs what it did; read the last line back for the caller.
    let last: Option<(String, String)> = conn
        .query_row(
            "SELECT result, kind FROM notify_log WHERE code = ?1 AND station = ?2 ORDER BY id DESC LIMIT 1",
            params![code, station],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .unwrap_or(None);
    let sent = n.sent.load(Ordering::Relaxed);
    match (sent, last) {
        (s, Some((r, _))) if s > 0 => Ok(r),
        (_, Some((r, k))) => Err(format!("not sent ({k}): {r}")),
        _ => Err("not sent: the rule for this event is off, or it waits for a digest".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A fake SMTP server (plain): records the client's lines and the message.
    fn fake_smtp(reply_to_data: &'static str) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            session(s, reply_to_data)
        });
        (port, h)
    }

    /// A fake SMTP server that takes any number of connections; returns the port and the count of messages.
    fn fake_smtp_many() -> (u16, Arc<AtomicU64>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = Arc::new(AtomicU64::new(0));
        let n2 = n.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                session(s, "250 ok\r\n");
                n2.fetch_add(1, Ordering::SeqCst);
            }
        });
        (port, n)
    }

    fn session(s: TcpStream, reply_to_data: &str) -> Vec<String> {
        {
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut got = vec![];
            w.write_all(b"220 fake ESMTP\r\n").unwrap();
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end().to_string();
                got.push(t.clone());
                if in_data {
                    if t == "." {
                        in_data = false;
                        w.write_all(reply_to_data.as_bytes()).unwrap();
                    }
                    continue;
                }
                let up = t.to_ascii_uppercase();
                if up.starts_with("EHLO") {
                    w.write_all(b"250-fake\r\n250 AUTH PLAIN\r\n").unwrap();
                } else if up.starts_with("AUTH") {
                    w.write_all(b"235 ok\r\n").unwrap();
                } else if up.starts_with("MAIL") || up.starts_with("RCPT") {
                    w.write_all(b"250 ok\r\n").unwrap();
                } else if up == "DATA" {
                    in_data = true;
                    w.write_all(b"354 go\r\n").unwrap();
                } else if up == "QUIT" {
                    w.write_all(b"221 bye\r\n").unwrap();
                    break;
                }
            }
            got
        }
    }

    fn cfg(port: u16) -> SmtpConfig {
        SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            tls: "none".into(),
            username: Some("mark".into()),
            password: Some("secret".into()),
            from: "Station <hotclock@example.org>".into(),
            to: vec!["owner@example.org".into()],
            ca_pem: None,
        }
    }

    #[test]
    fn a_message_goes_through_a_plain_server_with_the_right_commands() {
        let (port, h) = fake_smtp("250 queued as 42\r\n");
        let reply = send_mail(&cfg(port), "hello\nBcc: x@y", "line one\n.leading dot\n").unwrap();
        assert_eq!(reply, "250 queued as 42");
        let got = h.join().unwrap();
        assert!(
            got.iter().any(|l| l == "MAIL FROM:<hotclock@example.org>"),
            "{got:?}"
        );
        assert!(got.iter().any(|l| l == "RCPT TO:<owner@example.org>"));
        assert!(got.iter().any(|l| l.starts_with("AUTH PLAIN ")));
        assert!(
            got.iter().any(|l| l == "..leading dot"),
            "dot-stuffed: {got:?}"
        );
        assert!(
            got.iter().any(|l| l == "Subject: hello Bcc: x@y"),
            "no header injection: {got:?}"
        );
    }

    #[test]
    fn a_refusal_comes_back_as_the_servers_words() {
        let (port, h) = fake_smtp("554 5.7.1 rejected\r\n");
        let e = send_mail(&cfg(port), "s", "b").unwrap_err();
        assert!(e.contains("554 5.7.1 rejected"), "{e}");
        let _ = h.join();
    }

    #[test]
    fn an_unreachable_server_is_an_error_not_a_hang() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let t = std::time::Instant::now();
        assert!(send_mail(&cfg(port), "s", "b").is_err());
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn starttls_and_implicit_tls_work_against_a_server_with_a_private_certificate() {
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
        let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let pem = ck.cert.pem();
        let cert = ck.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let sc = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap(),
        );
        for mode in ["starttls", "tls"] {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let sc = sc.clone();
            let h = std::thread::spawn(move || {
                let (s, _) = l.accept().unwrap();
                let mut tcp = s;
                let mut lines = vec![];
                let mut conn: Box<dyn ReadWrite> = if mode == "tls" {
                    let c = rustls::ServerConnection::new(sc.clone()).unwrap();
                    Box::new(rustls::StreamOwned::new(c, tcp))
                } else {
                    tcp.write_all(b"220 fake\r\n").unwrap();
                    // EHLO, offer STARTTLS, then upgrade.
                    let mut r = BufReader::new(tcp.try_clone().unwrap());
                    let mut l1 = String::new();
                    r.read_line(&mut l1).unwrap();
                    tcp.write_all(b"250-fake\r\n250 STARTTLS\r\n").unwrap();
                    let mut l2 = String::new();
                    r.read_line(&mut l2).unwrap();
                    assert!(l2.starts_with("STARTTLS"));
                    tcp.write_all(b"220 go ahead\r\n").unwrap();
                    let c = rustls::ServerConnection::new(sc.clone()).unwrap();
                    Box::new(rustls::StreamOwned::new(c, tcp))
                };
                if mode == "tls" {
                    conn.write_all(b"220 fake\r\n").unwrap();
                }
                let mut r = BufReader::new(conn);
                let mut in_data = false;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let t = line.trim_end().to_string();
                    lines.push(t.clone());
                    let w = r.get_mut();
                    if in_data {
                        if t == "." {
                            in_data = false;
                            w.write_all(b"250 secure ok\r\n").unwrap();
                        }
                        continue;
                    }
                    let up = t.to_ascii_uppercase();
                    if up.starts_with("EHLO") {
                        w.write_all(b"250 fake\r\n").unwrap();
                    } else if up.starts_with("MAIL") || up.starts_with("RCPT") {
                        w.write_all(b"250 ok\r\n").unwrap();
                    } else if up == "DATA" {
                        in_data = true;
                        w.write_all(b"354 go\r\n").unwrap();
                    } else if up == "QUIT" {
                        w.write_all(b"221 bye\r\n").unwrap();
                        break;
                    }
                }
                lines
            });
            let mut c = cfg(port);
            c.tls = mode.into();
            c.username = None;
            c.ca_pem = Some(pem.clone());
            let reply = send_mail(&c, "s", "b").unwrap_or_else(|e| panic!("{mode}: {e}"));
            assert_eq!(reply, "250 secure ok", "{mode}");
            let lines = h.join().unwrap();
            assert!(
                lines.iter().any(|l| l.starts_with("MAIL FROM")),
                "{mode}: {lines:?}"
            );
        }
    }

    trait ReadWrite: Read + Write + Send {}
    impl<T: Read + Write + Send> ReadWrite for T {}

    #[test]
    fn a_certificate_the_client_does_not_trust_is_refused() {
        // With no ca_pem the self-signed certificate must not be accepted.
        use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
        let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let sc = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![ck.cert.der().clone()], key)
                .unwrap(),
        );
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let c = rustls::ServerConnection::new(sc).unwrap();
            let mut t = rustls::StreamOwned::new(c, s);
            let _ = t.write_all(b"220 x\r\n");
        });
        let mut c = cfg(port);
        c.tls = "tls".into();
        c.username = None;
        let e = send_mail(&c, "s", "b").unwrap_err();
        println!("untrusted: {e}");
        let _ = h.join();
    }

    #[test]
    fn a_digest_is_one_line_per_station_and_code_with_a_count_and_is_emptied_when_sent() {
        let d = fixtures::testdir::process_dir().join("digest");
        let _ = std::fs::remove_dir_all(&d);
        let lib = library::Library::open_worker(library::Paths::from_data_dir(&d)).unwrap();
        let (port, h) = fake_smtp("250 digest ok\r\n");
        lib.conn()
            .execute(
                "INSERT INTO notify_channels(name, kind, config, enabled) VALUES('t','smtp',?1,1)",
                [serde_json::json!({"host": "127.0.0.1", "port": port, "tls": "none", "from": "h@example.org", "to": ["m@example.org"]}).to_string()],
            )
            .unwrap();
        for i in 0..3 {
            lib.conn()
                .execute(
                    "INSERT INTO notify_queue(at, station, code, severity, message, cleared, bucket) VALUES(?1,'office','item_missing','warning',?2,0,'hourly_digest')",
                    rusqlite::params![format!("2026-09-30T0{i}:00:00.000Z"), format!("file {i} is missing")],
                )
                .unwrap();
        }
        lib.conn()
            .execute(
                "INSERT INTO notify_queue(at, station, code, severity, message, cleared, bucket) VALUES('2026-09-30T05:00:00.000Z','office','xrun_burst','warning','burst',0,'hourly_digest')",
                [],
            )
            .unwrap();
        let n = Notifier::new(
            d.join("library.db"),
            d.join("stations"),
            d.join("secrets.toml"),
            "Product".into(),
        );
        n.send_digest(lib.conn(), "hourly_digest");
        let got = h.join().unwrap();
        let body = got.join("\n");
        assert!(
            body.contains("Subject: [Product] hourly digest: 4 event(s)"),
            "{body}"
        );
        assert!(body.contains("office  item_missing  x3  first 2026-09-30T00:00:00.000Z  last 2026-09-30T02:00:00.000Z"), "{body}");
        assert!(body.contains("office  xrun_burst  x1"), "{body}");
        let left: i64 = lib
            .conn()
            .query_row("SELECT COUNT(*) FROM notify_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "a sent digest empties its bucket");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_alert_that_flaps_mails_one_raise_and_one_clear_per_window() {
        let d = fixtures::testdir::TempDir::new("notify-flap");
        let lib = library::Library::open_worker(library::Paths::from_data_dir(&d)).unwrap();
        let (port, sent) = fake_smtp_many();
        lib.conn()
            .execute(
                "INSERT INTO notify_channels(name, kind, config, enabled) VALUES('t','smtp',?1,1)",
                [serde_json::json!({"host": "127.0.0.1", "port": port, "tls": "none", "from": "h@example.org", "to": ["m@example.org"]}).to_string()],
            )
            .unwrap();
        let mut n = Notifier::new(
            d.join("library.db"),
            d.join("stations"),
            d.join("secrets.toml"),
            "P".into(),
        );
        for _ in 0..5 {
            for cleared in [false, true] {
                n.handle(
                    lib.conn(),
                    AlertEv {
                        station: "office".into(),
                        code: "silence".into(),
                        severity: "critical".into(),
                        message: "dead air".into(),
                        cleared,
                    },
                );
            }
        }
        assert_eq!(
            sent.load(Ordering::SeqCst),
            2,
            "one raise and one clear mailed"
        );
        let queued: i64 = lib
            .conn()
            .query_row("SELECT COUNT(*) FROM notify_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 8, "the other eight wait for the digest");
    }

    #[test]
    fn the_password_is_not_in_debug_output_and_header_values_are_one_line() {
        let c = cfg(25);
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        let bad = serde_json::json!({"host": "h", "from": "a@b\r\nRCPT TO:<x@y>", "to": ["m@example.org"]});
        assert!(SmtpConfig::from_channel(&bad, None).is_err());
        let bad =
            serde_json::json!({"host": "h", "from": "a@b", "to": ["m@example.org\nBcc: x@y"]});
        assert!(SmtpConfig::from_channel(&bad, None).is_err());
    }

    #[test]
    fn mail_dates_are_rfc_5322() {
        assert_eq!(rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        assert_eq!(
            rfc5322_date(1_790_769_600_000),
            "Wed, 30 Sep 2026 12:00:00 +0000"
        );
        assert_eq!(
            rfc5322_date(951_782_400_000 + 3_723_000),
            "Tue, 29 Feb 2000 01:02:03 +0000"
        );
    }

    #[test]
    fn a_deleted_channel_takes_its_password_with_it() {
        let d = fixtures::testdir::TempDir::new("secret-rm");
        let p = d.join("secrets.toml");
        write_secret(&p, 3, "a").unwrap();
        write_secret(&p, 4, "b").unwrap();
        remove_secret(&p, 3).unwrap();
        assert!(!secret_is_set(&p, 3));
        assert_eq!(read_secret(&p, 4).as_deref(), Some("b"));
    }

    #[test]
    fn default_rules_follow_the_table() {
        assert_eq!(default_rule("silence").delivery, "immediate");
        assert!(default_rule("silence").notify_on_clear);
        assert_eq!(default_rule("item_quarantined").delivery, "immediate");
        assert_eq!(default_rule("item_missing").delivery, "hourly_digest");
        assert_eq!(default_rule("disk_low").delivery, "daily_digest");
        assert_eq!(default_rule("decode_glitch").delivery, "off");
        assert_eq!(default_rule("stream_no_listener").delivery, "immediate");
        let f = default_rule("stream_encoder_failed");
        assert_eq!(
            (f.delivery.as_str(), f.notify_on_clear),
            ("immediate", true)
        );
    }

    #[test]
    fn secrets_are_written_0600_and_read_back() {
        use std::os::unix::fs::PermissionsExt;
        let d = fixtures::testdir::process_dir().join("secret");
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("secrets.toml");
        write_secret(&p, 3, "pa\"ss").unwrap();
        assert_eq!(read_secret(&p, 3).as_deref(), Some("pa\"ss"));
        assert!(!secret_is_set(&p, 4));
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_secret(&p, 4, "x").unwrap();
        assert_eq!(read_secret(&p, 3).as_deref(), Some("pa\"ss"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
