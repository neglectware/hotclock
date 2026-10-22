//! The Stations page's routes on the library service (see [`crate::stations_admin`]): list, create, demo, rename,
//! delete, start and stop. All of them are under `/api/v1/station-admin`.
//!
//! **Where an authorization layer wraps (none exists yet):** every route of this file goes through the one function
//! [`LibraryApi::station_admin`], the single place to put a login check. Writes go only through
//! [`crate::settings::write_atomic`] and [`crate::settings::unit_action`].

use crate::http::{client, Request, Response};
use crate::library_http::LibraryApi;
use crate::settings as st;
use crate::stations_admin as sa;
use common::config::{GlobalConfig, StationConfig};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn start_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("HOTCLOCK_STATION_START_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    )
}

fn invalid(errors: Vec<(&str, String)>) -> Response {
    let msg = errors
        .iter()
        .map(|(k, m)| format!("{k}: {m}"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut v = json!({"error": {"code": "invalid_station", "message": msg}});
    v["errors"] = json!(errors
        .iter()
        .map(|(k, m)| json!({"key": k, "message": m}))
        .collect::<Vec<_>>());
    Response::json(400, &v)
}

fn busy() -> Response {
    Response::error(
        409,
        "busy",
        "another station change is running; wait for it to finish",
    )
}

fn today() -> String {
    library::util::now_iso().chars().take(10).collect()
}

impl LibraryApi {
    fn data_dir(&self) -> PathBuf {
        self.opts
            .paths
            .db
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf()
    }

    pub(crate) fn state_dir(&self, name: &str) -> PathBuf {
        self.data_dir().join("stations").join(name)
    }

    fn archive_dir(&self) -> PathBuf {
        self.data_dir().join("archive")
    }

    fn toml_path(&self, name: &str) -> PathBuf {
        self.opts.stations_dir.join(format!("{name}.toml"))
    }

    fn library_port(&self) -> Option<u16> {
        GlobalConfig::load_or_default(&self.opts.global_config)
            .ok()
            .and_then(|g| g.library.listen.parse::<std::net::SocketAddr>().ok())
            .map(|a| a.port())
    }

    /// Ask the status display to read the station list again (it looks only when it starts). `Some(true)` when it
    /// was running and has been restarted.
    fn refresh_display(&self) -> Option<bool> {
        let unit = "hotclock-display.service";
        (st::unit_state(unit).as_deref() == Some("active")).then(|| st::restart_unit(unit).is_ok())
    }

    /// Wait for a station's `/health` to answer 200. `Err` carries the reason.
    fn wait_healthy(&self, unit: &str, addr: &str, limit: Duration) -> Result<f64, String> {
        let t = Instant::now();
        let mut last = String::from("no answer");
        while t.elapsed() < limit {
            match client::request(addr, "GET", "/health", &[], b"", Duration::from_secs(2)) {
                Ok(r) if r.status == 200 => return Ok(t.elapsed().as_secs_f64()),
                Ok(r) => last = format!("/health answered {}", r.status),
                Err(e) => last = format!("no answer ({e})"),
            }
            if t.elapsed() > Duration::from_millis(2500) {
                if let Some(s) = st::unit_state(unit) {
                    if s == "failed" || s == "inactive" {
                        return Err(format!("the unit is {s}"));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!(
            "/health did not answer 200 within {} s ({last})",
            limit.as_secs()
        ))
    }

    fn row(&self, name: &str, path: &Path) -> Value {
        let state = st::unit_state(&sa::unit_of(name));
        let has_state = self.state_dir(name).exists();
        match StationConfig::load(path) {
            Ok(c) => {
                let port = c
                    .listen
                    .rsplit(':')
                    .next()
                    .and_then(|x| x.parse::<u16>().ok());
                let card = c.device != "null" && !c.device.starts_with("file:");
                let output = match (card, c.stream.enabled) {
                    (true, true) => "both",
                    (true, false) => "card",
                    (false, true) => "stream",
                    (false, false) => "none",
                };
                let health = port.and_then(|p| {
                    client::request(
                        &format!("127.0.0.1:{p}"),
                        "GET",
                        "/health",
                        &[],
                        b"",
                        Duration::from_millis(700),
                    )
                    .ok()
                    .map(|r| json!({"http": r.status, "status": r.json()["status"], "state": r.json()["state"]}))
                });
                json!({
                    "id": name, "display_name": c.display_name.clone().unwrap_or_else(|| name.to_string()),
                    "listen": c.listen, "port": port, "device": c.device, "output": output,
                    "stream": {"enabled": c.stream.enabled, "path": format!("/stream/{}.ogg", c.stream.mount)},
                    "default_playlist": c.default_playlist, "emergency_playlist": c.emergency_playlist,
                    "transition_preset": c.transition_preset, "processing_preset": c.processing_preset,
                    "loudness": c.loudness.enabled, "unit_state": state, "health": health, "has_state": has_state,
                    "file": path.display().to_string(),
                })
            }
            Err(e) => json!({
                "id": name, "display_name": name, "unit_state": state, "has_state": has_state,
                "file": path.display().to_string(),
                "error": e.0.iter().map(|x| format!("{}: {}", x.key, x.message)).collect::<Vec<_>>().join("; "),
            }),
        }
    }

    fn archived(&self) -> Vec<Value> {
        let mut v = vec![];
        for e in std::fs::read_dir(self.archive_dir())
            .into_iter()
            .flatten()
            .flatten()
        {
            let f = e.file_name().to_string_lossy().to_string();
            // <name>-YYYYMMDD-HHMMSS
            let mut parts = f.rsplitn(3, '-');
            let (time, date, name) = (parts.next(), parts.next(), parts.next());
            if let (Some(time), Some(date), Some(name)) = (time, date, name) {
                v.push(json!({"name": name, "stamp": format!("{date}-{time}"), "dir": e.path().display().to_string()}));
            }
        }
        v.sort_by_key(|x| x["stamp"].as_str().unwrap_or("").to_string());
        v
    }

    /// `GET /api/v1/station-admin`.
    fn station_admin_list(&self) -> Response {
        let mut stations = vec![];
        for (name, path) in self.station_files() {
            stations.push(self.row(&name, &path));
        }
        let playlists: Vec<String> = self
            .lib()
            .ok()
            .and_then(|l| l.playlists().ok())
            .map(|v| v.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        let proc = std::env::var("HOTCLOCK_PROC_ASOUND").unwrap_or_else(|_| "/proc/asound".into());
        let probe = self.opts.stations_dir.join(".probe");
        Response::json(
            200,
            &json!({
                "stations": stations,
                "archived": self.archived(),
                "next_port": sa::next_port(&self.opts.stations_dir, self.library_port()),
                "library_port": self.library_port(),
                "devices": st::list_devices(Path::new(&proc)),
                "playlists": playlists,
                "presets": common::transition::BUILTIN_PRESETS.iter().map(|(n, _, d)| json!({"name": n, "description": d})).collect::<Vec<_>>(),
                "processing_presets": sa::PROCESSING_PRESETS,
                "reserved": sa::RESERVED,
                "max_name": sa::MAX_NAME,
                "manageable": st::service_manager_available(),
                "writable": st::can_write(&probe),
            }),
        )
    }

    /// The single entry for every route under `/api/v1/station-admin` (the wrap point for a future login).
    pub(crate) fn station_admin(&self, req: &mut Request, tail: &[&str]) -> Response {
        match (req.method.as_str(), tail) {
            ("GET", []) => self.station_admin_list(),
            ("POST", []) => self.station_create(req),
            ("POST", ["demo"]) => self.station_demo(),
            ("POST", [name, "rename"]) => self.station_rename(req, name),
            ("POST", [name, verb @ ("start" | "stop")]) => self.station_power(name, verb),
            ("DELETE", [name]) => self.station_delete(req, name),
            (_, []) | (_, ["demo"]) | (_, [_]) | (_, [_, _]) => {
                Response::error(405, "method_not_allowed", "that method is not allowed here")
            }
            _ => Response::error(404, "not_found", "no such endpoint"),
        }
    }

    fn station_power(&self, name: &str, verb: &str) -> Response {
        if sa::name_problem(name).is_some() || !self.toml_path(name).exists() {
            return Response::error(404, "not_found", &format!("no station `{name}`"));
        }
        match st::unit_action(verb, &sa::unit_of(name)) {
            Ok(()) => Response::json(
                200,
                &json!({"station": name, "action": verb, "unit_state": st::unit_state(&sa::unit_of(name))}),
            ),
            Err(e) => Response::error(502, "systemctl_failed", &e),
        }
    }

    // ---- create ----------------------------------------------------------------------------------------------

    fn parse_spec(&self, body: &Value) -> Result<sa::Spec, Response> {
        let mut errs: Vec<(&str, String)> = vec![];
        let text = |k: &str| body[k].as_str().map(|s| s.trim().to_string());
        let name = text("name").unwrap_or_default();
        if let Some(p) = sa::name_problem(&name) {
            errs.push(("name", p));
        } else if self.toml_path(&name).exists() {
            errs.push(("name", format!("a station named `{name}` already exists")));
        }
        let display_name = text("display_name")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.clone());
        if display_name.chars().count() > 60 || display_name.chars().any(char::is_control) {
            errs.push((
                "display_name",
                "at most 60 characters, no control characters".into(),
            ));
        }
        let output = text("output").unwrap_or_else(|| "stream".into());
        let (card, stream) = match output.as_str() {
            "card" => (true, false),
            "stream" => (false, true),
            "both" => (true, true),
            _ => {
                errs.push(("output", "`card`, `stream` or `both`".into()));
                (false, true)
            }
        };
        let mut device = "null".to_string();
        if card {
            match text("device").filter(|d| !d.is_empty() && d != "null") {
                Some(d) if !d.chars().any(char::is_control) => device = d,
                _ => errs.push(("device", "choose a sound card".into())),
            }
        }
        let libp = self.library_port();
        let port = match body["port"].as_u64() {
            None => sa::next_port(&self.opts.stations_dir, libp),
            Some(p) if !(1024..=65535).contains(&p) => {
                errs.push(("port", "between 1024 and 65535".into()));
                0
            }
            Some(p) => {
                let p = p as u16;
                if Some(p) == libp {
                    errs.push(("port", "that is the library service's port".into()));
                } else if let Some((other, _)) = sa::ports_in_use(&self.opts.stations_dir)
                    .into_iter()
                    .find(|x| x.1 == p)
                {
                    errs.push(("port", format!("station `{other}` already uses port {p}")));
                } else if sa::port_busy(p) {
                    errs.push((
                        "port",
                        format!("something on this box already uses port {p}"),
                    ));
                }
                p
            }
        };
        let playlists: Vec<String> = self
            .lib()
            .ok()
            .and_then(|l| l.playlists().ok())
            .map(|v| v.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        let mut pl = |k: &'static str| -> Option<String> {
            let v = text(k).filter(|s| !s.is_empty())?;
            if !playlists.contains(&v) {
                errs.push((k, format!("no playlist named `{v}`")));
            }
            Some(v)
        };
        let default_playlist = pl("default_playlist");
        let emergency_playlist = pl("emergency_playlist");
        let transition_preset = text("transition_preset").unwrap_or_else(|| "plexamp".into());
        if !common::config::TRANSITION_PRESETS.contains(&transition_preset.as_str()) {
            errs.push((
                "transition_preset",
                format!("one of {}", common::config::TRANSITION_PRESETS.join(", ")),
            ));
        }
        let processing_preset = text("processing_preset").unwrap_or_else(|| "bypass".into());
        if !sa::PROCESSING_PRESETS.contains(&processing_preset.as_str()) {
            errs.push((
                "processing_preset",
                format!("one of {}", sa::PROCESSING_PRESETS.join(", ")),
            ));
        }
        if !errs.is_empty() {
            return Err(invalid(errs));
        }
        Ok(sa::Spec {
            name,
            display_name,
            device,
            stream,
            port,
            default_playlist,
            emergency_playlist,
            transition_preset,
            processing_preset,
            loudness: body["loudness"].as_bool().unwrap_or(false),
            expect_listener: body["expect_listener"].as_bool().unwrap_or(false),
        })
    }

    /// `POST /api/v1/station-admin`.
    fn station_create(&self, req: &mut Request) -> Response {
        let body = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let spec = match self.parse_spec(&body) {
            Ok(s) => s,
            Err(r) => return r,
        };
        self.create_core(spec, req.header("host"))
    }

    fn create_core(&self, spec: sa::Spec, host: Option<&str>) -> Response {
        if !st::service_manager_available() {
            return Response::error(409, "no_service_manager", "this box has no systemd, so the page cannot start a station; write the file by hand (see deploy.md)");
        }
        let path = self.toml_path(&spec.name);
        if !st::can_write(&path) {
            return Response::error(409, "not_writable", "the service user cannot write the stations directory (see deploy.md, \"Stations from the web page\")");
        }
        let Ok(_guard) = sa::BUSY.try_lock() else {
            return busy();
        };
        let text = match sa::render_toml(&spec, &today()) {
            Ok(t) => t,
            Err(e) => return Response::error(500, "internal", &e),
        };
        if let Err(e) = StationConfig::parse(&text, &path) {
            let msg =
                e.0.iter()
                    .map(|x| format!("{}: {}", x.key, x.message))
                    .collect::<Vec<_>>()
                    .join("; ");
            return Response::error(
                500,
                "internal",
                &format!("the new file does not pass the parser: {msg}"),
            );
        }
        let mut warnings: Vec<String> = vec![];
        if self.state_dir(&spec.name).exists() {
            warnings.push(format!(
                "an earlier station called `{}` left data in {}; the new station continues from it",
                spec.name,
                self.state_dir(&spec.name).display()
            ));
        }
        if spec.default_playlist.is_none() {
            warnings.push("no default playlist: the station plays its emergency playlist or silence until you start a playlist on the On Air page".into());
        }
        let mut undo = sa::Undo::default();
        if let Err(e) = st::write_atomic(&path, &text) {
            return Response::error(500, "write_failed", &e);
        }
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664));
        }
        let p2 = path.clone();
        undo.push("remove the station file", move || {
            std::fs::remove_file(&p2).map_err(|e| e.to_string())
        });
        let unit = sa::unit_of(&spec.name);
        let addr = format!("127.0.0.1:{}", spec.port);
        let started = st::unit_action("start", &unit)
            .and_then(|()| self.wait_healthy(&unit, &addr, start_timeout()));
        let secs = match started {
            Ok(s) => s,
            Err(e) => {
                let _ = st::unit_action("stop", &unit);
                let (failed, _) = undo.rollback();
                let mut msg = format!(
                    "the station did not come up ({e}); nothing was kept. See: journalctl -u {unit}"
                );
                if !failed.is_empty() {
                    msg.push_str(&format!(" Could not undo: {}", failed.join("; ")));
                }
                return Response::error(502, "start_failed", &msg);
            }
        };
        undo.commit();
        let display_restarted = self.refresh_display();
        let row = self.row(&spec.name, &path);
        let stream_url = spec.stream.then(|| {
            let h = host
                .and_then(|h| h.rsplit_once(':').map(|x| x.0).or(Some(h)))
                .unwrap_or("localhost");
            format!("http://{h}:{}/stream/program.ogg", spec.port)
        });
        Response::json(
            201,
            &json!({"created": spec.name, "station": row, "seconds_to_healthy": secs, "stream_url": stream_url,
                    "display_restarted": display_restarted, "warnings": warnings}),
        )
    }

    // ---- the demo station --------------------------------------------------------------------------------------

    /// Six generated tones and two playlists, once; the names of the playlists.
    fn ensure_demo_content(&self) -> Result<(String, String), String> {
        const MAIN: &str = "Demo tones";
        const EMERGENCY: &str = "Demo tones emergency";
        let lib = self
            .lib()
            .map_err(|_| "cannot open the library".to_string())?;
        if lib.playlist_id(MAIN).map_err(|e| e.to_string())?.is_some()
            && lib
                .playlist_id(EMERGENCY)
                .map_err(|e| e.to_string())?
                .is_some()
        {
            return Ok((MAIN.into(), EMERGENCY.into()));
        }
        let dir = self
            .opts
            .paths
            .upload_tmp
            .join(format!("demo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let notes = [
            ("C4", 261.63),
            ("D4", 293.66),
            ("E4", 329.63),
            ("G4", 392.0),
            ("A4", 440.0),
            ("C5", 523.25),
        ];
        let mut jobs = vec![];
        for (i, (n, hz)) in notes.iter().enumerate() {
            let p = dir.join(format!("demo-tone-{}-{n}.wav", i + 1));
            engine::render::render_tone(&engine::render::ToneRender {
                out: p.clone(),
                rate: 48000,
                seconds: 30.0,
                hz: *hz,
                level_dbfs: -20.0,
                format: engine::render::parse_wav_format("s16").ok_or("no wav format")?,
            })?;
            jobs.push((
                p,
                library::ImportOptions {
                    category: Some("music".into()),
                    source: "demo".into(),
                },
            ));
        }
        let results = library::import::import_many(&self.opts.paths, jobs, 2);
        let _ = std::fs::remove_dir_all(&dir);
        let mut ids = vec![];
        for (i, (_, out)) in results.into_iter().enumerate() {
            match out {
                library::ImportOutcome::Done { item_id, .. }
                | library::ImportOutcome::Duplicate { item_id } => {
                    let _ = lib.set_item_title(
                        item_id,
                        &format!("Demo tone {} ({}, {:.0} Hz)", i + 1, notes[i].0, notes[i].1),
                    );
                    ids.push(item_id);
                }
                library::ImportOutcome::Failed { error } => {
                    return Err(format!("importing the demo tones: {error}"))
                }
            }
        }
        for (name, mode, take) in [
            (MAIN, library::playlists::Mode::Shuffle, ids.len()),
            (EMERGENCY, library::playlists::Mode::Sequential, 2),
        ] {
            if lib.playlist_id(name).map_err(|e| e.to_string())?.is_some() {
                continue;
            }
            let mut p = library::playlists::NewPlaylist::shuffle(name);
            p.mode = mode;
            p.no_repeat_minutes = 0;
            let pid = lib.create_playlist(&p).map_err(|e| e.to_string())?;
            for id in ids.iter().take(take) {
                lib.add_item_to_playlist(pid, *id, 1.0)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok((MAIN.into(), EMERGENCY.into()))
    }

    /// `POST /api/v1/station-admin/demo`: a stream-only station on generated test tones.
    fn station_demo(&self) -> Response {
        let (main, emergency) = match self.ensure_demo_content() {
            Ok(x) => x,
            Err(e) => return Response::error(500, "demo_failed", &e),
        };
        let name = std::iter::once("demo".to_string())
            .chain((2..100).map(|i| format!("demo-{i}")))
            .find(|n| !self.toml_path(n).exists())
            .unwrap_or_else(|| "demo".into());
        let spec = sa::Spec {
            name,
            display_name: "Demo station".into(),
            device: "null".into(),
            stream: true,
            port: sa::next_port(&self.opts.stations_dir, self.library_port()),
            default_playlist: Some(main),
            emergency_playlist: Some(emergency),
            transition_preset: "plexamp".into(),
            processing_preset: "bypass".into(),
            loudness: false,
            expect_listener: false,
        };
        self.create_core(spec, None)
    }

    // ---- rename ----------------------------------------------------------------------------------------------

    fn rename_plan(&self, old: &str, new: &str) -> Result<Value, Response> {
        let old_path = self.toml_path(old);
        if sa::name_problem(old).is_some() || !old_path.exists() {
            return Err(Response::error(
                404,
                "not_found",
                &format!("no station `{old}`"),
            ));
        }
        if let Some(p) = sa::name_problem(new) {
            return Err(invalid(vec![("new_name", p)]));
        }
        if new == old {
            return Err(invalid(vec![(
                "new_name",
                "that is already the station's name".into(),
            )]));
        }
        let cfg = StationConfig::load(&old_path).map_err(|e| {
            Response::error(
                409,
                "bad_station_file",
                &format!(
                    "the station's file does not pass the parser: {}",
                    e.0.iter()
                        .map(|x| format!("{}: {}", x.key, x.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )
        })?;
        let mut conflicts = vec![];
        if self.toml_path(new).exists() {
            conflicts.push(format!("a station named `{new}` already exists"));
        }
        if self.state_dir(new).exists() {
            conflicts.push(format!(
                "{} exists (data of an earlier station of that name)",
                self.state_dir(new).display()
            ));
        }
        if !sa::snapshot_files(&self.opts.paths.backup, new).is_empty() {
            conflicts.push("backup snapshots of an earlier station of that name exist".into());
        }
        let (refs_old, refs_new) = match self.lib() {
            Ok(l) => (
                sa::library_refs(l.conn(), old).unwrap_or_default(),
                sa::library_refs(l.conn(), new).unwrap_or_default(),
            ),
            Err(_) => (vec![], vec![]),
        };
        if !refs_new.is_empty() {
            conflicts.push(
                "the library already holds records (schedules, history) of a station of that name"
                    .into(),
            );
        }
        if !conflicts.is_empty() {
            return Err(invalid(vec![("new_name", conflicts.join("; "))]));
        }
        let meters = self.meters_current().stations.contains_key(old);
        let state = st::unit_state(&sa::unit_of(old));
        let running = state.as_deref() == Some("active");
        let snaps = sa::snapshot_files(&self.opts.paths.backup, old).len();
        let port = cfg.listen.rsplit(':').next().unwrap_or("").to_string();
        let stream_path = cfg
            .stream
            .enabled
            .then(|| format!("/stream/{}.ogg", cfg.stream.mount));
        Ok(json!({
            "from": old, "to": new, "running": running,
            "changes": {
                "unit": {"from": sa::unit_of(old), "to": sa::unit_of(new)},
                "file": {"from": old_path.display().to_string(), "to": self.toml_path(new).display().to_string(), "name_key": new},
                "data_dir": {"from": self.state_dir(old).display().to_string(), "to": self.state_dir(new).display().to_string(), "exists": self.state_dir(old).exists()},
                "snapshots": snaps,
                "library_rows": refs_old.iter().map(|(t, n)| json!({"table": t, "rows": n})).collect::<Vec<_>>(),
                "meter_override": meters,
                "api_path": {"from": format!("/api/v1/stations/{old}/"), "to": format!("/api/v1/stations/{new}/")},
            },
            "unchanged": {"port": port, "listen": cfg.listen, "stream_path": stream_path, "display_name": cfg.display_name.clone().unwrap_or_else(|| old.to_string())},
            "notes": [
                "The station is stopped, renamed and started again. It is off the air for a few seconds and comes back on the same item at the same place.",
                format!("Its port ({port}) does not change, so a program that reads the stream directly (OBS) keeps working."),
                format!("Anything that reaches the station through the library, as /api/v1/stations/{old}/..., must use /api/v1/stations/{new}/... from now on (Stream Deck buttons, scripts)."),
                "Monitoring that watches the unit name hotclock-station@".to_string() + old + " must watch the new one."
            ],
        }))
    }

    /// `POST /api/v1/station-admin/{name}/rename`, body `{"new_name": "...", "dry_run": false}`.
    fn station_rename(&self, req: &mut Request, old: &str) -> Response {
        let body = match req.json() {
            Ok(b) => b,
            Err(r) => return r,
        };
        let new = body["new_name"].as_str().unwrap_or("").trim().to_string();
        let plan = match self.rename_plan(old, &new) {
            Ok(p) => p,
            Err(r) => return r,
        };
        if body["dry_run"].as_bool().unwrap_or(false) {
            return Response::json(200, &json!({"dry_run": true, "plan": plan}));
        }
        if !st::service_manager_available() {
            return Response::error(409, "no_service_manager", "this box has no systemd");
        }
        if !st::can_write(&self.toml_path(&new)) {
            return Response::error(
                409,
                "not_writable",
                "the service user cannot write the stations directory (see deploy.md)",
            );
        }
        let Ok(_guard) = sa::BUSY.try_lock() else {
            return busy();
        };
        let was_active = plan["running"].as_bool().unwrap_or(false);
        let mut undo = sa::Undo::default();
        let t0 = Instant::now();
        match self.rename_steps(old, &new, &mut undo) {
            Ok(secs) => {
                undo.commit();
                // the meters' map changed; tell the displays
                let all = self.meters_current().to_json();
                self.hub.send_with("display", &|seq| {
                    json!({"v": 1, "type": "display_meters", "seq": seq, "data": all}).to_string()
                });
                let display_restarted = self.refresh_display();
                Response::json(
                    200,
                    &json!({"renamed": {"from": old, "to": new}, "station": self.row(&new, &self.toml_path(&new)),
                            "seconds_to_healthy": secs, "seconds_total": t0.elapsed().as_secs_f64(),
                            "display_restarted": display_restarted, "plan": plan}),
                )
            }
            Err(e) => {
                let (failed, undone) = undo.rollback();
                // The old station, as it was.
                let mut restored = Value::Null;
                if was_active {
                    let unit = sa::unit_of(old);
                    let addr = self.addr_of_file(old);
                    restored = json!(st::unit_action("start", &unit)
                        .and_then(|()| addr
                            .ok_or_else(|| "no address".to_string())
                            .and_then(|a| self.wait_healthy(&unit, &a, Duration::from_secs(30))))
                        .is_ok());
                }
                let mut v = json!({"error": {"code": "rename_failed", "message": format!("the rename failed at: {e}. Everything was put back.")},
                    "undone": undone, "old_station_running_again": restored});
                if !failed.is_empty() {
                    v["error"]["message"] = json!(format!(
                        "the rename failed at: {e}. These could NOT be put back: {}",
                        failed.join("; ")
                    ));
                    v["not_undone"] = json!(failed);
                }
                Response::json(500, &v)
            }
        }
    }

    fn addr_of_file(&self, name: &str) -> Option<String> {
        let c = StationConfig::load(&self.toml_path(name)).ok()?;
        Some(format!("127.0.0.1:{}", c.listen.rsplit(':').next()?))
    }

    /// The steps of a rename. Each one that changes something pushes its own undo first.
    fn rename_steps(&self, old: &str, new: &str, undo: &mut sa::Undo) -> Result<f64, String> {
        let (old_path, new_path) = (self.toml_path(old), self.toml_path(new));
        let old_unit = sa::unit_of(old);
        // 1. stop
        st::unit_action("stop", &old_unit).map_err(|e| format!("stopping the station: {e}"))?;
        // 2. the file, with its `name` key
        let old_text =
            std::fs::read_to_string(&old_path).map_err(|e| format!("reading the file: {e}"))?;
        let mut doc = st::read_doc(&old_path)?;
        doc["name"] = toml_edit::value(new);
        let new_text = doc.to_string();
        StationConfig::parse(&new_text, &new_path).map_err(|e| {
            format!(
                "the renamed file does not pass the parser: {:?}",
                e.0.first().map(|x| x.message.clone())
            )
        })?;
        st::write_atomic(&new_path, &new_text)?;
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o664));
        }
        let (op, np, ot) = (old_path.clone(), new_path.clone(), old_text.clone());
        let bak_old = PathBuf::from(format!("{}.bak", old_path.display()));
        let bak_new = PathBuf::from(format!("{}.bak", new_path.display()));
        std::fs::remove_file(&old_path).map_err(|e| format!("removing the old file: {e}"))?;
        let bak_moved = bak_old.exists() && std::fs::rename(&bak_old, &bak_new).is_ok();
        undo.push("put the station file back under its old name", move || {
            std::fs::write(&op, ot).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&np);
            Ok(())
        });
        if bak_moved {
            undo.push("put the .bak back", move || {
                std::fs::rename(&bak_new, &bak_old).map_err(|e| e.to_string())
            });
        }
        // 3. the data directory
        let (sd_old, sd_new) = (self.state_dir(old), self.state_dir(new));
        if sd_old.exists() {
            sa::move_path(&sd_old, &sd_new)?;
            undo.push("move the data directory back", move || {
                sa::move_path(&sd_new, &sd_old)
            });
        }
        // 4. the nightly snapshots
        let backup = self.opts.paths.backup.clone();
        let moved = sa::rename_snapshots(&backup, old, new)?;
        if !moved.is_empty() {
            undo.push("rename the snapshots back", move || {
                for (o, n) in moved.iter().rev() {
                    std::fs::rename(n, o).map_err(|e| e.to_string())?;
                }
                Ok(())
            });
        }
        // 5. the library database
        {
            let lib = self
                .lib()
                .map_err(|_| "cannot open the library".to_string())?;
            sa::library_rename(lib.conn(), old, new)?;
            let (paths, o, n) = (self.opts.paths.clone(), old.to_string(), new.to_string());
            undo.push(
                "point the library records back at the old name",
                move || {
                    let l = library::Library::open_worker(paths).map_err(|e| e.to_string())?;
                    sa::library_rename(l.conn(), &n, &o)
                },
            );
            library::Library::bump_revision(lib.conn()).map_err(|e| e.to_string())?;
        }
        // 6. the meter ballistics override
        let mp = self.meters_path();
        if let Ok(text) = std::fs::read_to_string(&mp) {
            if let Ok(mut m) = serde_json::from_str::<Value>(&text) {
                if let Some(b) = m["stations"].as_object_mut().and_then(|o| o.remove(old)) {
                    m["stations"][new] = b;
                    let tmp = mp.with_extension("tmp");
                    std::fs::write(&tmp, m.to_string())
                        .and_then(|()| std::fs::rename(&tmp, &mp))
                        .map_err(|e| format!("the meter settings: {e}"))?;
                    let mp2 = mp.clone();
                    undo.push("restore the meter settings", move || {
                        std::fs::write(&mp2, text).map_err(|e| e.to_string())
                    });
                }
            }
        }
        // 7. start under the new name and wait
        let new_unit = sa::unit_of(new);
        let addr = self
            .addr_of_file(new)
            .ok_or("the new file has no address")?;
        let nu = new_unit.clone();
        undo.push("stop the station under the new name", move || {
            st::unit_action("stop", &nu)
        });
        st::unit_action("start", &new_unit).map_err(|e| format!("starting {new_unit}: {e}"))?;
        self.wait_healthy(&new_unit, &addr, start_timeout())
            .map_err(|e| format!("{new_unit} did not become healthy: {e}"))
    }

    // ---- delete ----------------------------------------------------------------------------------------------

    /// `DELETE /api/v1/station-admin/{name}?confirm=<name>&data=keep|delete`.
    fn station_delete(&self, req: &mut Request, name: &str) -> Response {
        let path = self.toml_path(name);
        if sa::name_problem(name).is_some() || !path.exists() {
            return Response::error(404, "not_found", &format!("no station `{name}`"));
        }
        if req.query_get("confirm").as_deref() != Some(name) {
            return invalid(vec![(
                "confirm",
                format!("type the station's short name, `{name}`, to confirm"),
            )]);
        }
        let data = req.query_get("data").unwrap_or_else(|| "keep".into());
        if data != "keep" && data != "delete" {
            return invalid(vec![("data", "`keep` or `delete`".into())]);
        }
        if !st::service_manager_available() {
            return Response::error(409, "no_service_manager", "this box has no systemd");
        }
        let Ok(_guard) = sa::BUSY.try_lock() else {
            return busy();
        };
        let was_active = st::unit_state(&sa::unit_of(name)).as_deref() == Some("active");
        let mut undo = sa::Undo::default();
        match self.delete_steps(name, &data, &mut undo) {
            Ok(v) => {
                undo.commit();
                let all = self.meters_current().to_json();
                self.hub.send_with("display", &|seq| {
                    json!({"v": 1, "type": "display_meters", "seq": seq, "data": all}).to_string()
                });
                let display_restarted = self.refresh_display();
                let mut v = v;
                v["display_restarted"] = json!(display_restarted);
                Response::json(200, &v)
            }
            Err(e) => {
                let (failed, undone) = undo.rollback();
                let mut restored = Value::Null;
                if was_active && failed.is_empty() {
                    restored = json!(st::unit_action("start", &sa::unit_of(name)).is_ok());
                }
                let mut msg = format!("the delete failed at: {e}. Everything was put back.");
                if !failed.is_empty() {
                    msg = format!(
                        "the delete failed at: {e}. These could NOT be put back: {}",
                        failed.join("; ")
                    );
                }
                Response::json(
                    500,
                    &json!({"error": {"code": "delete_failed", "message": msg}, "undone": undone, "station_running_again": restored}),
                )
            }
        }
    }

    fn delete_steps(&self, name: &str, data: &str, undo: &mut sa::Undo) -> Result<Value, String> {
        let path = self.toml_path(name);
        st::unit_action("stop", &sa::unit_of(name))
            .map_err(|e| format!("stopping the station: {e}"))?;
        let stamp = sa::stamp(&library::util::now_iso());
        let arch = self.archive_dir().join(format!("{name}-{stamp}"));
        std::fs::create_dir_all(&arch).map_err(|e| format!("creating {}: {e}", arch.display()))?;
        let a2 = arch.clone();
        undo.push("remove the archive directory", move || {
            std::fs::remove_dir_all(&a2).map_err(|e| e.to_string())
        });
        // the file (copy, then remove: /etc and /var/lib may be different file systems)
        let (file_in_arch, p2) = (arch.join(format!("{name}.toml")), path.clone());
        std::fs::copy(&path, &file_in_arch)
            .map_err(|e| format!("copying the station file: {e}"))?;
        std::fs::remove_file(&path).map_err(|e| format!("removing the station file: {e}"))?;
        let fa = file_in_arch.clone();
        undo.push("put the station file back", move || {
            std::fs::copy(&fa, &p2)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        if bak.exists() {
            let _ = std::fs::copy(&bak, arch.join(format!("{name}.toml.bak")));
            let _ = std::fs::remove_file(&bak);
        }
        // the data directory
        let sd = self.state_dir(name);
        if sd.exists() {
            let dst = arch.join("state");
            sa::move_path(&sd, &dst)?;
            undo.push("move the data directory back", move || {
                sa::move_path(&dst, &sd)
            });
        }
        // the library records
        let mut library_rows = 0;
        {
            let lib = self
                .lib()
                .map_err(|_| "cannot open the library".to_string())?;
            library_rows = sa::library_refs(lib.conn(), name)
                .map(|v| v.iter().map(|x| x.1).sum::<i64>())
                .unwrap_or(0)
                .max(library_rows);
            if data == "delete" {
                sa::library_forget(lib.conn(), name)?;
                library::Library::bump_revision(lib.conn()).map_err(|e| e.to_string())?;
            } else {
                sa::display_view_clear(lib.conn(), name).map_err(|e| e.to_string())?;
            }
        }
        let mut removed_snapshots = 0;
        let mut warnings: Vec<String> = vec![];
        if data == "delete" {
            // from here on nothing can be put back, and the steps above are all done
            undo.commit();
            for f in sa::snapshot_files(&self.opts.paths.backup, name) {
                if std::fs::remove_file(&f).is_ok() {
                    removed_snapshots += 1;
                }
            }
            if let Err(e) = std::fs::remove_dir_all(&arch) {
                warnings.push(format!("could not remove {}: {e}", arch.display()));
            }
            let mp = self.meters_path();
            if let Ok(text) = std::fs::read_to_string(&mp) {
                if let Ok(mut m) = serde_json::from_str::<Value>(&text) {
                    if m["stations"]
                        .as_object_mut()
                        .and_then(|o| o.remove(name))
                        .is_some()
                    {
                        let _ = std::fs::write(&mp, m.to_string());
                    }
                }
            }
        }
        Ok(json!({
            "deleted": name, "data": data,
            "archive": (data == "keep").then(|| arch.display().to_string()),
            "library_rows": library_rows, "snapshots_removed": removed_snapshots,
            "kept": if data == "keep" { json!(["its configuration file and data directory (in the archive)", "its backup snapshots", "its schedule and history records in the library"]) } else { json!([]) },
            "warnings": warnings,
        }))
    }
}
