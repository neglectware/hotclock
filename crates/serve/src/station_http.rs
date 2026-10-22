//! The station's HTTP routes (7.2): reads from the published view, actions through the command channel.

use crate::http::{Handler, Reply, Request, Response};
use crate::meters::{self, FRAME_LEN};
use crate::station_api::{ApiOp, ApiReply, StationApi, JSON_TOPICS};
use crate::util::{art_response, csv_field, parse_time};
use crate::web;
use crate::ws::{self, Client, Message, Out};
use serde_json::{json, Value};
use station::programming::{BoundaryMode, StartHow};
use station::runtime::Op;
use station::sched::{At, SwitchHow};
use station::state::iso_ms;
use std::io::Write as _;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct StationHandler {
    pub api: Arc<StationApi>,
}

fn bad(msg: &str) -> Response {
    Response::error(400, "bad_request", msg)
}

fn id_of(s: &str) -> Option<i64> {
    s.parse().ok()
}

fn method_not_allowed() -> Response {
    Response::error(405, "method_not_allowed", "that method is not allowed here")
}

fn parse_at(v: &Value) -> Result<At, Response> {
    match v {
        Value::Null => Ok(At::Next),
        Value::String(s) if s == "next" => Ok(At::Next),
        Value::String(s) if s == "end" => Ok(At::End),
        Value::Number(n) => n
            .as_u64()
            .map(|i| At::Index(i as usize))
            .ok_or_else(|| bad("`at` must be \"next\", \"end\" or a queue index")),
        _ => Err(bad("`at` must be \"next\", \"end\" or a queue index")),
    }
}

fn reply_of(r: ApiReply) -> Response {
    match r {
        ApiReply::Done { mut state, extra } => {
            if !extra.is_null() {
                if let Some(o) = state.as_object_mut() {
                    o.insert("result".into(), extra);
                }
            }
            Response::json(202, &state)
        }
        ApiReply::Refused {
            status,
            code,
            message,
        } => Response::error(status, code, &message),
    }
}

impl StationHandler {
    fn act(&self, op: ApiOp) -> Response {
        reply_of(self.api.command(op))
    }

    /// A command whose answer is the thing asked for (200 with its `result`), not the station's state.
    fn read(&self, op: ApiOp) -> Response {
        match self.api.command(op) {
            ApiReply::Done { extra, .. } => Response::json(200, &extra),
            ApiReply::Refused {
                status,
                code,
                message,
            } => Response::error(status, code, &message),
        }
    }

    /// `POST /api/v1/schedule/changes`: `{playlist_id, at, how?, window_early_s?, window_late_s?, mode?}`.
    fn schedule_change(&self, b: &Value) -> Response {
        let Some(playlist_id) = b["playlist_id"].as_i64() else {
            return bad("`playlist_id` is required");
        };
        let Some(at_ms) = b["at"].as_str().and_then(parse_time) else {
            return bad("`at` must be a time (RFC 3339)");
        };
        let how = match b["how"].as_str() {
            None => StartHow::Segue,
            Some(h) => match StartHow::parse(h) {
                Some(h) => h,
                None => return bad("`how` is now, segue or end"),
            },
        };
        let mode = match b["mode"].as_str() {
            None => match how {
                StartHow::Now => BoundaryMode::Hard,
                _ => BoundaryMode::Soft,
            },
            Some(m) => match BoundaryMode::parse(m) {
                Some(m) => m,
                None => return bad("`mode` is soft_then_hard, soft, hard or hard_wait"),
            },
        };
        self.act(ApiOp::AddChange {
            playlist_id,
            at_ms,
            how,
            mode,
            early_s: b["window_early_s"].as_i64().unwrap_or(0).max(0),
            late_s: b["window_late_s"].as_i64().unwrap_or(300).max(0),
        })
    }

    /// `POST /api/v1/program/start`: `{playlist_id, t0?: "now" | "nominal" | RFC 3339, how?: "now" | "segue" | "end"}`.
    fn program_start(&self, b: &Value) -> Response {
        let Some(playlist_id) = b["playlist_id"].as_i64() else {
            return bad("`playlist_id` is required");
        };
        // The caller says which station it means (the Stream Deck key's): another station's port refuses, and a
        // key with no station is never taken as this one.
        match b.get("station") {
            Some(Value::String(s)) if s.is_empty() => {
                return Response::error(
                    409,
                    "no_station",
                    "the key has no station chosen: pick one in its settings",
                );
            }
            Some(Value::String(s)) if *s != self.api.opts.name => {
                return Response::error(
                    409,
                    "wrong_station",
                    &format!(
                        "this is station {}; the request is for station {s}",
                        self.api.opts.name
                    ),
                );
            }
            _ => {}
        }
        let t0 = match b["t0"].as_str() {
            None | Some("now") => station::program::T0Req::Now,
            Some("nominal") => station::program::T0Req::Nominal,
            Some(t) => match parse_time(t) {
                Some(ms) => station::program::T0Req::At(ms),
                None => return bad("`t0` is now, nominal, or a time (RFC 3339)"),
            },
        };
        let how = match b["how"].as_str() {
            None | Some("now") => SwitchHow::Now,
            Some("segue") => SwitchHow::Segue,
            Some("end") => SwitchHow::End,
            Some(_) => return bad("`how` is now, segue or end"),
        };
        let source = match b["source"].as_str() {
            Some("deck") => "deck",
            Some("ui") => "ui",
            _ => "api",
        };
        self.read(ApiOp::ProgramStart {
            playlist_id,
            t0,
            how,
            source,
        })
    }

    /// `POST /api/v1/end-at`: `{playlist_id, at: RFC 3339, then?: {"switch_playlist": id} | "end"}`.
    fn end_at(&self, b: &Value) -> Response {
        let Some(playlist_id) = b["playlist_id"].as_i64() else {
            return bad("`playlist_id` is required");
        };
        let Some(at_ms) = b["at"].as_str().and_then(parse_time) else {
            return bad("`at` must be a time (RFC 3339)");
        };
        let then = match (&b["then"], b["then"].as_str()) {
            (Value::Null, _) | (_, Some("end")) => station::program::EndThen::End,
            (v, _) => match v["switch_playlist"].as_i64() {
                Some(id) => station::program::EndThen::Playlist(id),
                None => return bad("`then` is \"end\" or {\"switch_playlist\": id}"),
            },
        };
        self.read(ApiOp::EndAt {
            playlist_id,
            at_ms,
            then,
        })
    }

    fn get_now(&self) -> Response {
        let v = match self.api.view.read() {
            Ok(v) => v,
            Err(_) => return Response::error(500, "internal", "view poisoned"),
        };
        let wall = crate::station_api::wall_ms();
        let state = v.state["state"].clone();
        match &v.now {
            None => Response::json(200, &json!({"playing": false, "state": state})),
            Some(n) => {
                let mut o = n.json.clone();
                let m = o.as_object_mut().expect("object");
                m.insert("playing".into(), json!(true));
                m.insert("state".into(), state);
                m.insert("elapsed_ms".into(), json!((wall - n.started_ms).max(0)));
                m.insert("remaining_ms".into(), json!((n.ends_ms - wall).max(0)));
                m.insert(
                    "remaining_to_segue_ms".into(),
                    n.segue_ms.map_or(Value::Null, |s| json!((s - wall).max(0))),
                );
                m.insert("next".into(), n.next.clone());
                Response::json(200, &o)
            }
        }
    }

    fn history(&self, req: &Request, csv: bool) -> Response {
        let Some(db) = &self.api.opts.state_db else {
            return Response::error(404, "not_found", "this station keeps no history");
        };
        let now = crate::station_api::wall_ms();
        let from = match req.query_get("from") {
            Some(t) => match parse_time(&t) {
                Some(v) => v,
                None => return bad("`from` is not a time"),
            },
            None => now - 24 * 3_600_000,
        };
        let to = match req.query_get("to") {
            Some(t) => match parse_time(&t) {
                Some(v) => v,
                None => return bad("`to` is not a time"),
            },
            None => now + 60_000,
        };
        let limit: usize = req
            .query_get("limit")
            .and_then(|l| l.parse().ok())
            .unwrap_or(if csv { 100_000 } else { 100 })
            .clamp(1, 100_000);
        let rows = match station::state::read_as_run_range(db, from, to, limit) {
            Ok(r) => r,
            Err(e) => return Response::error(503, "unavailable", &e),
        };
        if csv {
            let mut out = String::from(
                "started_at,ended_at,title,artist,category,origin,end_reason,played_ms,deviation_ms\r\n",
            );
            for r in rows.iter().rev() {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{}\r\n",
                    iso_ms(r.started_ms),
                    r.ended_ms.map(iso_ms).unwrap_or_default(),
                    csv_field(&r.title),
                    csv_field(&r.artist),
                    csv_field(&r.category),
                    r.origin,
                    r.end_reason.clone().unwrap_or_default(),
                    r.played_ms.map(|p| p.to_string()).unwrap_or_default(),
                    r.planned_ms
                        .map(|p| (r.started_ms - p).to_string())
                        .unwrap_or_default(),
                ));
            }
            return Response::text(200, "text/csv; charset=utf-8", out)
                .with_header("content-disposition", "attachment; filename=\"as-run.csv\"");
        }
        let arr: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "id": r.id, "item_id": r.item_id, "started_at": iso_ms(r.started_ms),
                    "ended_at": r.ended_ms.map(iso_ms), "title": r.title, "artist": r.artist,
                    "category": r.category, "origin": r.origin, "end_reason": r.end_reason,
                    "played_ms": r.played_ms, "planned_at": r.planned_ms.map(iso_ms),
                    "deviation_ms": r.planned_ms.map(|p| r.started_ms - p), "flags": r.flags,
                    "block_id": r.block_id, "run_id": r.run_id, "playlist_id": r.playlist_id,
                })
            })
            .collect();
        Response::json(200, &json!({"rows": arr, "count": arr.len()}))
    }

    /// The block log: in M1 the only blocks that fire are the standing `insert_every` ones, so it is the
    /// as-run rows they produced (the timed blocks of M3 and M5 join it).
    fn block_log(&self, req: &Request) -> Response {
        let Some(db) = &self.api.opts.state_db else {
            return Response::json(200, &json!({"rows": []}));
        };
        let now = crate::station_api::wall_ms();
        let from = req
            .query_get("from")
            .and_then(|t| parse_time(&t))
            .unwrap_or(now - 24 * 3_600_000);
        let to = req
            .query_get("to")
            .and_then(|t| parse_time(&t))
            .unwrap_or(now + 60_000);
        // The block log of the station (5.3): what each timed block did, when, and how far from its time.
        let rows = station::state::read_block_log(db, from, to, 5000).unwrap_or_default();
        let arr: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "run_id": r.run_id, "block_id": r.block_id, "name": r.name, "type": r.kind,
                    "target": iso_ms(r.target_ms), "planned_at": iso_ms(r.planned_ms),
                    "fired_at": r.fired_ms.map(iso_ms), "deviation_ms": r.deviation_ms,
                    "how": r.how, "detail": r.detail,
                })
            })
            .collect();
        Response::json(200, &json!({"rows": arr}))
    }

    fn chain(&self, req: &Request) -> Response {
        match self.api.chain.get() {
            Some(c) => Response::json(200, &c.json(&self.api, req.query_get("schema").is_some())),
            None => Response::error(503, "unavailable", "the processing chain is not running"),
        }
    }

    /// `GET /api/v1/loudness`: the readings come from the latest meter frame (so they equal the frame at
    /// that `seq`, acceptance M6a 7), the settings and the controller's internals from the station.
    fn loudness_get(&self) -> Response {
        match self.loudness_value() {
            Ok(v) => Response::json(200, &v),
            Err(r) => r,
        }
    }

    fn loudness_value(&self) -> Result<Value, Response> {
        let m = &self.api.meter;
        m.want_until_ms.store(
            (crate::station_api::wall_ms() + 3000) as u64,
            Ordering::Relaxed,
        );
        let have = m.frames_emitted.load(Ordering::Relaxed);
        for _ in 0..60 {
            if m.frames_emitted.load(Ordering::Relaxed) > have {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let Some(f) = crate::station_api::latest_frame(&self.api) else {
            return Err(Response::error(
                503,
                "unavailable",
                "the meter has produced no frame yet",
            ));
        };
        let fj = meters::frame_json(&f);
        let cfg = self
            .api
            .loudness_cfg
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let st = m.ctl.status.lock().ok().and_then(|s| s.clone());
        let at = |v: Option<f64>| v.map_or(Value::Null, |x| json!(x));
        let tip = self.makeup_tip(st.as_ref(), cfg.target_lufs);
        Ok(json!({
            "seq": fj["seq"], "frame": fj["frame"],
            "target": fj["target"], "momentary": fj["momentary"], "short_term": fj["short_term"],
            "window": fj["window"], "integrated": fj["integrated"],
            "gain_db": fj["gain_db"], "state": fj["loudness_state"],
            "enabled": cfg.enabled,
            "settings": {
                "enabled": cfg.enabled, "target_lufs": cfg.target_lufs, "true_peak_dbtp": cfg.true_peak_dbtp,
                "window_s": cfg.window_s, "max_slew_db_per_min": cfg.max_slew_db_per_min,
                "deadband_lu": cfg.deadband_lu, "gain_min_db": cfg.gain_min_db, "gain_max_db": cfg.gain_max_db,
                "min_valid_s": cfg.min_valid_s, "gate_categories": cfg.gate_categories,
                "trim_tau_s": cfg.trim_tau_s, "trim_range_db": cfg.trim_range_db,
                    "preset": cfg.preset, "user_preset": cfg.user_preset, "within_item": cfg.within_item, "item_memory_s": cfg.item_memory_s, "robust_items": cfg.robust_items, "item_cap_s": cfg.item_cap_s,
                    "peak_allowance_db": cfg.peak_allowance_db, "max_limiter_gr_db": cfg.max_limiter_gr_db,
            },
            "controller": st.as_ref().map(|s| json!({
                "target_gain_db": at(s.target_gain_db), "trim_db": s.trim_db, "window_in": at(s.window_in),
                "window_out": at(s.window_out), "valid_s": s.valid_s, "at_limit_s": s.at_limit_s,
                "frozen_playing_s": s.frozen_playing_s,
            })),
            "static_start_db": self.api.opts.loudness_static_db,
            "note": tip.as_ref().map_or(Value::Null, |t| json!(t.0)),
            "makeup": tip.as_ref().map_or(Value::Null, |t| t.1.clone()),
        }))
    }

    /// The tip under the loudness controller's card, and what "Move to makeup" would do. It is shown only when
    /// the gain has stood still more than 3 dB from 0 for five minutes (not mid-move), the controller is on,
    /// a shipped or saved processing chain is in the path, and no move is already gliding. Plain numbers, no
    /// alarm: the controller is doing its job, and the chain could do it instead.
    fn makeup_tip(
        &self,
        st: Option<&engine::loudness::Status>,
        target: f64,
    ) -> Option<(String, Value)> {
        let st = st?;
        if !st.enabled
            || st.shifting_db != 0.0
            || st.gain_db.abs() <= engine::loudness::FAR_GAIN_DB
            || st.far_steady_s < engine::loudness::FAR_GAIN_AFTER_S
        {
            return None;
        }
        let chain = self.api.chain.get()?;
        let cfg = chain.current().cfg;
        let in_path = common::chain::BLOCKS
            .iter()
            .any(|b| b.switch && cfg.enabled(b.key));
        if !in_path {
            return None;
        }
        let g = st.gain_db;
        let moved = (g * 10.0).round() / 10.0;
        let now = cfg.num("makeup", "gain_db");
        let to = ((now + moved) * 100.0).round() / 100.0;
        let text = makeup_tip_text(g, target);
        let available = (-24.0..=24.0).contains(&to) && moved.abs() >= 0.1;
        Some((
            text,
            json!({"available": available, "move_db": moved, "makeup_db": now, "new_makeup_db": to,
                   "gain_db": g, "glide_s": moved.abs() / engine::chain::MAKEUP_SLEW_DB_PER_S}),
        ))
    }

    /// Start the controller with `cfg` (nothing is written).
    pub(crate) fn loudness_apply(&self, cfg: common::config::LoudnessConfig) {
        if let Some(live) = self.api.loudness_live.get() {
            if let Ok(mut g) = live.gate.write() {
                *g = cfg.gate_categories.clone();
            }
            live.set_allowance(cfg.enabled.then_some(cfg.peak_allowance_db));
        }
        let p = crate::meters::params_of(&cfg);
        if let Ok(mut cur) = self.api.loudness_cfg.lock() {
            *cur = cfg;
        }
        if let Ok(mut pp) = self.api.meter.ctl.params.lock() {
            *pp = p;
        }
        self.api.meter.ctl.rev.fetch_add(1, Ordering::AcqRel);
    }

    /// `PUT /api/v1/loudness`: change the controller live; the new settings are also written to the station
    /// config's `[loudness]` block.
    fn loudness_put(&self, body: &Value) -> Response {
        let Some(obj) = body.as_object() else {
            return bad("the body must be a JSON object");
        };
        let mut cfg = self
            .api
            .loudness_cfg
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        // A preset named in the body goes first (it replaces the five settings it controls); the other keys of
        // the body then override it, and a preset they no longer match reads `custom`.
        if let Some(pv) = obj.get("preset") {
            match pv.as_str() {
                Some("custom") => cfg.preset = "custom".into(),
                Some(name) => {
                    if !cfg.apply_preset(name) {
                        return bad(&format!("unknown preset `{name}`"));
                    }
                }
                None => return bad("`preset` must be a string"),
            }
        }
        let mut rest = obj.clone();
        rest.remove("preset");
        if let Err(m) = cfg.apply_params(&rest) {
            return bad(&m);
        }
        cfg.mark_custom();
        let problems = cfg.check();
        if let Some((k, m)) = problems.first() {
            return bad(&format!("`{k}` {m}"));
        }
        let mut written = false;
        if let Some(path) = &self.api.opts.config_path {
            let write = || common::config::write_loudness_block(path, &cfg);
            let r = match self.api.history() {
                Some(h) => h.track("loudness", write),
                None => write(),
            };
            match r {
                Ok(()) => written = true,
                Err(e) => {
                    return Response::error(
                        500,
                        "config_write_failed",
                        &format!("cannot write {}: {e}", path.display()),
                    )
                }
            }
        }
        self.loudness_apply(cfg);
        match self.loudness_value() {
            Ok(mut v) => {
                v["config_written"] = json!(written);
                Response::json(200, &v)
            }
            Err(r) => r,
        }
    }

    /// `POST /api/v1/loudness/reset`: forget the integrated reading, the windows and the trim, and put the
    /// gain back at the static start estimate (6.1).
    fn loudness_reset(&self) -> Response {
        let est = self.api.opts.loudness_static_db;
        if let Ok(mut r) = self.api.meter.ctl.reset.lock() {
            *r = Some(est);
        }
        self.api.meter.reset.store(true, Ordering::Relaxed);
        Response::json(202, &json!({"reset": true, "gain_db": est}))
    }

    /// `GET /api/v1/chain/agc`: what the AGC is doing now, for its card: the state (`riding`, `gated`,
    /// `riding_fade`, `clamping`; the most telling one since the previous request), the gain and the gain it is
    /// heading for, the fast and program detector levels and the target.
    fn chain_agc(&self) -> Response {
        let Some(c) = self.api.chain.get() else {
            return Response::error(503, "unavailable", "the processing chain is not running");
        };
        let cfg = c.current().cfg;
        let v = c.ctl.meters.agc_view();
        let lv = |x: f64| {
            if x.is_finite() && x > -200.0 {
                json!(x)
            } else {
                Value::Null
            }
        };
        Response::json(
            200,
            &json!({
                "enabled": cfg.enabled("agc"),
                "state": match v.state {
                    engine::chain::agc::AgcState::Riding => "riding",
                    engine::chain::agc::AgcState::Gated => "gated",
                    engine::chain::agc::AgcState::RidingFade => "riding_fade",
                    engine::chain::agc::AgcState::Clamping => "clamping",
                },
                "gain_db": v.gain_db, "desired_db": v.desired_db,
                "fast_dbfs": lv(v.fast_db), "program_dbfs": lv(v.program_db),
                "target_dbfs": cfg.num("agc", "target_dbfs"),
            }),
        )
    }

    fn chain_meters(&self) -> Response {
        let m = &self.api.meter;
        m.want_until_ms.store(
            (crate::station_api::wall_ms() + 3000) as u64,
            Ordering::Relaxed,
        );
        let have = m.frames_emitted.load(Ordering::Relaxed);
        // Frames are built only while somebody listens: wait for one built after this request.
        for _ in 0..60 {
            if m.frames_emitted.load(Ordering::Relaxed) > have {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        match crate::station_api::latest_frame(&self.api) {
            Some(f) => Response::json(200, &meters::frame_json(&f)),
            None => Response::error(503, "unavailable", "the meter has produced no frame yet"),
        }
    }

    fn ws(&self, req: &Request) -> Reply {
        let topics: Vec<String> = req
            .query_get("topics")
            .unwrap_or_else(|| JSON_TOPICS.to_string())
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let api = self.api.clone();
        ws::upgrade(req, move |stream, _buffered| {
            let a2 = api.clone();
            serve_ws(
                api.hub.clone(),
                Arc::new(move |seq| a2.snapshot_message(seq)),
                stream,
                topics,
            )
        })
    }
}

/// One WebSocket client: a writer thread drains its queue, this thread reads its commands.
pub fn serve_ws(
    hub: Arc<crate::ws::Hub>,
    snapshot: Arc<dyn Fn(u64) -> String + Send + Sync>,
    stream: TcpStream,
    topics: Vec<String>,
) {
    let _ = stream.set_nodelay(true);
    // Little kernel buffering behind a meter client, so that one that stops reading fills it in seconds and the
    // bounded queue (which drops the oldest frame) takes over, instead of MB of stale frames piling up.
    let _ = rustix::net::sockopt::set_socket_send_buffer_size(&stream, 32 * 1024);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
    let Ok(wstream) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(wstream));
    let client = Client::new(topics);
    if let Ok(s) = stream.try_clone() {
        client.attach(s);
    }
    // The snapshot first, then whatever comes.
    client.push(
        Out::Text(Arc::from(snapshot(client.seq.load(Ordering::SeqCst)))),
        false,
    );
    hub.add(client.clone());
    let (c2, w2) = (client.clone(), writer.clone());
    let wt = std::thread::Builder::new()
        .name("ws-write".into())
        .spawn(move || {
            let mut last_ping = std::time::Instant::now();
            loop {
                match c2.pop(Duration::from_millis(500)) {
                    Err(_) => {
                        if let Ok(mut w) = w2.lock() {
                            let _ = ws::write_message(&mut *w, &Message::Close, None);
                        }
                        break;
                    }
                    Ok(Some(out)) => {
                        let m = match out {
                            Out::Text(t) => Message::Text(t.to_string()),
                            Out::Binary(b) => Message::Binary(b.to_vec()),
                        };
                        let Ok(mut w) = w2.lock() else { break };
                        if ws::write_message(&mut *w, &m, None).is_err() {
                            c2.close();
                            break;
                        }
                    }
                    Ok(None) => {}
                }
                if last_ping.elapsed() >= Duration::from_secs(20) {
                    last_ping = std::time::Instant::now();
                    let Ok(mut w) = w2.lock() else { break };
                    if ws::write_message(&mut *w, &Message::Ping(vec![]), None).is_err() {
                        c2.close();
                        break;
                    }
                }
            }
            // The reader below blocks in `read` with no timeout: shut the socket so that it returns and the
            // connection (thread, slot of MAX_CONNECTIONS, hub entry) is released, whether the client was
            // cut off for falling behind or its writes timed out (a peer that vanished).
            if let Ok(w) = w2.lock() {
                let _ = w.shutdown(std::net::Shutdown::Both);
            }
        });
    let mut reader = stream;
    loop {
        match ws::read_message(&mut reader, 64 * 1024) {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                match v["cmd"].as_str() {
                    Some("snapshot") => {
                        client.push(
                            Out::Text(Arc::from(snapshot(client.seq.load(Ordering::SeqCst)))),
                            false,
                        );
                    }
                    Some("subscribe") | Some("unsubscribe") => {
                        let add = v["cmd"] == "subscribe";
                        let list: Vec<String> = v["topics"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Ok(mut tp) = client.topics.lock() {
                            for t in list {
                                if add {
                                    if !tp.contains(&t) {
                                        tp.push(t);
                                    }
                                } else {
                                    tp.retain(|x| *x != t);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Ping(p)) => {
                if let Ok(mut w) = writer.lock() {
                    let _ = ws::write_message(&mut *w, &Message::Pong(p), None);
                }
            }
            Ok(Message::Close) | Err(_) => break,
            Ok(_) => {}
        }
    }
    client.close();
    hub.remove(&client);
    if let Ok(t) = wt {
        let _ = t.join();
    }
    if let Ok(w) = writer.lock() {
        let _ = w.shutdown(std::net::Shutdown::Both);
    }
    let _ = std::io::stdout().flush();
}

impl Handler for StationHandler {
    fn handle(&self, req: &mut Request) -> Reply {
        let path = req.path.clone();
        let method = req.method.clone();
        let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
        let get = method == "GET" || method == "HEAD";
        let r: Response = match (segs.as_slice(), get) {
            (["health"], true) => {
                // One read lock: taking a second one here (as `is_stale()` did) can deadlock against the
                // loop's writer, which would stop the station.
                let v = self.api.view.read().unwrap_or_else(|e| e.into_inner());
                if v.is_stale() {
                    let mut h = v.health.clone();
                    if let Some(o) = h.as_object_mut() {
                        o.insert("status".into(), json!("critical"));
                        o.insert("stale".into(), json!(true));
                        o.insert(
                            "message".into(),
                            json!(
                                "the station loop has stopped publishing (engine stall or startup)"
                            ),
                        );
                    } else {
                        h = json!({"status": "critical", "stale": true, "product": self.api.opts.product});
                    }
                    Response::json(503, &h)
                } else {
                    Response::json(v.health_status, &v.health)
                }
            }
            (["metrics"], true) => {
                let v = self.api.view.read().unwrap_or_else(|e| e.into_inner());
                Response::text(
                    200,
                    "text/plain; version=0.0.4; charset=utf-8",
                    crate::observe::station_metrics(
                        &self.api,
                        &v.counters,
                        &self.api.meter,
                        &v.audio,
                    ),
                )
            }
            (["display"], true) => web::page("display.html", &self.api.opts.product, "/api/v1"),
            (["static", name], true) => web::static_asset(name),
            // The stream mount (1.3a): read-only, GET and HEAD, no body accepted.
            (["stream", file], true) => match self.api.stream.get() {
                Some(m) => {
                    return crate::stream_http::listener_reply(
                        m,
                        file,
                        &self.api.opts.product,
                        &req.peer.to_string(),
                        method == "HEAD",
                    )
                }
                None => Response::error(404, "not_found", "this station serves no stream"),
            },
            (["api", "v1", rest @ ..], _) => return self.api_route(req, rest, &method),
            _ if known_shape(&segs.join("/")) => method_not_allowed(),
            _ => Response::error(404, "not_found", "no such page"),
        };
        r.into()
    }
}

impl StationHandler {
    fn api_route(&self, req: &mut Request, segs: &[&str], method: &str) -> Reply {
        let get = method == "GET" || method == "HEAD";
        let post = method == "POST";
        let r = match (segs, method) {
            (["ws"], _) if get => return self.ws(req),
            (["state"], _) if get => Response::json(200, &self.api.state_json()),
            (["now"], _) if get => self.get_now(),
            (["queue"], _) if get => {
                let q = self
                    .api
                    .view
                    .read()
                    .map(|v| v.queue.clone())
                    .unwrap_or(Value::Null);
                Response::json(200, &json!({"entries": q}))
            }
            (["queue"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match (b["item_id"].as_i64(), parse_at(&b["at"])) {
                    (Some(item_id), Ok(at)) => self.act(ApiOp::Op(Op::Insert { item_id, at })),
                    (None, _) => bad("`item_id` is required"),
                    (_, Err(r)) => r,
                },
            },
            (["queue", "album"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match parse_at(&b["at"]) {
                    Err(r) => r,
                    Ok(at) => {
                        let gapless = match b["continuity"].as_str() {
                            None | Some("gapless") => true,
                            Some("normal") => false,
                            Some(_) => {
                                return bad("`continuity` is \"gapless\" or \"normal\"").into()
                            }
                        };
                        self.act(ApiOp::Album {
                            item_id: b["item_id"].as_i64(),
                            album_key: b["album_key"].as_str().map(str::to_string),
                            gapless,
                            at,
                        })
                    }
                },
            },
            (["queue", id], "DELETE") => match id_of(id) {
                Some(entry_id) => self.act(ApiOp::Op(Op::Remove { entry_id })),
                None => bad("the entry id must be a number"),
            },
            (["queue", id], "PATCH") => match (id_of(id), req.json()) {
                (Some(entry_id), Ok(b)) => {
                    // `transition`: null clears; "cut"; or {"preset": name, "patch": {...}} (2.6, step 4).
                    let transition = match &b["transition"] {
                        Value::Null => None,
                        Value::String(s) if s == "cut" => Some("cut".to_string()),
                        Value::Object(o) if o.keys().all(|k| k == "preset" || k == "patch") => {
                            Some(b["transition"].to_string())
                        }
                        _ => {
                            return bad(
                                "`transition` is null, \"cut\", or an object with `preset` and/or `patch`",
                            )
                            .into()
                        }
                    };
                    if b.get("transition").is_none() {
                        return bad("`transition` is required (null clears it)").into();
                    }
                    self.act(ApiOp::Op(Op::SetTransition {
                        entry_id,
                        transition,
                    }))
                }
                (None, _) => bad("the entry id must be a number"),
                (_, Err(r)) => r,
            },
            (["queue", id, "move"], "POST") => match (id_of(id), req.json()) {
                (Some(entry_id), Ok(b)) => match b["to"].as_u64() {
                    Some(to) => self.act(ApiOp::Op(Op::Move {
                        entry_id,
                        to: to as usize,
                    })),
                    None => bad("`to` must be a queue index"),
                },
                (None, _) => bad("the entry id must be a number"),
                (_, Err(r)) => r,
            },
            (["transport", action], "POST") => {
                let body = match req.json() {
                    Ok(b) => b,
                    Err(r) => return r.into(),
                };
                let fade = body["fade_ms"].as_f64().map(|ms| ms / 1000.0);
                match *action {
                    "play" => self.act(ApiOp::Play {
                        playlist_id: body["playlist_id"].as_i64(),
                    }),
                    "next" => self.act(ApiOp::Op(Op::Next)),
                    "fade-next" => self.act(ApiOp::Op(Op::FadeNext(fade))),
                    "cut" => self.act(ApiOp::Op(Op::Cut)),
                    "stop" => self.act(ApiOp::Op(Op::Stop { fade_s: fade })),
                    "hold" => self.act(ApiOp::Op(Op::Hold)),
                    "resume" => self.act(ApiOp::Op(Op::Resume)),
                    "pause" | "unpause" => Response::error(
                        501,
                        "not_implemented",
                        "pause stops the station clock, which the engine does not do yet",
                    ),
                    _ => Response::error(404, "not_found", "no such transport action"),
                }
            }
            (["mode"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match b["mode"].as_str() {
                    Some("auto") => self.act(ApiOp::Mode { assist: false }),
                    Some("assist") => self.act(ApiOp::Mode { assist: true }),
                    _ => bad("`mode` is \"auto\" or \"assist\""),
                },
            },
            (["playlist", "switch"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => {
                    let how = match b["how"].as_str() {
                        Some("now") => SwitchHow::Now,
                        Some("segue") | None => SwitchHow::Segue,
                        Some("end") => SwitchHow::End,
                        Some(_) => return bad("`how` is \"now\", \"segue\" or \"end\"").into(),
                    };
                    match b["playlist_id"].as_i64() {
                        Some(playlist_id) => self.act(ApiOp::Op(Op::Switch { playlist_id, how })),
                        None => bad("`playlist_id` is required"),
                    }
                }
            },
            (["playlist", "reshuffle"], "POST") => self.act(ApiOp::Op(Op::Reshuffle)),
            (["rotation"], _) if get => self.rotation(req),
            (["cart"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match b["item_id"].as_i64() {
                    Some(item_id) => self.act(ApiOp::Op(Op::FireCart { item_id })),
                    None => bad("`item_id` is required"),
                },
            },
            (["alerts"], _) if get => {
                let a = self
                    .api
                    .view
                    .read()
                    .map(|v| v.alerts.clone())
                    .unwrap_or(Value::Null);
                Response::json(200, &json!({"alerts": a}))
            }
            (["alerts", id, "ack"], "POST") => match id_of(id) {
                Some(alert_id) => self.act(ApiOp::Ack { alert_id }),
                None => bad("the alert id must be a number"),
            },
            (["history"], _) if get => self.history(req, false),
            (["history.csv"], _) if get => self.history(req, true),
            (["blocks", "log"], _) if get => self.block_log(req),
            (["chain"], _) if get => self.chain(req),
            (["chain"], "PUT") => match (self.api.chain.get(), req.json()) {
                (_, Err(r)) => r,
                (None, _) => {
                    Response::error(503, "unavailable", "the processing chain is not running")
                }
                (Some(c), Ok(b)) => c.put_chain(&self.api, &b),
            },
            (["chain", stage], "PUT") if *stage != "meters" => {
                match (self.api.chain.get(), req.json()) {
                    (_, Err(r)) => r,
                    (None, _) => {
                        Response::error(503, "unavailable", "the processing chain is not running")
                    }
                    (Some(c), Ok(b)) => c.put_block(&self.api, stage, &b),
                }
            }
            (["chain", "meters"], _) if get => self.chain_meters(),
            (["chain", "agc"], _) if get => self.chain_agc(),
            (["transition-preset"], "GET") => {
                // Read from the station itself (not the published view, which lags a change by up to a second).
                match self.api.command(ApiOp::TransitionPreset { name: None }) {
                    ApiReply::Done { extra, .. } => Response::json(200, &extra),
                    r => reply_of(r),
                }
            }
            (["transition-preset"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match b["name"].as_str() {
                    Some(name) => self.act(ApiOp::TransitionPreset {
                        name: Some(name.into()),
                    }),
                    None => bad("`name` is required"),
                },
            },
            (["live-settings"], "POST") => match req.json() {
                Err(r) => r,
                Ok(Value::Object(values)) => self.act(ApiOp::LiveSettings { values }),
                Ok(_) => bad("the body must be an object of settings"),
            },
            (["processing", "preset"], "POST") => match (self.api.chain.get(), req.json()) {
                (_, Err(r)) => r,
                (None, _) => {
                    Response::error(503, "unavailable", "the processing chain is not running")
                }
                (Some(c), Ok(b)) => match b["name"].as_str() {
                    Some(name) => c.post_preset(&self.api, name),
                    None => bad("`name` is required"),
                },
            },
            (["processing", "bypass"], "POST") => match (self.api.chain.get(), req.json()) {
                (_, Err(r)) => r,
                (None, _) => {
                    Response::error(503, "unavailable", "the processing chain is not running")
                }
                (Some(c), Ok(b)) => match b["on"].as_bool() {
                    Some(on) => c.post_bypass(&self.api, on),
                    None => bad("`on` must be true or false"),
                },
            },
            (["output-trim"], _) if get => self.act(ApiOp::Trim { db: None }),
            (["output-trim"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match b["db"].as_f64() {
                    Some(db)
                        if (common::config::OUTPUT_TRIM_MIN_DB
                            ..=common::config::OUTPUT_TRIM_MAX_DB)
                            .contains(&db) =>
                    {
                        self.act(ApiOp::Trim { db: Some(db) })
                    }
                    _ => bad(&format!(
                        "`db` must be a number from {} to +{}",
                        common::config::OUTPUT_TRIM_MIN_DB,
                        common::config::OUTPUT_TRIM_MAX_DB
                    )),
                },
            },
            (["external", name], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => self.act(ApiOp::External {
                    name: (*name).into(),
                    value: b["value"].clone(),
                }),
            },
            (["items", id, "release"], "POST") => match id_of(id) {
                Some(item_id) => self.act(ApiOp::Release { item_id }),
                None => bad("the item id must be a number"),
            },
            (["internal", "reload"], "POST") => self.act(ApiOp::Reload),
            (["items", id, "art"], _) if get => match id_of(id) {
                Some(item_id) => art_response(
                    &self.api.opts.library_db,
                    &self.api.opts.media_dir,
                    item_id,
                    &req.query_get("size").unwrap_or_else(|| "600".into()),
                    req.header("if-none-match"),
                ),
                None => bad("the item id must be a number"),
            },
            (["schedule", "upcoming"], _) if get => {
                let hours = req
                    .query_get("hours")
                    .and_then(|h| h.parse::<i64>().ok())
                    .unwrap_or(24);
                self.read(ApiOp::Upcoming { hours })
            }
            (["schedule", "changes"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => self.schedule_change(&b),
            },
            (["schedule", "changes", id], "DELETE") => match id_of(id) {
                Some(id) => self.read(ApiOp::CancelChange { id }),
                None => bad("the change id must be a number"),
            },
            (["program", "state"], _) if get => self.read(ApiOp::ProgramState),
            (["program", "start"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => self.program_start(&b),
            },
            (["program", "abort"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => self.act(ApiOp::ProgramAbort {
                    stop_audio: b["stop_audio"].as_bool().unwrap_or(false),
                }),
            },
            (["program", "continue"], "POST") => self.act(ApiOp::ProgramContinue),
            (["program", action @ ("arm" | "disarm")], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => match b["playlist_id"].as_i64() {
                    None => bad("`playlist_id` is required"),
                    Some(playlist_id) => {
                        let occurrence_ms = match b["occurrence"].as_str() {
                            None => None,
                            Some(t) => match parse_time(t) {
                                Some(m) => Some(m),
                                None => {
                                    return bad("`occurrence` must be a time (RFC 3339)").into()
                                }
                            },
                        };
                        self.read(ApiOp::ProgramArm {
                            playlist_id,
                            occurrence_ms,
                            armed: *action == "arm",
                        })
                    }
                },
            },
            (["program", "blocks", "next", "skip"], "POST") => self.read(ApiOp::ProgramSkip),
            (["program", "blocks", id, "fire"], "POST") => match id_of(id) {
                Some(block_id) => self.read(ApiOp::ProgramFire { block_id }),
                None => bad("the block id must be a number"),
            },
            (["program", ..], _) => method_not_allowed(),
            (["end-at"], "POST") => match req.json() {
                Err(r) => r,
                Ok(b) => self.end_at(&b),
            },
            (["config-history", ..], _) => self.config_history_route(req, &segs[1..], method),
            (["loudness"], _) if get => self.loudness_get(),
            (["loudness"], "PUT") => match req.json() {
                Err(r) => r,
                Ok(b) => self.loudness_put(&b),
            },
            (["loudness", "reset"], "POST") => self.loudness_reset(),
            (["loudness", "move-to-makeup"], "POST") => match self.api.chain.get() {
                Some(c) => c.move_gain_to_makeup(&self.api),
                None => Response::error(503, "unavailable", "the processing chain is not running"),
            },
            (["schedule", ..], _) => method_not_allowed(),
            (["stream"], _) if get => match self.api.stream.get() {
                Some(m) => Response::json(200, &crate::stream_http::stream_json(m)),
                None => Response::json(200, &json!({"enabled": false})),
            },
            (["stream", "disconnect"], "POST") => self.stream_disconnect(req),
            _ if known_shape(&format!("api/v1/{}", segs.join("/"))) => method_not_allowed(),
            _ => Response::error(404, "not_found", "no such endpoint"),
        };
        let _ = post;
        r.into()
    }

    /// `POST /api/v1/stream/disconnect` with `{"id": n}` or `{"all": true}`.
    fn stream_disconnect(&self, req: &mut Request) -> Response {
        let Some(m) = self.api.stream.get() else {
            return Response::error(404, "not_found", "this station serves no stream");
        };
        let body = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let n = if body["all"].as_bool() == Some(true) {
            m.disconnect(None)
        } else if let Some(id) = body["id"].as_u64() {
            m.disconnect(Some(id))
        } else {
            return bad("send {\"id\": <listener id>} or {\"all\": true}");
        };
        Response::json(200, &json!({"disconnecting": n}))
    }

    fn rotation(&self, req: &Request) -> Response {
        let pid = req.query_get("playlist_id").and_then(|s| s.parse().ok());
        reply_data(self.api.command(ApiOp::Rotation { playlist_id: pid }))
    }
}

/// An answer whose body is the operation's data, not the state.
fn reply_data(r: ApiReply) -> Response {
    match r {
        ApiReply::Done { extra, .. } => Response::json(200, &extra),
        ApiReply::Refused {
            status,
            code,
            message,
        } => Response::error(status, code, &message),
    }
}

/// Kept so the meter frame length is visible next to the WebSocket code that carries it.
pub const _FRAME_LEN: usize = FRAME_LEN;

/// The path shapes this service has, so a wrong method answers 405 and an unknown path 404. `*` is one segment.
/// The words of the tip under the loudness controller's card for a gain `g` dB that has stood still far from 0.
/// Plain and neutral: what the controller is doing, that it works, and the two ways to hand the work to the chain.
pub fn makeup_tip_text(g: f64, target: f64) -> String {
    let num = |x: f64| {
        if (x - x.round()).abs() < 0.05 {
            format!("{x:.0}")
        } else {
            format!("{x:.1}")
        }
    };
    if g > 0.0 {
        format!(
            "Tip: the loudness controller is adding {:+.1} dB to reach {} LUFS. That works, but the processing chain is meant to land close to the target by itself. Raise the chain's output (makeup) by about {:.0} dB, or press Move to makeup.",
            g,
            num(target),
            g.abs()
        )
    } else {
        format!(
            "Tip: the loudness controller is taking away {:.1} dB to reach {} LUFS. That works, but the processing chain is meant to land close to the target by itself. Lower the chain's output (makeup) by about {:.0} dB, or press Move to makeup.",
            g.abs(),
            num(target),
            g.abs()
        )
    }
}

pub fn known_shape(path: &str) -> bool {
    const SHAPES: &[&str] = &[
        "health",
        "metrics",
        "display",
        "static/*",
        "api/v1/ws",
        "api/v1/state",
        "api/v1/now",
        "api/v1/queue",
        "api/v1/queue/album",
        "api/v1/queue/*",
        "api/v1/queue/*/move",
        "api/v1/transport/*",
        "api/v1/mode",
        "api/v1/transition-preset",
        "api/v1/live-settings",
        "api/v1/playlist/switch",
        "api/v1/playlist/reshuffle",
        "api/v1/rotation",
        "api/v1/cart",
        "api/v1/alerts",
        "api/v1/alerts/*/ack",
        "api/v1/history",
        "api/v1/history.csv",
        "api/v1/blocks/log",
        "api/v1/schedule/upcoming",
        "api/v1/schedule/changes",
        "api/v1/schedule/changes/*",
        "api/v1/program/state",
        "api/v1/program/start",
        "api/v1/program/abort",
        "api/v1/program/continue",
        "api/v1/program/arm",
        "api/v1/program/disarm",
        "api/v1/program/blocks/*/fire",
        "api/v1/program/blocks/next/skip",
        "api/v1/chain",
        "api/v1/chain/meters",
        "api/v1/chain/*",
        "api/v1/processing/preset",
        "api/v1/processing/bypass",
        "api/v1/output-trim",
        "api/v1/external/*",
        "api/v1/internal/reload",
        "api/v1/items/*/art",
        "api/v1/items/*/release",
        "api/v1/config-history",
        "api/v1/config-history/*",
        "api/v1/config-history/*/restore",
        "api/v1/loudness",
        "api/v1/loudness/reset",
        "api/v1/loudness/move-to-makeup",
        "api/v1/end-at",
        "api/v1/stream",
        "api/v1/stream/disconnect",
        "stream/*",
    ];
    shape_matches(SHAPES, path)
}

pub fn shape_matches(shapes: &[&str], path: &str) -> bool {
    let p: Vec<&str> = path.split('/').collect();
    shapes.iter().any(|s| {
        let q: Vec<&str> = s.split('/').collect();
        q.len() == p.len() && q.iter().zip(&p).all(|(a, b)| *a == "*" || a == b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    #[test]
    fn the_makeup_tip_says_what_the_controller_does_with_the_real_numbers() {
        let up = makeup_tip_text(3.11, -14.0);
        println!("{up}");
        assert_eq!(up, "Tip: the loudness controller is adding +3.1 dB to reach -14 LUFS. That works, but the processing chain is meant to land close to the target by itself. Raise the chain's output (makeup) by about 3 dB, or press Move to makeup.");
        let down = makeup_tip_text(-4.46, -16.5);
        println!("{down}");
        assert!(
            down.contains("taking away 4.5 dB to reach -16.5 LUFS"),
            "{down}"
        );
        assert!(
            down.contains("Lower the chain's output (makeup) by about 4 dB"),
            "{down}"
        );
        assert!(!up.contains("makeup is off") && !up.to_lowercase().contains("warning"));
    }

    /// A WebSocket client that stops reading is cut off when its JSON queue overflows, and everything it held
    /// (the connection's thread, the hub entry) is released at once, not after the 120 s write timeout.
    #[test]
    fn a_client_that_stops_reading_is_released_when_cut_off() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        // The client: a small receive buffer (so the server's writes block, as behind a real stuck client),
        // connected, never reads, never closes (until the end of the test).
        let fd = rustix::net::socket(
            rustix::net::AddressFamily::INET,
            rustix::net::SocketType::STREAM,
            None,
        )
        .unwrap();
        rustix::net::sockopt::set_socket_recv_buffer_size(&fd, 4096).unwrap();
        rustix::net::connect(&fd, &addr).unwrap();
        let client = TcpStream::from(fd);
        let (server_side, _) = l.accept().unwrap();
        let hub = Arc::new(crate::ws::Hub::default());
        let snap: Arc<dyn Fn(u64) -> String + Send + Sync> = Arc::new(|_| "{}".to_string());
        let h2 = hub.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            serve_ws(h2, snap, server_side, vec!["now".into()]);
            let _ = done_tx.send(());
        });
        let t = Instant::now();
        while hub.is_empty() {
            assert!(t.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        // First fill the socket buffers at a pace the writer keeps up with, so that it ends up blocked in a
        // write; then far more than the queue holds.
        let big = "x".repeat(64 * 1024);
        for _ in 0..200 {
            hub.send_json("now", &big);
            std::thread::sleep(Duration::from_millis(2));
        }
        let t = Instant::now();
        for _ in 0..(crate::ws::JSON_QUEUE * 2) {
            hub.send_json("now", &big);
        }
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "sending never waits on a client: {:?}",
            t.elapsed()
        );
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("serve_ws returned");
        assert!(hub.is_empty(), "the hub let the client go");
        drop(client);
    }
}
