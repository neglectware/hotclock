//! The Plex tools in the web UI (M2): the same functions the `hotclock import-plex` command runs
//! (`library::plex`), behind `/api/v1/plex`. The token stays a file on the box: the page sets its path and is
//! only ever told whether it is set and readable, never the path or the token. Imports and fills can take
//! minutes, so they run as jobs the page polls; one at a time.

use crate::http::{Request, Response};
use crate::library_http::LibraryApi;
use library::playlists::Mode;
use library::plex::{self, FillScope, PlexClient, PlexImportOptions, Sink};
use library::Library;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const DEFAULT_SERVER: &str = "http://192.0.2.3:32400";
const MAX_LINES: usize = 3000;
const KEEP_JOBS: usize = 12;

pub struct PlexJob {
    id: u64,
    kind: &'static str,
    label: String,
    dry_run: bool,
    state: &'static str,
    lines: Vec<String>,
    dropped: usize,
    result: Value,
    error: Option<String>,
    started: String,
}

#[derive(Default)]
pub struct PlexJobs {
    next: u64,
    jobs: Vec<Arc<Mutex<PlexJob>>>,
}

fn job_json(j: &PlexJob, with_lines: bool) -> Value {
    let mut v = json!({"id": j.id, "kind": j.kind, "label": j.label, "dry_run": j.dry_run, "state": j.state,
                       "started": j.started, "result": j.result, "error": j.error,
                       "line_count": j.lines.len() + j.dropped});
    if with_lines {
        v["lines"] = json!(j.lines);
        v["lines_dropped"] = json!(j.dropped);
    }
    v
}

fn bad(m: &str) -> Response {
    Response::error(400, "bad_request", m)
}

impl LibraryApi {
    fn plex_get(&self, c: &rusqlite::Connection, key: &str) -> Option<String> {
        c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .unwrap_or(None)
    }

    /// The configured server and a client with the token read from its file; `Err` is a message for the page.
    fn plex_client(&self, c: &rusqlite::Connection) -> Result<PlexClient, String> {
        let server = self
            .plex_get(c, "plex_server")
            .unwrap_or_else(|| DEFAULT_SERVER.into());
        let Some(path) = self
            .plex_get(c, "plex_token_file")
            .filter(|p| !p.is_empty())
        else {
            return Err(
                "no Plex token file is set: give the path of the file that holds the token".into(),
            );
        };
        let token = plex::read_token_file(&PathBuf::from(path))
            .map_err(|_| "the Plex token file cannot be read, or it is empty".to_string())?;
        PlexClient::new(&server, token).map_err(|e| e.to_string())
    }

    fn plex_settings(&self, c: &rusqlite::Connection) -> Response {
        let path = self
            .plex_get(c, "plex_token_file")
            .filter(|p| !p.is_empty());
        let ok = path
            .as_ref()
            .is_some_and(|p| plex::read_token_file(&PathBuf::from(p)).is_ok());
        Response::json(
            200,
            &json!({"server": self.plex_get(c, "plex_server").unwrap_or_else(|| DEFAULT_SERVER.into()),
                    "token_file_set": path.is_some(), "token_ok": ok}),
        )
    }

    pub(crate) fn plex(&self, req: &mut Request, segs: &[&str]) -> Response {
        let lib = match self.lib() {
            Ok(l) => l,
            Err(r) => return r,
        };
        let c = lib.conn();
        let method = req.method.clone();
        match (method.as_str(), segs) {
            ("GET", []) => self.plex_settings(c),
            ("PUT", []) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                if let Some(s) = b.get("server").and_then(Value::as_str) {
                    if let Err(e) = PlexClient::new(s, String::new()) {
                        return Response::error(422, "invalid", &e.to_string());
                    }
                    let _ = c.execute(
                        "INSERT INTO meta(key, value) VALUES('plex_server', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1",
                        [s.trim()],
                    );
                }
                if let Some(p) = b.get("token_file").and_then(Value::as_str) {
                    if p.is_empty() {
                        let _ = c.execute("DELETE FROM meta WHERE key = 'plex_token_file'", []);
                    } else {
                        if !p.starts_with('/') {
                            return Response::error(
                                422,
                                "invalid",
                                "give the token file's full path (it starts with /)",
                            );
                        }
                        if plex::read_token_file(&PathBuf::from(p)).is_err() {
                            return Response::error(422, "invalid", "that file cannot be read by this service, or it is empty (the token goes alone on its first line)");
                        }
                        let _ = c.execute(
                            "INSERT INTO meta(key, value) VALUES('plex_token_file', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1",
                            [p],
                        );
                    }
                }
                self.audit(c, req, "plex_settings", "-", &json!({"server": b.get("server"), "token_file_changed": b.get("token_file").is_some()}));
                self.plex_settings(c)
            }
            ("GET", ["playlists"]) => {
                let client = match self.plex_client(c) {
                    Ok(p) => p,
                    Err(e) => return Response::error(422, "invalid", &e),
                };
                match client.playlists() {
                    Ok(l) => Response::json(
                        200,
                        &json!({"playlists": l.iter().map(|p| json!({"title": p.title, "tracks": p.leaf_count,
                            "minutes": (p.duration_ms as f64 / 60000.0).round(), "smart": p.smart,
                            "in_library": lib.playlist_id(&p.title).ok().flatten().is_some()})).collect::<Vec<_>>()}),
                    ),
                    Err(e) => Response::error(502, "plex", &e.to_string()),
                }
            }
            ("POST", ["import"]) | ("POST", ["fill-art"]) => {
                let b = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r,
                };
                let fill = segs[0] == "fill-art";
                let dry_run = b["dry_run"].as_bool().unwrap_or(false);
                let client = match self.plex_client(c) {
                    Ok(p) => p,
                    Err(e) => return Response::error(422, "invalid", &e),
                };
                let paths = self.opts.paths.clone();
                if fill {
                    let scope = match (b["playlist"].as_str(), b["all"].as_bool()) {
                        (Some(p), _) if !p.is_empty() => FillScope::Playlist(p.to_string()),
                        (_, Some(true)) => FillScope::AllPlexItems,
                        _ => return bad("name a playlist, or set all to true"),
                    };
                    let label = match &scope {
                        FillScope::Playlist(p) => format!("fill missing artwork: {p}"),
                        FillScope::AllPlexItems => "fill missing artwork: every Plex item".into(),
                    };
                    self.plex_job("fill-art", label, dry_run, move |sink| {
                        let lib = Library::open_worker(paths).map_err(|e| e.to_string())?;
                        let st = plex::fill_art(&lib, &client, &scope, dry_run, sink)
                            .map_err(|e| e.to_string())?;
                        Ok(serde_json::to_value(&st).unwrap_or(Value::Null))
                    })
                } else {
                    let Some(name) = b["playlist"].as_str().filter(|p| !p.is_empty()) else {
                        return bad("name the playlist");
                    };
                    let mode = match b["mode"].as_str().unwrap_or("sequential") {
                        "shuffle" => Mode::Shuffle,
                        "sequential" => Mode::Sequential,
                        _ => return bad("mode is sequential or shuffle"),
                    };
                    let name = name.to_string();
                    let opts_base = (
                        b["as"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        b["category"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        b["fill_art"].as_bool().unwrap_or(true),
                    );
                    self.plex_job("import", format!("import: {name}"), dry_run, move |sink| {
                        let lib = Library::open_worker(paths).map_err(|e| e.to_string())?;
                        let rep = plex::import_playlist(
                            &lib,
                            &client,
                            &name,
                            &PlexImportOptions {
                                category: opts_base.1,
                                mode,
                                as_name: opts_base.0,
                                dry_run,
                                fill_art: opts_base.2,
                                on_line: sink.clone(),
                            },
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(json!({"playlist": rep.playlist, "tracks": rep.tracks, "downloaded": rep.downloaded,
                                  "imported": rep.imported, "known": rep.known, "failed": rep.failed,
                                  "playlist_created": rep.playlist_created, "entries_added": rep.entries_added,
                                  "entries_present": rep.entries_present, "metadata_filled": rep.metadata_filled,
                                  "skipped": rep.skipped, "art": rep.art}))
                    })
                }
            }
            ("GET", ["jobs"]) => {
                let g = self.plex_jobs.lock().unwrap();
                Response::json(
                    200,
                    &json!({"jobs": g.jobs.iter().rev().map(|j| job_json(&j.lock().unwrap(), false)).collect::<Vec<_>>()}),
                )
            }
            ("GET", ["jobs", id]) => {
                let g = self.plex_jobs.lock().unwrap();
                match id
                    .parse::<u64>()
                    .ok()
                    .and_then(|n| g.jobs.iter().find(|j| j.lock().unwrap().id == n))
                {
                    Some(j) => Response::json(200, &job_json(&j.lock().unwrap(), true)),
                    None => Response::error(404, "not_found", "no such job"),
                }
            }
            _ => Response::error(405, "method_not_allowed", "not allowed here"),
        }
    }

    /// Start `f` on its own thread as a job; one runs at a time.
    fn plex_job(
        &self,
        kind: &'static str,
        label: String,
        dry_run: bool,
        f: impl FnOnce(&Sink) -> Result<Value, String> + Send + 'static,
    ) -> Response {
        let job = {
            let mut g = self.plex_jobs.lock().unwrap();
            if g.jobs.iter().any(|j| j.lock().unwrap().state == "running") {
                return Response::error(
                    409,
                    "conflict",
                    "a Plex job is already running; wait for it to finish",
                );
            }
            g.next += 1;
            let j = Arc::new(Mutex::new(PlexJob {
                id: g.next,
                kind,
                label,
                dry_run,
                state: "running",
                lines: vec![],
                dropped: 0,
                result: Value::Null,
                error: None,
                started: library::util::now_iso(),
            }));
            g.jobs.push(j.clone());
            while g.jobs.len() > KEEP_JOBS {
                g.jobs.remove(0);
            }
            j
        };
        let id = job.lock().unwrap().id;
        let jl = job.clone();
        let sink = Sink(Some(Arc::new(move |l: &str| {
            let mut j = jl.lock().unwrap();
            if j.lines.len() < MAX_LINES {
                j.lines.push(l.to_string());
            } else {
                j.dropped += 1;
            }
        })));
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&sink)));
            let mut j = job.lock().unwrap();
            match r {
                Ok(Ok(v)) => {
                    j.result = v;
                    j.state = "done";
                }
                Ok(Err(e)) => {
                    j.error = Some(e);
                    j.state = "failed";
                }
                Err(_) => {
                    j.error = Some("the job stopped unexpectedly".into());
                    j.state = "failed";
                }
            }
        });
        Response::json(202, &json!({"job": id}))
    }
}
