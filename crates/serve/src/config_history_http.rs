//! `GET /api/v1/config-history`, `GET /api/v1/config-history/{id}` and `POST /api/v1/config-history/{id}/restore`:
//! the station's configuration history (see [`crate::config_history`]) and the restore, which writes an earlier
//! version of the file back through the same route as any other save and starts what can start while the
//! station runs.

use crate::config_history::{diff_text, Change, ConfigHistory, KEEP};
use crate::http::{Request, Response};
use crate::settings as st;
use crate::station_api::ApiOp;
use crate::station_http::StationHandler;
use serde_json::{json, Map, Value};

fn unavailable() -> Response {
    Response::error(
        404,
        "not_found",
        "this station keeps no configuration history (it has no configuration file or data directory)",
    )
}

/// Where a changed key reaches the running station.
#[derive(PartialEq)]
enum Part {
    Chain,
    Loudness,
    Other,
}

fn part_of(key: &str) -> Part {
    let top = key
        .trim_start_matches('[')
        .split(['.', ']'])
        .next()
        .unwrap_or("");
    match top {
        "chain" | "processing_preset" => Part::Chain,
        "loudness" => Part::Loudness,
        _ => Part::Other,
    }
}

impl StationHandler {
    pub(crate) fn config_history_route(
        &self,
        req: &mut Request,
        segs: &[&str],
        method: &str,
    ) -> Response {
        let _ = req;
        let Some(h) = self.api.history() else {
            return unavailable();
        };
        let get = method == "GET" || method == "HEAD";
        match (segs, get, method) {
            ([], true, _) => self.history_list(&h),
            ([id], true, _) => match id.parse() {
                Ok(id) => self.history_one(&h, id),
                Err(_) => Response::error(400, "bad_request", "the version id must be a number"),
            },
            ([id, "restore"], _, "POST") => match id.parse() {
                Ok(id) => self.history_restore(&h, id),
                Err(_) => Response::error(400, "bad_request", "the version id must be a number"),
            },
            _ => Response::error(405, "method_not_allowed", "that method is not allowed here"),
        }
    }

    fn history_list(&self, h: &ConfigHistory) -> Response {
        match h.list() {
            Ok(l) => Response::json(
                200,
                &json!({
                    "station": self.api.opts.name,
                    "keep": KEEP,
                    "coalesce_s": crate::config_history::COALESCE_MS / 1000,
                    "versions": l.iter().map(|e| e.json()).collect::<Vec<_>>(),
                }),
            ),
            Err(e) => Response::error(500, "internal", &e),
        }
    }

    fn history_one(&self, h: &ConfigHistory, id: i64) -> Response {
        let (v, prev) = match h.get(id) {
            Ok(Some(x)) => x,
            Ok(None) => {
                return Response::error(
                    404,
                    "not_found",
                    "no such version (it may have been pruned)",
                )
            }
            Err(e) => return Response::error(500, "internal", &e),
        };
        let current = std::fs::read_to_string(h.toml_path()).unwrap_or_default();
        let mut out = v.entry.json();
        // What this version changed, and what restoring it would change now.
        let own: Vec<Change> = diff_text(&prev.unwrap_or_default(), &v.toml);
        let vs_now: Vec<Change> = diff_text(&current, &v.toml);
        out["toml"] = json!(v.toml);
        out["detail"] = Value::Array(
            own.iter()
                .map(|c| crate::config_labels::describe(c).json())
                .collect(),
        );
        out["vs_current"] = Value::Array(
            vs_now
                .iter()
                .map(|c| crate::config_labels::describe(c).json())
                .collect(),
        );
        out["same_as_current"] = json!(current == v.toml);
        Response::json(200, &out)
    }

    fn history_restore(&self, h: &ConfigHistory, id: i64) -> Response {
        let (v, _) = match h.get(id) {
            Ok(Some(x)) => x,
            Ok(None) => {
                return Response::error(
                    404,
                    "not_found",
                    "no such version (it may have been pruned)",
                )
            }
            Err(e) => return Response::error(500, "internal", &e),
        };
        let path = h.toml_path().to_path_buf();
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current == v.toml {
            return Response::json(
                200,
                &json!({"restored": id, "changed": 0, "version": h.newest_id(),
                        "message": "The settings already match that version.",
                        "applied": {}, "needs_restart": []}),
            );
        }
        let cfg = match common::config::StationConfig::parse(&v.toml, &path) {
            Ok(c) => c,
            Err(e) => {
                return Response::error(
                    409,
                    "conflict",
                    &format!(
                        "that version no longer passes the parser, so it was not restored: {}",
                        e.0.first().map(|x| x.message.clone()).unwrap_or_default()
                    ),
                )
            }
        };
        let changes = diff_text(&current, &v.toml);
        // The route every save takes: a temporary file, a `.bak`, a rename; the history records it as a restore.
        let wrote = h.track_at("restore", Some(v.entry.ts_ms), || {
            st::write_atomic(&path, &v.toml)
        });
        if let Err(e) = wrote {
            return Response::error(500, "write_failed", &e);
        }
        let mut applied = Map::new();
        let mut restart: Vec<String> = vec![];
        let mut notes: Vec<String> = vec![];
        // The chain.
        if changes.iter().any(|c| part_of(&c.key) == Part::Chain) {
            match self.api.chain.get() {
                Some(c) => match c.restore(&self.api, &cfg) {
                    Ok(()) => {
                        applied.insert("chain".into(), json!(true));
                    }
                    Err(e) => {
                        applied.insert("chain".into(), json!(false));
                        restart.push("processing chain".into());
                        notes.push(format!("The processing chain could not start now ({e})."));
                    }
                },
                None => {
                    applied.insert("chain".into(), json!(false));
                    restart.push("processing chain".into());
                }
            }
        }
        // The loudness controller.
        if changes.iter().any(|c| part_of(&c.key) == Part::Loudness) {
            self.loudness_apply(cfg.loudness.clone());
            applied.insert("loudness".into(), json!(true));
        }
        // Single settings the station takes while it runs.
        let live = match (
            current.parse::<toml_edit::DocumentMut>(),
            v.toml.parse::<toml_edit::DocumentMut>(),
        ) {
            (Ok(a), Ok(b)) => st::changed_keys(&a, &b, "station"),
            _ => vec![],
        };
        let mut live_values = Map::new();
        for (d, val) in &live {
            if (d.apply == st::LIVE || d.apply == st::NEXT_BOUNDARY)
                && part_of(d.key) == Part::Other
            {
                live_values.insert(d.key.to_string(), val.clone());
            }
        }
        let mut live_done = vec![];
        if !live_values.is_empty() {
            match self.api.command(ApiOp::LiveSettings {
                values: live_values.clone(),
            }) {
                crate::station_api::ApiReply::Done { .. } => {
                    live_done = live_values.keys().cloned().collect();
                }
                crate::station_api::ApiReply::Refused { message, .. } => {
                    notes.push(format!("Some settings could not start now ({message})."));
                    restart.extend(live_values.keys().cloned());
                }
            }
        }
        applied.insert("settings".into(), json!(live_done));
        // Everything else that differs waits for a restart of the station.
        for c in &changes {
            // A section that came or went has its keys listed on their own.
            if part_of(&c.key) != Part::Other || c.key.starts_with('[') {
                continue;
            }
            if !live_values.contains_key(&c.key) && !restart.contains(&c.key) {
                restart.push(c.key.clone());
            }
        }
        let message = if restart.is_empty() {
            "Restored. Everything is in use now.".to_string()
        } else {
            format!(
                "Restored. These settings are in the file and take effect when the station restarts: {}.",
                restart.join(", ")
            )
        };
        Response::json(
            200,
            &json!({
                "restored": id,
                "changed": changes.len(),
                "version": h.newest_id(),
                "applied": applied,
                "needs_restart": restart,
                "notes": notes,
                "message": message,
            }),
        )
    }
}
