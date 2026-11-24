//! The status display's night color on the library service's side: what the Settings page shows (the look now,
//! today's sunrise and sunset, the next change), the `display_night` message that carries a save to the display at
//! once over the `display` topic, the Preview button, and the opt-in internet lookup of the box's location.
//!
//! The look itself is computed by `common::night`, the same code the display draws with, so the page says what the
//! panel shows. Nothing here touches the web pages' own colors or the stream.

use crate::http::{Request, Response};
use crate::library_http::LibraryApi;
use crate::ws::Hub;
use common::config::GlobalConfig;
use common::night::{self, NightSettings, Phase, Place, SunDay};
use common::timespec::Zone;
use serde_json::{json, Value};
use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The internet lookup is made at most once a day.
pub const LOOKUP_EVERY_MS: i64 = 86_400_000;
/// The service asked, unless `HOTCLOCK_GEOIP_URL` names another (the tests' local server). It answers
/// `{"latitude": .., "longitude": .., "city": ..}` for the address the request comes from.
pub const GEOIP_URL: &str = "https://ipapi.co/json/";

fn wall_ms() -> i64 {
    crate::config_history::wall_ms()
}

/// Where the internet lookup's answer is kept: beside the library database.
fn location_cache(db: &Path) -> PathBuf {
    db.with_file_name("display-location.json")
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// `HH:MM` local.
fn hhmm(zone: &Zone, ms: i64) -> String {
    let c = zone.civil(ms);
    format!("{:02}:{:02}", c.hour(), c.minute())
}

/// `today`, `tomorrow`, or the date.
fn day_word(zone: &Zone, now: i64, ms: i64) -> String {
    let (a, b) = (zone.civil(now).date(), zone.civil(ms).date());
    match (b - a).get_days() {
        0 => "today".into(),
        1 => "tomorrow".into(),
        _ => b.to_string(),
    }
}

/// The night color as the library service sees it now: the `night_*` keys of the global file, the zone, the place,
/// the look, today's sun and the next change. The display reads `settings`, `place` and `error` of it.
pub fn night_status(global: &Path, db: &Path, hub: Option<&Arc<Hub>>, now: i64) -> Value {
    let d = GlobalConfig::load_or_default(global)
        .map(|g| g.display)
        .unwrap_or_default();
    let s = NightSettings::from_config(&d);
    let (zone, zone_err) = match s.zone() {
        Ok(z) => (z, None),
        Err(e) => (Zone::utc(), Some(e)),
    };
    let cache = read_json(&location_cache(db));
    let internet = cache.as_ref().and_then(|c| {
        Some(Place {
            lat: c["lat"].as_f64()?,
            lon: c["lon"].as_f64()?,
            from: match c["city"].as_str() {
                Some(city) if !city.is_empty() => format!("internet ({city})"),
                _ => "internet".into(),
            },
        })
    });
    if s.location == night::LocationSource::Internet {
        lookup_if_due(global, db, hub, cache.as_ref(), now);
    }
    let place = night::resolve_place(&s, &zone, internet.as_ref());
    let st = night::evaluate(&s, place.as_ref().ok(), &zone, now, None);
    let today = match st.today {
        Some(SunDay::Normal { rise, set }) => json!({
            "sunrise_ms": rise, "sunset_ms": set, "sunrise": hhmm(&zone, rise), "sunset": hhmm(&zone, set), "polar": null,
        }),
        Some(SunDay::AlwaysUp) => json!({"polar": "up", "text": "the sun does not set today"}),
        Some(SunDay::AlwaysDown) => json!({"polar": "down", "text": "the sun does not rise today"}),
        None => Value::Null,
    };
    let next = st.next.map(|(at, ph)| {
        let when = format!("{} {}", hhmm(&zone, at), day_word(&zone, now, at));
        let k = s.kelvin.round();
        let text = match ph {
            Phase::Warming => format!("Warming starts at {when}"),
            Phase::Night => format!("Night ({k:.0} K) from {when}"),
            Phase::Cooling => format!("Back to day starts at {when}"),
            _ => format!("Day (6500 K) from {when}"),
        };
        json!({"at_ms": at, "at": hhmm(&zone, at), "phase": ph.name(), "text": text})
    });
    // a place is needed for `sun` only; a bad zone matters to `sun` and `schedule`
    let error = match s.mode {
        night::Mode::Off => None,
        night::Mode::Schedule => zone_err,
        night::Mode::Sun => zone_err.or_else(|| place.as_ref().err().cloned()),
    };
    json!({
        "settings": NightSettings::config_json(&d),
        "zone": zone.name(),
        "place": place.as_ref().ok().map(Place::to_json),
        "error": error,
        "state": {
            "phase": st.phase.name(), "text": st.text(), "kelvin": st.kelvin.round(),
            "dim_percent": (st.dim * 100.0).round(), "progress": st.progress,
        },
        "today": today,
        "next": next,
        "internet": cache.map(|c| json!({"at_ms": c["at_ms"], "attempt_ms": c["attempt_ms"], "error": c["error"]})),
        "preview_s": night::PREVIEW_S,
        // what white becomes on the panel at each temperature (every 100 K), for the page's swatch: the display's own table
        "whites": (19..=65).map(|h| {
            let (r, g, b) = night::Lut::for_look(f64::from(h * 100), 0.0).map(255, 255, 255);
            json!([h * 100, r, g, b])
        }).collect::<Vec<_>>(),
    })
}

/// Tell the display (and anyone on the `display` topic) the night settings now.
pub fn send_night(hub: &Arc<Hub>, global: &Path, db: &Path) {
    let v = night_status(global, db, Some(hub), wall_ms());
    hub.send_with("display", &|seq| {
        json!({"v": 1, "type": "display_night", "seq": seq, "data": v}).to_string()
    });
}

static LOOKING: AtomicBool = AtomicBool::new(false);

/// Start the internet lookup in the background when the last attempt is a day old (or there was none). The answer is
/// kept in `display-location.json` and sent to the display; a failure is kept too (no second try that day), and the
/// time zone stands in.
fn lookup_if_due(
    global: &Path,
    db: &Path,
    hub: Option<&Arc<Hub>>,
    cache: Option<&Value>,
    now: i64,
) {
    let last = cache.and_then(|c| c["attempt_ms"].as_i64()).unwrap_or(0);
    if now - last < LOOKUP_EVERY_MS || LOOKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (global, db, hub, prev) = (
        global.to_path_buf(),
        db.to_path_buf(),
        hub.cloned(),
        cache.cloned().unwrap_or_else(|| json!({})),
    );
    let spawned = std::thread::Builder::new()
        .name("night-geoip".into())
        .spawn(move || {
            let url = std::env::var("HOTCLOCK_GEOIP_URL").unwrap_or_else(|_| GEOIP_URL.into());
            let mut out = prev;
            out["attempt_ms"] = json!(wall_ms());
            match lookup_retrying(&url, retry_delay()) {
                Ok((lat, lon, city)) => {
                    tracing::info!("night color: the internet places this box at {lat:.2}, {lon:.2} ({city})");
                    out["lat"] = json!(lat);
                    out["lon"] = json!(lon);
                    out["city"] = json!(city);
                    out["at_ms"] = json!(wall_ms());
                    out["error"] = Value::Null;
                }
                Err(e) => {
                    tracing::warn!("night color: the internet lookup failed: {e}; using the time zone until tomorrow");
                    out["error"] = json!(e);
                }
            }
            let p = location_cache(&db);
            let tmp = p.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, out.to_string()).and_then(|()| std::fs::rename(&tmp, &p)) {
                tracing::warn!("night color: cannot write {}: {e}", p.display());
            }
            LOOKING.store(false, Ordering::SeqCst);
            if let Some(h) = hub {
                send_night(&h, &global, &db);
            }
        });
    if spawned.is_err() {
        LOOKING.store(false, Ordering::SeqCst);
    }
}

/// How long to wait before the one retry; `HOTCLOCK_GEOIP_RETRY_MS` replaces it (the tests).
fn retry_delay() -> Duration {
    let ms = std::env::var("HOTCLOCK_GEOIP_RETRY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5_000);
    Duration::from_millis(ms)
}

/// Why a lookup failed. A connection or I/O error may be a blip, so it earns one more try; a bad answer will not mend.
struct Fail {
    retry: bool,
    msg: String,
}

impl From<String> for Fail {
    fn from(msg: String) -> Self {
        Fail { retry: true, msg }
    }
}

impl Fail {
    fn bad(msg: String) -> Self {
        Fail { retry: false, msg }
    }
}

/// `lookup`, and once more after `delay` when the first try failed on the connection or the wire.
fn lookup_retrying(url: &str, delay: Duration) -> Result<(f64, f64, String), String> {
    match lookup_once(url) {
        Err(f) if f.retry => {
            tracing::info!(
                "night color: the internet lookup failed ({}); trying once more",
                f.msg
            );
            std::thread::sleep(delay);
            lookup_once(url).map_err(|f| f.msg)
        }
        r => r.map_err(|f| f.msg),
    }
}

/// `GET url` (http or https, the system's usual roots) and read latitude, longitude and city from the JSON answer
/// (`latitude`/`longitude`, or `lat`/`lon`, or `loc` as `"lat,lon"`).
pub fn lookup(url: &str) -> Result<(f64, f64, String), String> {
    lookup_once(url).map_err(|f| f.msg)
}

fn lookup_once(url: &str) -> Result<(f64, f64, String), Fail> {
    let body = http_get(url, Duration::from_secs(8))?;
    let v: Value = serde_json::from_slice(&body)
        .map_err(|e| Fail::bad(format!("the answer is not JSON: {e}")))?;
    let num = |k: &str| {
        v[k].as_f64()
            .or_else(|| v[k].as_str().and_then(|s| s.parse().ok()))
    };
    let (lat, lon) = match (
        num("latitude").or_else(|| num("lat")),
        num("longitude").or_else(|| num("lon")),
    ) {
        (Some(a), Some(b)) => (a, b),
        _ => v["loc"]
            .as_str()
            .and_then(|l| l.split_once(','))
            .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
            .ok_or_else(|| Fail::bad("the answer has no latitude and longitude".into()))?,
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(Fail::bad(format!(
            "the answer's coordinates are out of range: {lat}, {lon}"
        )));
    }
    Ok((lat, lon, v["city"].as_str().unwrap_or("").to_string()))
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

/// One GET, `Connection: close`; the body (de-chunked).
fn http_get(url: &str, timeout: Duration) -> Result<Vec<u8>, Fail> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(Fail::bad(format!(
            "`{url}` is not an http or https address"
        )));
    };
    let (hostport, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => {
            (h.to_string(), p.parse::<u16>().unwrap_or(443))
        }
        _ => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("{host}: {e}"))?;
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();
    let mut conn = if tls {
        use rustls::pki_types::ServerName;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let conf = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = ServerName::try_from(host.clone()).map_err(|e| format!("server name: {e}"))?;
        let c = rustls::ClientConnection::new(Arc::new(conf), name).map_err(|e| e.to_string())?;
        Conn::Tls(Box::new(rustls::StreamOwned::new(c, tcp)))
    } else {
        Conn::Plain(tcp)
    };
    // one write: the request in several segments let a server that closes early reset the connection
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: hotclock-display-night\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    conn.write_all(req.as_bytes())
        .and_then(|()| conn.flush())
        .map_err(|e| format!("{host}: {e}"))?;
    let r = crate::http::client::read_reply(&mut BufReader::new(conn))
        .map_err(|e| format!("{host}: {e}"))?;
    if r.status != 200 {
        return Err(Fail::bad(format!("{host} answered {}", r.status)));
    }
    if r.header("transfer-encoding")
        .is_some_and(|t| t.eq_ignore_ascii_case("chunked"))
    {
        return dechunk(&r.body).map_err(Fail::bad);
    }
    Ok(r.body)
}

fn dechunk(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let eol = b[i..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk")?
            + i;
        let size = String::from_utf8_lossy(&b[i..eol]);
        let n = usize::from_str_radix(size.split(';').next().unwrap_or("0").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        if n == 0 {
            break;
        }
        let start = eol + 2;
        out.extend_from_slice(b.get(start..start + n).ok_or("short chunk")?);
        i = start + n + 2;
    }
    Ok(out)
}

impl LibraryApi {
    /// `GET /api/v1/display/night`: what the Settings page shows beside the night keys.
    pub(crate) fn night_get(&self) -> Response {
        Response::json(
            200,
            &night_status(
                &self.opts.global_config,
                &self.opts.paths.db,
                Some(&self.hub),
                wall_ms(),
            ),
        )
    }

    /// The `night` part of the `display` topic's snapshot.
    pub(crate) fn night_snapshot(&self) -> Value {
        night_status(
            &self.opts.global_config,
            &self.opts.paths.db,
            Some(&self.hub),
            wall_ms(),
        )
    }

    /// After a save of the global file that changed a `display.night_*` key: the display follows at once.
    pub(crate) fn night_changed(&self) {
        send_night(&self.hub, &self.opts.global_config, &self.opts.paths.db);
    }

    /// `POST /api/v1/display/night/preview`: the night look on the panel for 15 s, then back. The body may give
    /// `kelvin` (1900 to 6500) and `dim_percent` (0 to 50), the values on the page before they are saved; otherwise
    /// the saved ones.
    pub(crate) fn night_preview(&self, req: &mut Request) -> Response {
        let b = match req.json() {
            Ok(b) => b,
            Err(_) => json!({}),
        };
        let k = b["kelvin"].as_f64();
        let d = b["dim_percent"].as_f64();
        if k.is_some_and(|k| !(night::MIN_K..=night::MAX_K).contains(&k)) {
            return Response::error(422, "invalid", "kelvin is 1900 to 6500");
        }
        if d.is_some_and(|d| !(0.0..=50.0).contains(&d)) {
            return Response::error(422, "invalid", "dim_percent is 0 to 50");
        }
        let until = wall_ms() + night::PREVIEW_S * 1000;
        let data = json!({"until_ms": until, "kelvin": k, "dim_percent": d});
        self.hub.send_with("display", &|seq| {
            json!({"v": 1, "type": "display_night_preview", "seq": seq, "data": data}).to_string()
        });
        Response::json(
            200,
            &json!({"until_ms": until, "seconds": night::PREVIEW_S, "kelvin": k, "dim_percent": d}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_answers_are_joined() {
        assert_eq!(
            dechunk(b"4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n").unwrap(),
            b"{\"a\":1}"
        );
        assert!(dechunk(b"zz\r\n").is_err());
    }

    #[test]
    fn a_non_http_address_is_refused() {
        assert!(lookup("ftp://example.org/").is_err());
    }
}
