//! The Settings page's routes on the library service (see [`crate::settings`]).

use crate::http::{client, Request, Response};
use crate::library_http::LibraryApi;
use crate::settings::{self as st, KeyError};
use common::config::StationConfig;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn errs_json(e: &[KeyError]) -> Value {
    json!(e
        .iter()
        .map(|k| json!({"key": k.key, "message": k.message}))
        .collect::<Vec<_>>())
}

fn invalid(e: &[KeyError]) -> Response {
    let msg = e
        .iter()
        .map(|k| format!("{}: {}", k.key, k.message))
        .collect::<Vec<_>>()
        .join("; ");
    let mut v = json!({"error": {"code": "invalid_settings", "message": msg}});
    v["errors"] = errs_json(e);
    Response::json(400, &v)
}

impl LibraryApi {
    /// Every `*.toml` of the stations directory, with the station name taken from the file name.
    pub(crate) fn station_files(&self) -> Vec<(String, PathBuf)> {
        let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(&self.opts.stations_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let n = p.file_name()?.to_str()?.strip_suffix(".toml")?.to_string();
                (!n.starts_with('.')).then_some((n, p))
            })
            .collect();
        v.sort();
        v
    }

    fn file_block(&self, path: &Path, scope: &str) -> Value {
        match st::read_doc(path) {
            Ok(doc) => {
                let explicit = st::explicit_values(&doc, scope);
                let parse = st::validate(scope, &doc.to_string(), path).err();
                json!({
                    "file": path.display().to_string(),
                    "writable": st::can_write(path),
                    "values": explicit,
                    "effective": st::effective_values(&explicit, scope),
                    "file_errors": parse.map(|e| errs_json(&e)),
                })
            }
            Err(e) => json!({"file": path.display().to_string(), "writable": false, "error": e}),
        }
    }

    /// `GET /api/v1/settings`.
    pub(crate) fn settings_get(&self) -> Response {
        let playlists: Vec<String> = self
            .lib()
            .and_then(|l| {
                l.playlists()
                    .map_err(|e| Response::error(500, "internal", &e.to_string()))
            })
            .map(|v| v.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        let mut stations = vec![];
        for (name, path) in self.station_files() {
            let mut b = self.file_block(&path, "station");
            b["id"] = json!(name);
            // What the running station says it is using (it may differ until it restarts).
            if let Some(addr) = self.station_addr(&name) {
                if let Ok(r) = client::request(
                    &addr,
                    "GET",
                    "/api/v1/transition-preset",
                    &[],
                    b"",
                    Duration::from_secs(2),
                ) {
                    if r.status == 200 {
                        b["running"] = json!({"transition_preset": r.json()["preset"]});
                    }
                }
            }
            stations.push(b);
        }
        let presets: Vec<Value> = common::transition::BUILTIN_PRESETS
            .iter()
            .map(|(n, _, d)| json!({"name": n, "description": d}))
            .collect();
        let proc = std::env::var("HOTCLOCK_PROC_ASOUND").unwrap_or_else(|_| "/proc/asound".into());
        Response::json(
            200,
            &json!({
                "catalog": {"station": st::catalog_json("station"), "global": st::catalog_json("global")},
                "global": self.file_block(&self.opts.global_config, "global"),
                "stations": stations,
                "playlists": playlists,
                "presets": presets,
                "devices": st::list_devices(Path::new(&proc)),
                "restart_available": std::env::var("HOTCLOCK_SYSTEMCTL").is_ok() || Path::new("/run/systemd/system").exists(),
                "pending": st::pending_keys(),
            }),
        )
    }

    /// `PUT /api/v1/settings/station/{name}` and `PUT /api/v1/settings/global`, body `{"values": {key: value}}`.
    pub(crate) fn settings_put(&self, req: &mut Request, station: Option<&&str>) -> Response {
        let body = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let Some(changes) = body["values"].as_object() else {
            return Response::error(400, "bad_request", "`values` must be an object");
        };
        let (scope, path) = match station {
            Some(n) => match self.station_files().into_iter().find(|f| f.0 == **n) {
                Some((_, p)) => ("station", p),
                None => return Response::error(404, "not_found", &format!("no station `{n}`")),
            },
            None => ("global", self.opts.global_config.clone()),
        };
        let before = match st::read_doc(&path) {
            Ok(d) => d,
            Err(e) => return Response::error(500, "internal", &e),
        };
        let mut doc = before.clone();
        if let Err(e) = st::apply_changes(&mut doc, scope, changes) {
            return invalid(&e);
        }
        let text = doc.to_string();
        if let Err(e) = st::validate(scope, &text, &path) {
            return invalid(&e);
        }
        let changed = st::changed_keys(&before, &doc, scope);
        if changed.is_empty() {
            return Response::json(200, &json!({"changed": [], "saved": false}));
        }
        let all_live = changed
            .iter()
            .all(|(d, _)| d.apply == st::LIVE || d.apply == st::NEXT_BOUNDARY);
        let mut saved = false;
        let mut backup = Value::Null;
        let mut warning = Value::Null;
        let write = || st::write_atomic(&path, &text);
        let written = match station {
            Some(n) => crate::config_history::ConfigHistory::new(
                self.state_dir(n).join("config-history.db"),
                path.clone(),
            )
            .track("station settings", write),
            None => write(),
        };
        match written {
            Ok(b) => {
                saved = true;
                backup = json!(b.display().to_string());
            }
            Err(e) => {
                // A change the station can take while it runs still takes effect when the file cannot be written;
                // it is lost at the next start, and the answer says so.
                if station.is_none() || !all_live {
                    return Response::error(
                        409,
                        "not_writable",
                        &format!("{e}. The service user needs write access to the configuration (see deploy.md, \"Settings\")."),
                    );
                }
                warning = json!(format!(
                    "{e}: the change applies now and is lost at the next start"
                ));
            }
        }
        let mut live_values = Map::new();
        for (d, v) in &changed {
            if d.apply == st::LIVE || d.apply == st::NEXT_BOUNDARY {
                live_values.insert(d.key.to_string(), v.clone());
            }
        }
        let mut live_result = Value::Null;
        if let (Some(n), false) = (station, live_values.is_empty()) {
            live_result = match self.station_addr(n) {
                None => json!({"applied": false, "message": "the station is not configured"}),
                Some(addr) => match client::request(
                    &addr,
                    "POST",
                    "/api/v1/live-settings",
                    &[("content-type", "application/json")],
                    Value::Object(live_values.clone()).to_string().as_bytes(),
                    Duration::from_secs(5),
                ) {
                    Ok(r) if r.status < 300 => json!({"applied": true}),
                    Ok(r) => json!({"applied": false, "message": r.json()["error"]["message"]}),
                    Err(e) => {
                        json!({"applied": false, "message": format!("the station does not answer ({e}); it reads the file at its next start")})
                    }
                },
            };
        }
        // a live key of the global file is read by its user on every request (the box screen's top bar): saved is applied
        let live_applied =
            live_result["applied"].as_bool().unwrap_or(false) || (station.is_none() && saved);
        let items: Vec<Value> = changed
            .iter()
            .map(|(d, v)| {
                let live = d.apply == st::LIVE || d.apply == st::NEXT_BOUNDARY;
                json!({"key": d.key, "value": v, "apply": d.apply, "applied": live && live_applied})
            })
            .collect();
        let keys_of = |a: &str| -> Vec<&str> {
            changed
                .iter()
                .filter(|(d, _)| d.apply == a)
                .map(|(d, _)| d.key)
                .collect()
        };
        Response::json(
            200,
            &json!({
                "saved": saved, "backup": backup, "warning": warning, "changed": items, "live": live_result,
                "station_restart": keys_of(st::STATION_RESTART),
                "library_restart": keys_of(st::LIBRARY_RESTART),
                "display_restart": keys_of(st::DISPLAY_RESTART),
            }),
        )
    }

    /// `POST /api/v1/system/restart`: `{"unit": "station", "name": "office"}` or `{"unit": "library"}`. A station is
    /// restarted and then waited for until its `/health` answers 200 (the check `upgrade.sh restart` makes); the
    /// library restarts after the answer has gone out, since it is the service answering.
    pub(crate) fn system_restart(&self, req: &mut Request) -> Response {
        let body = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        match body["unit"].as_str() {
            Some("library") => {
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(700));
                    if let Err(e) = st::restart_unit("hotclock-library.service") {
                        tracing::error!("{e}");
                    }
                });
                Response::json(
                    202,
                    &json!({"unit": "hotclock-library.service", "restarting": true, "note": "this page reconnects when the service is back"}),
                )
            }
            Some("station") => {
                let Some(name) = body["name"].as_str() else {
                    return Response::error(400, "bad_request", "`name` is required");
                };
                // Only a configured station's name ever reaches the command line.
                let Some(addr) = self.station_addr(name).filter(|_| {
                    StationConfig::load(&self.opts.stations_dir.join(format!("{name}.toml")))
                        .is_ok()
                }) else {
                    return Response::error(404, "not_found", &format!("no station `{name}`"));
                };
                let unit = format!("hotclock-station@{name}.service");
                if let Err(e) = st::restart_unit(&unit) {
                    return Response::error(502, "restart_failed", &e);
                }
                let t = Instant::now();
                let mut last = 0;
                while t.elapsed() < Duration::from_secs(30) {
                    if let Ok(r) =
                        client::request(&addr, "GET", "/health", &[], b"", Duration::from_secs(2))
                    {
                        last = r.status;
                        if r.status == 200 {
                            return Response::json(
                                200,
                                &json!({"unit": unit, "healthy": true, "seconds": t.elapsed().as_secs_f64()}),
                            );
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Response::json(
                    504,
                    &json!({"error": {"code": "unhealthy", "message": format!("{unit} restarted but /health did not answer 200 within 30 s (last status {last})")}}),
                )
            }
            Some("display") => {
                // exactly one more unit than the library and the stations; the display is a client and nothing waits on it
                let unit = "hotclock-display.service";
                if let Err(e) = st::restart_unit(unit) {
                    return Response::error(502, "restart_failed", &e);
                }
                Response::json(200, &json!({"unit": unit, "restarted": true}))
            }
            _ => Response::error(
                400,
                "bad_request",
                "`unit` is \"station\", \"library\" or \"display\"",
            ),
        }
    }
}
