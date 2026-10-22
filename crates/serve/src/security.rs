//! The three friction-free protections of architecture 13, applied to every request before any handler:
//!
//! 1. a mutating request must carry `Content-Type: application/json` (`415` otherwise), which a plain
//!    HTML form cannot send and which makes a script's cross-origin request need a CORS preflight;
//! 2. CORS answers only for the box's own origins;
//! 3. the `Host` header must be a name this box answers to (`421` otherwise), which defeats DNS rebinding.
//!
//! The LAN and the tailnet are trusted: there is no login. A browser tab on the owner's own machine is the one
//! attacker these rules are for.

use crate::http::Request;
use crate::http::Response;
use std::net::IpAddr;

#[derive(Debug, Clone, Default)]
pub struct Security {
    /// Names (lowercase) this box answers to besides `localhost` and IP literals: its host name and any
    /// extra names from the config.
    pub hosts: Vec<String>,
    /// The ports of this box's own Hotclock listeners: an `Origin` counts as ours only on one of these.
    pub own_ports: Vec<u16>,
}

pub enum Decision {
    Allow {
        /// Send `Access-Control-Allow-Origin` with this value.
        cors_origin: Option<String>,
    },
    Refuse(Response),
}

/// The machine's host name (`hostname`, or `/etc/hostname`).
pub fn local_hostname() -> Option<String> {
    let from_cmd = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_ascii_lowercase());
    from_cmd
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_ascii_lowercase())
        })
        .filter(|s| !s.is_empty())
}

/// `host:port` or `[v6]:port` split into the host and the port.
fn split_host_port(h: &str) -> (String, Option<u16>) {
    if let Some(rest) = h.strip_prefix('[') {
        if let Some((addr, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (addr.to_ascii_lowercase(), port);
        }
    }
    match h.rsplit_once(':') {
        Some((a, p)) if p.chars().all(|c| c.is_ascii_digit()) && !a.contains(':') => {
            (a.to_ascii_lowercase(), p.parse().ok())
        }
        _ => (h.to_ascii_lowercase(), None),
    }
}

impl Security {
    pub fn new(extra_hosts: &[String], own_ports: Vec<u16>) -> Self {
        let mut hosts: Vec<String> = extra_hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        if let Some(h) = local_hostname() {
            hosts.push(h);
        }
        Self { hosts, own_ports }
    }

    /// Does this box answer to `host` (no port)? IP literals always: DNS rebinding needs a *name*, so a
    /// Host header that is an address cannot be one.
    pub fn host_ok(&self, host: &str) -> bool {
        if host == "localhost" || host.ends_with(".localhost") {
            return true;
        }
        if host.parse::<IpAddr>().is_ok() {
            return true;
        }
        // The box's own name, its `.local` form and its tailnet name (`name.<tailnet>.ts.net`). Never
        // `name.<anything else>`: an attacker can register `radiobox.evil.example` for a rebinding page.
        self.hosts.iter().any(|h| {
            let short = h.split('.').next().unwrap_or(h);
            host == h
                || host == short
                || host == format!("{short}.local")
                || (host.ends_with(".ts.net")
                    && host.split('.').count() == 4
                    && host.split('.').next() == Some(short))
        })
    }

    /// Is `origin` (`http://host:port`) one of this box's own?
    pub fn origin_ok(&self, origin: &str) -> bool {
        let Some((scheme, rest)) = origin.split_once("://") else {
            return false;
        };
        if scheme != "http" && scheme != "https" {
            return false;
        }
        let (host, port) = split_host_port(rest);
        let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
        self.host_ok(&host) && self.own_ports.contains(&port)
    }

    pub fn check(&self, req: &Request) -> Decision {
        let Some(host_header) = req.header("host") else {
            return Decision::Refuse(Response::error(
                400,
                "bad_request",
                "the Host header is required",
            ));
        };
        let (host, _) = split_host_port(host_header);
        if !self.host_ok(&host) {
            return Decision::Refuse(Response::error(
                421,
                "misdirected",
                "this station does not answer to that host name",
            ));
        }
        let mut cors_origin = None;
        if let Some(origin) = req.header("origin") {
            if self.origin_ok(origin) {
                cors_origin = Some(origin.to_string());
            } else {
                // A foreign page in a browser. A read (`GET`) is answered without CORS headers, so the
                // browser will not show the page the answer; a preflight or anything that changes state,
                // or a WebSocket, is refused outright.
                let readonly = matches!(req.method.as_str(), "GET" | "HEAD");
                let is_ws = req
                    .header("upgrade")
                    .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
                if !readonly || is_ws {
                    return Decision::Refuse(Response::error(
                        403,
                        "forbidden_origin",
                        "cross-origin requests are not allowed",
                    ));
                }
            }
        }
        if req.method == "OPTIONS" {
            // A preflight from one of our own origins.
            let mut r = Response::new(204);
            if let Some(o) = &cors_origin {
                r = r
                    .with_header("access-control-allow-origin", o)
                    .with_header(
                        "access-control-allow-methods",
                        "GET, POST, PUT, PATCH, DELETE, OPTIONS",
                    )
                    .with_header(
                        "access-control-allow-headers",
                        "content-type, x-requested-with",
                    )
                    .with_header("access-control-max-age", "600")
                    .with_header("vary", "Origin");
            }
            return Decision::Refuse(r);
        }
        if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
            let ct = req
                .header("content-type")
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let upload = upload_kind(&req.method, &req.path);
            let ok = match upload {
                // An upload a form could send (multipart) needs a header a form cannot set.
                Some(Upload::Multipart) => {
                    ct == "multipart/form-data" && req.header("x-requested-with").is_some()
                }
                Some(Upload::Image) => matches!(ct.as_str(), "image/jpeg" | "image/png"),
                None => ct == "application/json",
            };
            if !ok {
                let want = match upload {
                    Some(Upload::Multipart) => {
                        "multipart/form-data with an X-Requested-With header"
                    }
                    Some(Upload::Image) => "image/jpeg or image/png",
                    None => "application/json",
                };
                return Decision::Refuse(Response::error(
                    415,
                    "unsupported_media_type",
                    &format!("requests that change state must send Content-Type: {want}"),
                ));
            }
        }
        Decision::Allow { cors_origin }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Upload {
    Multipart,
    Image,
}

fn upload_kind(method: &str, path: &str) -> Option<Upload> {
    let p = path.trim_end_matches('/');
    if method == "POST" && (p == "/api/v1/imports" || p.ends_with("/api/v1/imports")) {
        return Some(Upload::Multipart);
    }
    if method == "PUT" && p.contains("/items/") && p.ends_with("/art") {
        return Some(Upload::Image);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec() -> Security {
        Security {
            hosts: vec!["radiobox".into()],
            own_ports: vec![8300, 8301],
        }
    }

    #[test]
    fn host_rules() {
        let s = sec();
        assert!(s.host_ok("localhost"));
        assert!(s.host_ok("127.0.0.1"));
        assert!(s.host_ok("192.0.2.30"));
        assert!(s.host_ok("::1"));
        assert!(s.host_ok("radiobox"));
        assert!(s.host_ok("radiobox.local"));
        assert!(s.host_ok("radiobox.tail1234.ts.net"));
        assert!(!s.host_ok("evil.example"));
        assert!(s.host_ok("radiobox.tail1234.ts.net"));
        assert!(!s.host_ok("radiobox.evil.example"));
        assert!(!s.host_ok("notradiobox"));
        assert!(!s.host_ok("radiobox.evil.example.ts.net"));
    }

    #[test]
    fn origin_rules() {
        let s = sec();
        assert!(s.origin_ok("http://radiobox:8300"));
        assert!(s.origin_ok("http://127.0.0.1:8301"));
        assert!(s.origin_ok("http://localhost:8300"));
        assert!(!s.origin_ok("http://localhost:9999"));
        assert!(!s.origin_ok("http://evil.example:8300"));
        assert!(!s.origin_ok("null"));
        assert!(!s.origin_ok("file://"));
    }

    #[test]
    fn split() {
        assert_eq!(split_host_port("a.b:80"), ("a.b".into(), Some(80)));
        assert_eq!(split_host_port("[::1]:8300"), ("::1".into(), Some(8300)));
        assert_eq!(split_host_port("Studio"), ("studio".into(), None));
    }
}
