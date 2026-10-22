//! The live display: the station and library clients, the model of every station, the view, and the 30 Hz
//! loop that renders and presents. The output is behind a trait, so the loop runs the same against KMS on the
//! box and against a recording double in the tests.
//!
//! Everything the display shows comes over the WebSockets of the stations (11.3) and the library service's
//! `display` topic. It writes nothing but its own last view (`view.json` in its state directory), so a failure
//! here can never touch the audio.

use crate::canvas::Color;
use crate::layout::Plan;
use crate::model::StationModel;
use crate::net::{self, Event, Job};
use crate::output::Output;
use crate::render::Renderer;
use crate::scene::{BarInfo, SceneInput, Shown};
use crate::view::{Presenter, ViewMode, ViewSettings};
use common::config::{DisplayConfig, GlobalConfig, StationConfig};
use common::meters::MeterSettings;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FRAME_INTERVAL: Duration = Duration::from_micros(33_333);

#[derive(Clone, Debug)]
pub struct StationSpec {
    pub id: String,
    pub display_name: String,
    pub addr: String,
}

#[derive(Clone)]
pub struct Config {
    pub display: DisplayConfig,
    pub stations: Vec<StationSpec>,
    pub library_addr: String,
    pub state_file: Option<PathBuf>,
    /// Ignore the library's view and always use this one (`hotclock display --view`, for measurements and tests).
    pub force_view: Option<ViewMode>,
    /// Minutes east of UTC; `None` asks the system (hourly).
    pub tz_min: Option<i64>,
    pub product: String,
}

/// Where the frames go; opened again after the monitor was lost.
pub trait OutputFactory: Send {
    fn open(&mut self, cfg: &DisplayConfig) -> Result<Box<dyn Output>, String>;
}

pub struct KmsFactory;

impl OutputFactory for KmsFactory {
    #[cfg(target_os = "linux")]
    fn open(&mut self, cfg: &DisplayConfig) -> Result<Box<dyn Output>, String> {
        crate::kms::Kms::open(cfg).map(|k| Box::new(k) as Box<dyn Output>)
    }
    #[cfg(not(target_os = "linux"))]
    fn open(&mut self, _cfg: &DisplayConfig) -> Result<Box<dyn Output>, String> {
        Err("the KMS output exists only on Linux".into())
    }
}

/// The stations configured on this box, in port order (the first is the left half of `split`).
pub fn discover(g: &GlobalConfig) -> Vec<StationSpec> {
    let mut found: Vec<(u16, StationSpec)> = vec![];
    if let Ok(rd) = std::fs::read_dir(&g.stations_dir) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                continue;
            }
            match StationConfig::load(&p) {
                Ok(c) => {
                    let port = c
                        .listen
                        .rsplit(':')
                        .next()
                        .and_then(|x| x.parse::<u16>().ok())
                        .unwrap_or(0);
                    found.push((
                        port,
                        StationSpec {
                            display_name: c.display_name.clone().unwrap_or_else(|| c.name.clone()),
                            addr: format!("127.0.0.1:{port}"),
                            id: c.name,
                        },
                    ));
                }
                Err(e) => tracing::warn!("display: skipping {}: {e}", p.display()),
            }
        }
    }
    found.sort_by_key(|(p, s)| (*p, s.id.clone()));
    found.into_iter().map(|(_, s)| s).collect()
}

pub fn library_addr(g: &GlobalConfig) -> String {
    let port = g.library.listen.rsplit(':').next().unwrap_or("8300");
    format!("127.0.0.1:{port}")
}

/// What the tests read of the last frame.
#[derive(Clone, Debug, Default)]
pub struct Introspect {
    pub frames: u64,
    pub shown: Option<Shown>,
    pub size: (i32, i32),
    /// `(zone, text, color)` of every glyph run of the last frame.
    pub runs: Vec<(String, String, Color)>,
    pub header_bg: Vec<(String, bool)>,
    pub connected: Vec<(String, bool)>,
    pub view: String,
    pub library_up: bool,
    pub output_up: bool,
    pub bars: Vec<(String, Option<f64>, i32)>,
    pub indicators: Vec<(String, Color)>,
    /// The preset of the meter ballistics in use.
    /// The ballistics preset of each station, in station order.
    pub ballistics: Vec<String>,
    /// Per frame: `(frame, shown station, its title on air, the first row of its queue)`; the display's own record of
    /// what it drew, for the tests (capped).
    pub title_log: Vec<(u64, String, String, String)>,
    /// Changes of the artwork a zone draws: `(frame, zone, url, loaded)`.
    pub art_log: Vec<(u64, String, String, bool)>,
}

pub struct App {
    cfg: Config,
    models: Vec<StationModel>,
    presenter: Presenter,
    rx: Receiver<Event>,
    jobs: Vec<Sender<Job>>,
    stop_threads: Arc<AtomicBool>,
    factory: Box<dyn OutputFactory>,
    out: Option<(Box<dyn Output>, Renderer)>,
    next_open: Instant,
    open_failures: u32,
    loaded: HashSet<String>,
    requested: HashMap<String, Instant>,
    chain_req: Vec<Instant>,
    last_prune: Instant,
    pub introspect: Option<Arc<Mutex<Introspect>>>,
    library_up: bool,
    last_saved: Option<ViewSettings>,
    pending_art: Vec<(String, image::RgbaImage)>,
    stats: Stats,
    rates: Vec<EventRate>,
    meter_settings: MeterSettings,
    snapshot: Option<(PathBuf, Instant)>,
    bar: BarInfo,
    loudness_req: Vec<Instant>,
}

/// Counts a station's events per second, to say so in the log when one type comes in a burst: a station that sends a
/// hundred `now` or `queue` events in 30 seconds is what a viewer sees as the screen flipping through songs, and the
/// journal should say which events did it (the display itself draws only what the model holds, once per frame).
#[derive(Default)]
pub struct EventRate {
    second: i64,
    counts: HashMap<String, u32>,
    /// The worst second of the current minute, by type.
    pub busiest: HashMap<String, u32>,
}

/// More than this many events of one of these types in a second is a burst.
pub const BURST: u32 = 5;
pub const BURST_TYPES: [&str; 4] = ["now", "queue", "snapshot", "state"];

impl EventRate {
    /// Count one event at `now_ms`; returns the type and its count when this event is the one that makes a burst
    /// (reported once per second per type).
    pub fn count(&mut self, typ: &str, now_ms: i64) -> Option<(String, u32)> {
        let sec = now_ms.div_euclid(1000);
        if sec != self.second {
            self.second = sec;
            self.counts.clear();
        }
        let c = self.counts.entry(typ.to_string()).or_default();
        *c += 1;
        let c = *c;
        let b = self.busiest.entry(typ.to_string()).or_default();
        *b = (*b).max(c);
        (c == BURST + 1 && BURST_TYPES.contains(&typ)).then(|| (typ.to_string(), c))
    }
}

/// A line in the log every minute: what was drawn and how much CPU the process used (acceptance 9).
struct Stats {
    at: Instant,
    cpu_s: f64,
    frames: u64,
    presents: u64,
}

fn cpu_seconds() -> f64 {
    // SAFETY: getrusage on the own process with a zeroed out-parameter.
    unsafe {
        let mut u: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut u);
        (u.ru_utime.tv_sec + u.ru_stime.tv_sec) as f64
            + (u.ru_utime.tv_usec + u.ru_stime.tv_usec) as f64 / 1e6
    }
}

fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl App {
    pub fn start(cfg: Config, factory: Box<dyn OutputFactory>) -> App {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut models = vec![];
        let mut jobs = vec![];
        for (i, s) in cfg.stations.iter().enumerate() {
            models.push(StationModel::new(&s.id, &s.display_name));
            let (jt, jr) = channel();
            jobs.push(jt);
            let (a, t) = (s.addr.clone(), tx.clone());
            std::thread::Builder::new()
                .name(format!("disp-http-{}", s.id))
                .spawn(move || net::worker_thread(i, a, jr, t))
                .ok();
            let (a, t, st) = (s.addr.clone(), tx.clone(), stop.clone());
            std::thread::Builder::new()
                .name(format!("disp-ws-{}", s.id))
                .spawn(move || net::station_thread(i, a, t, st))
                .ok();
        }
        let (a, t, st) = (cfg.library_addr.clone(), tx.clone(), stop.clone());
        std::thread::Builder::new()
            .name("disp-lib".into())
            .spawn(move || net::library_thread(a, t, st))
            .ok();
        let (a, t, st) = (cfg.library_addr.clone(), tx.clone(), stop.clone());
        std::thread::Builder::new()
            .name("disp-bar".into())
            .spawn(move || net::bar_thread(a, t, st))
            .ok();
        // the view: the last one saved, else the configured default
        let mut vs = ViewSettings {
            view: ViewMode::parse(&cfg.display.default_view).unwrap_or(ViewMode::Single),
            pin_on_alert: cfg.display.rotate_pin_on_alert,
            ..ViewSettings::default()
        };
        if let Some(p) = &cfg.state_file {
            if let Some(v) = std::fs::read_to_string(p)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                vs = ViewSettings::from_json(&v, &vs);
                tracing::info!(
                    "display: view {} restored from {}",
                    vs.view.name(),
                    p.display()
                );
            }
        }
        // what the config says wins over what an earlier run saved (the library's payload says it again when it arrives)
        vs.pin_on_alert = cfg.display.rotate_pin_on_alert;
        vs.rotate_processing = cfg.display.rotate_processing;
        if let Some(v) = cfg.force_view {
            vs.view = v;
        }
        let saved_meters = cfg
            .state_file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p.with_file_name("meters.json")).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| MeterSettings::from_json(&v).ok())
            .unwrap_or_default();
        for m in &mut models {
            m.dynamics.params = saved_meters.effective(&m.id).clone();
        }
        let cfg_len = cfg.stations.len();
        let last_saved = Some(vs.clone());
        let bar = BarInfo::new(&cfg.product, &cfg.display);
        App {
            cfg,
            models,
            presenter: Presenter::new(vs),
            rx,
            jobs,
            stop_threads: stop,
            factory,
            out: None,
            next_open: Instant::now(),
            open_failures: 0,
            loaded: HashSet::new(),
            requested: HashMap::new(),
            chain_req: vec![Instant::now() - Duration::from_secs(10); 8],
            last_prune: Instant::now(),
            introspect: None,
            library_up: false,
            last_saved,
            pending_art: vec![],
            rates: (0..cfg_len).map(|_| EventRate::default()).collect(),
            meter_settings: saved_meters,
            stats: Stats {
                at: Instant::now(),
                cpu_s: cpu_seconds(),
                frames: 0,
                presents: 0,
            },
            bar,
            loudness_req: vec![Instant::now(); cfg_len],
            snapshot: std::env::var_os("HOTCLOCK_DISPLAY_SNAPSHOT")
                .map(|p| (PathBuf::from(p), Instant::now() - Duration::from_secs(60))),
        }
    }

    pub fn models(&self) -> &[StationModel] {
        &self.models
    }

    pub fn settings(&self) -> &ViewSettings {
        &self.presenter.settings
    }

    fn handle(&mut self, ev: Event, now_ms: i64) {
        match ev {
            Event::Up(i) => {
                self.models[i].set_connected(true);
                self.models[i].chain_rev_seen = None;
                let _ = self.jobs[i].send(Job::History);
                let _ = self.jobs[i].send(Job::Chain);
                let _ = self.jobs[i].send(Job::Loudness);
                self.loudness_req[i] = Instant::now();
            }
            Event::Down(i) => self.models[i].set_connected(false),
            Event::Json(i, v) => {
                let typ = v["type"].as_str().unwrap_or("?").to_string();
                if let Some((t, n)) = self.rates[i].count(&typ, now_ms) {
                    tracing::warn!(
                        "display: station {}: more than {} `{t}` events in one second ({n} so far); now `{}`, next `{}`",
                        self.models[i].id,
                        BURST,
                        self.models[i].now.as_ref().map(|n| n.title.as_str()).unwrap_or(""),
                        self.models[i].queue.first().map(|e| e.title.as_str()).unwrap_or("")
                    );
                }
                self.models[i].apply_json(&v, now_ms);
                if v["type"] == "alert" || v["type"] == "alert_cleared" {
                    // the snapshot of alerts is authoritative after a gap; nothing more to do
                }
            }
            Event::Frame(i, f) => {
                self.models[i].apply_frame(*f, now_ms);
                if self.models[i].needs_chain()
                    && self.chain_req[i].elapsed() > Duration::from_millis(900)
                {
                    self.chain_req[i] = Instant::now();
                    let _ = self.jobs[i].send(Job::Chain);
                }
            }
            Event::Chain(i, v) => {
                if let Some(c) = crate::model::Chain::from_json(&v) {
                    // whatever revision the station answers with, the one the frames carry is what was asked for
                    let asked = self.models[i].frame.as_ref().map(|f| f.chain_cfg_rev);
                    self.models[i].chain_rev_seen = Some(asked.unwrap_or(c.rev));
                    self.models[i].chain = Some(c);
                }
            }
            Event::History(i, v) => self.models[i].set_history(&v),
            Event::Art(url, img) => {
                if let Some((_, r)) = self.out.as_mut() {
                    r.canvas.images.insert(&url, img);
                    self.loaded.insert(url);
                } else {
                    // no screen yet: keep it for when there is one
                    self.pending_art.push((url, img));
                }
            }
            Event::ArtFailed(url) => {
                tracing::debug!("display: artwork {url} failed");
            }
            Event::Library(v) => {
                self.library_up = true;
                if v.is_object() && self.cfg.force_view.is_none() {
                    let s = ViewSettings::from_json(&v, &self.presenter.settings);
                    self.presenter.set(s);
                    self.save_view();
                }
            }
            Event::Meters(v) => match MeterSettings::from_json(&v) {
                Ok(m) => self.set_meter_settings(m),
                Err(e) => tracing::warn!("display: ignoring meter settings: {e}"),
            },
            Event::LibraryDown => self.library_up = false,
            Event::Bar(v) => self.bar.update(&v),
            Event::Loudness(i, v) => self.models[i].set_loudness(&v),
        }
    }

    /// Apply the box's meter ballistics: each station's bars use that station's own setting (or the box default),
    /// live from the next frame; a copy is kept for a restart with the library down.
    fn set_meter_settings(&mut self, m: MeterSettings) {
        if self.meter_settings == m {
            return;
        }
        tracing::info!(
            "display: meter ballistics: default {}, {}",
            m.default.preset,
            if m.stations.is_empty() {
                "no station overrides".to_string()
            } else {
                m.stations
                    .iter()
                    .map(|(k, b)| format!("{k} {}", b.preset))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        if let Some(p) = &self.cfg.state_file {
            let _ = std::fs::write(p.with_file_name("meters.json"), m.to_json().to_string());
        }
        for st in &mut self.models {
            st.dynamics.params = m.effective(&st.id).clone();
        }
        self.meter_settings = m;
    }

    fn save_view(&mut self) {
        let s = self.presenter.settings.clone();
        if self.last_saved.as_ref() == Some(&s) {
            return;
        }
        if let Some(p) = &self.cfg.state_file {
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, s.to_json().to_string())
                .and_then(|()| std::fs::rename(&tmp, p))
                .is_err()
            {
                tracing::debug!("display: cannot save the view to {}", p.display());
            }
        }
        self.last_saved = Some(s);
    }

    /// Request the artwork the screen can need: the item on air and the first rows of each queue.
    fn want_art(&mut self) {
        let mut wanted: Vec<(usize, String)> = vec![];
        for (i, m) in self.models.iter().enumerate() {
            if !m.connected {
                continue;
            }
            if let Some(u) = m.now.as_ref().and_then(|n| n.art_url.as_ref()) {
                wanted.push((i, crate::scene::sized(u, 600)));
            }
            for e in m.queue.iter().take(6) {
                if let Some(u) = &e.art_url {
                    wanted.push((i, crate::scene::sized(u, 144)));
                }
            }
        }
        for (i, u) in &wanted {
            let due = self
                .requested
                .get(u)
                .is_none_or(|t| !self.loaded.contains(u) && t.elapsed() > Duration::from_secs(30));
            if due && !self.loaded.contains(u) {
                self.requested.insert(u.clone(), Instant::now());
                let _ = self.jobs[*i].send(Job::Art(u.clone()));
            }
        }
        if self.last_prune.elapsed() > Duration::from_secs(5) {
            self.last_prune = Instant::now();
            let keep: HashSet<&String> = wanted.iter().map(|(_, u)| u).collect();
            if let Some((_, r)) = self.out.as_mut() {
                r.canvas.images.retain(&|u| keep.contains(&u.to_string()));
            }
            self.loaded.retain(|u| keep.contains(u));
            self.requested.retain(|u, _| keep.contains(u));
        }
    }

    fn try_open(&mut self) {
        if self.out.is_some() || Instant::now() < self.next_open {
            return;
        }
        match self.factory.open(&self.cfg.display) {
            Ok(o) => {
                let (w, h) = o.size();
                let mut plan = Plan::new(
                    w,
                    h,
                    self.cfg.display.diagonal_in,
                    self.cfg.display.view_distance_cm,
                );
                if o.force_fallback_layout() {
                    plan = plan.with_fallback_layout();
                }
                if plan.degraded {
                    tracing::warn!(
                        "display: {w}x{h} on {} inches asks for a {} px minimum; the fallback layout draws 19 px, so this mode is degraded",
                        self.cfg.display.diagonal_in,
                        plan.min_px
                    );
                }
                tracing::info!(
                    "display: output up: {} ({w}x{h}, {:?} layout, minimum text {} px)",
                    o.describe(),
                    plan.variant,
                    plan.min_px
                );
                let mut r = Renderer::new(plan);
                self.loaded.clear();
                self.requested.clear();
                for (u, img) in self.pending_art.drain(..) {
                    r.canvas.images.insert(&u, img);
                    self.loaded.insert(u);
                }
                self.out = Some((o, r));
                self.open_failures = 0;
            }
            Err(e) => {
                // log the first failure and then once a minute; the stations play on regardless
                if self.open_failures == 0 || self.open_failures.is_multiple_of(30) {
                    tracing::warn!("display: no output: {e} (retrying every 2 s)");
                }
                self.open_failures += 1;
                self.next_open = Instant::now() + Duration::from_secs(2);
            }
        }
    }

    /// One 30 Hz tick: take the events that have arrived, draw, present.
    pub fn tick(&mut self) {
        let now_ms = wall_ms();
        while let Ok(ev) = self.rx.try_recv() {
            self.handle(ev, now_ms);
        }
        self.try_open();
        for m in &mut self.models {
            let live = m.live_frame(now_ms).cloned();
            m.dynamics.update(live.as_ref(), now_ms);
        }
        self.want_art();
        // the controller's gain range can change while the station runs (the loudness card applies it live)
        for (i, m) in self.models.iter().enumerate() {
            if m.connected && self.loudness_req[i].elapsed() > Duration::from_secs(30) {
                self.loudness_req[i] = Instant::now();
                let _ = self.jobs[i].send(Job::Loudness);
            }
        }
        let shown = if self.models.is_empty() {
            Shown::Single {
                idx: 0,
                other: None,
                rotate: None,
                pinned: false,
            }
        } else {
            self.presenter.shown(&self.models, now_ms)
        };
        let mut lost = false;
        if let Some((out, r)) = self.out.as_mut() {
            if !out.connected() {
                tracing::warn!("display: the monitor was disconnected; retrying every 2 s");
                lost = true;
            } else if !self.models.is_empty() {
                let tz = self
                    .cfg
                    .tz_min
                    .unwrap_or_else(|| common::localtime::utc_offset_min(now_ms));
                let m = (now_ms.div_euclid(1000) + tz * 60).rem_euclid(86_400);
                let clock = format!("{:02}:{:02}:{:02}", m / 3600, m / 60 % 60, m % 60);
                let notices: Vec<String> = out.alarm().into_iter().collect();
                let loaded = &self.loaded;
                let has = |u: &str| loaded.contains(u);
                let inp = SceneInput {
                    stations: &self.models,
                    shown: shown.clone(),
                    now_ms,
                    clock,
                    tz_min: tz,
                    product: self.cfg.product.clone(),
                    cfg: &self.cfg.display,
                    plan: r.plan,
                    has_image: &has,
                    notices: &notices,
                    bar: &self.bar,
                };
                let damage = r.frame(&inp);
                self.stats.frames += 1;
                if !damage.is_empty() {
                    self.stats.presents += 1;
                    if let Err(e) = out.present(&r.canvas, &damage) {
                        tracing::warn!("display: output failed: {e}; reopening");
                        lost = true;
                    }
                }
                if let Some(i) = &self.introspect {
                    if let Ok(mut g) = i.lock() {
                        g.frames += 1;
                        g.shown = Some(shown.clone());
                        g.size = (r.plan.w, r.plan.h);
                        let frames_now = g.frames;
                        for z in &r.zones {
                            let imgs = z.ops.iter().filter_map(|op| {
                                if let crate::canvas::Op::Image { url, loaded, .. } = op {
                                    Some((url.clone(), *loaded))
                                } else {
                                    None
                                }
                            });
                            for (k, (url, loaded)) in imgs.enumerate() {
                                let zone = format!("{}#{k}", z.name);
                                let prev = g
                                    .art_log
                                    .iter()
                                    .rev()
                                    .find(|a| a.1 == zone)
                                    .map(|a| (a.2.clone(), a.3));
                                if prev != Some((url.clone(), loaded)) && g.art_log.len() < 20_000 {
                                    g.art_log.push((frames_now, zone, url, loaded));
                                }
                            }
                        }
                        if g.title_log.len() < 20_000 {
                            let idx = match &shown {
                                Shown::Single { idx, .. } => *idx,
                                Shown::Split { left, .. } => *left,
                                Shown::Processing { idx, .. } => *idx,
                            };
                            let m = &self.models[idx];
                            let frames = g.frames;
                            g.title_log.push((
                                frames,
                                m.id.clone(),
                                m.now.as_ref().map(|n| n.title.clone()).unwrap_or_default(),
                                m.queue.first().map(|e| e.title.clone()).unwrap_or_default(),
                            ));
                        }
                        g.runs = r
                            .zones
                            .iter()
                            .flat_map(|z| {
                                crate::canvas::text_ops(z)
                                    .map(|t| (z.name.clone(), t.text.clone(), t.color))
                            })
                            .collect();
                        g.header_bg = r.report.header_red.clone();
                        g.indicators = r.report.indicators.clone();
                        g.bars = r
                            .report
                            .bars
                            .iter()
                            .map(|b| (b.zone.clone(), b.db, b.fill_x))
                            .collect();
                    }
                }
            }
        }
        if let (Some((path, last)), Some((_, r))) = (self.snapshot.as_mut(), self.out.as_ref()) {
            if last.elapsed() > Duration::from_secs(10) {
                *last = Instant::now();
                if let Err(e) = r.canvas.save_png(path) {
                    tracing::debug!("display: snapshot: {e}");
                }
            }
        }
        if self.stats.at.elapsed() >= Duration::from_secs(60) {
            let (cpu, secs) = (cpu_seconds(), self.stats.at.elapsed().as_secs_f64());
            let busiest: Vec<String> = self
                .rates
                .iter_mut()
                .zip(&self.models)
                .map(|(r, m)| {
                    let mut v: Vec<_> = r.busiest.drain().filter(|(t, _)| t != "timing").collect();
                    v.sort();
                    format!(
                        "{}: {}",
                        m.id,
                        v.iter()
                            .map(|(t, n)| format!("{t} {n}/s"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .collect();
            tracing::info!("display: busiest second of events: {}", busiest.join("; "));
            tracing::info!(
                "display: {} frames, {} presented in {secs:.0} s; cpu {:.1} % of one core; view {}",
                self.stats.frames,
                self.stats.presents,
                100.0 * (cpu - self.stats.cpu_s) / secs,
                self.presenter.settings.view.name()
            );
            self.stats = Stats {
                at: Instant::now(),
                cpu_s: cpu,
                frames: 0,
                presents: 0,
            };
        }
        if lost {
            self.out = None;
            self.next_open = Instant::now() + Duration::from_secs(2);
        }
        if let Some(i) = &self.introspect {
            if let Ok(mut g) = i.lock() {
                g.connected = self
                    .models
                    .iter()
                    .map(|m| (m.id.clone(), m.connected))
                    .collect();
                g.view = self.presenter.settings.view.name().to_string();
                g.ballistics = self
                    .models
                    .iter()
                    .map(|m| m.dynamics.params.preset.clone())
                    .collect();
                g.library_up = self.library_up;
                g.output_up = self.out.is_some();
            }
        }
    }

    pub fn run(&mut self, stop: &AtomicBool) {
        let mut next = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            self.tick();
            next += FRAME_INTERVAL;
            let now = Instant::now();
            if next > now {
                // wake early for nothing: events are collected at the next tick, 33 ms at most away
                std::thread::sleep(next - now);
            } else if now - next > FRAME_INTERVAL * 3 {
                next = now;
            }
        }
        self.stop_threads.store(true, Ordering::Relaxed);
    }
}

/// `hotclock display`, live.
pub fn run(
    g: &GlobalConfig,
    _global: &Path,
    force_view: Option<ViewMode>,
    product: &str,
) -> ExitCode {
    let stations = discover(g);
    if stations.is_empty() {
        eprintln!(
            "hotclock display: no station configuration found in {}",
            g.stations_dir.display()
        );
        return ExitCode::from(2);
    }
    let state_file = std::env::var_os("STATE_DIRECTORY")
        .map(|d| {
            PathBuf::from(
                d.to_string_lossy()
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
            .join("view.json")
        })
        .filter(|p| p.parent().is_some_and(Path::is_dir));
    let cfg = Config {
        display: g.display.clone(),
        stations,
        library_addr: library_addr(g),
        state_file,
        force_view,
        tz_min: None,
        product: product.into(),
    };
    tracing::info!(
        "display: stations {}; library {}; mode {}, {} inches, {} cm",
        cfg.stations
            .iter()
            .map(|s| format!("{} at {}", s.id, s.addr))
            .collect::<Vec<_>>()
            .join(", "),
        cfg.library_addr,
        cfg.display.mode,
        cfg.display.diagonal_in,
        cfg.display.view_distance_cm
    );
    static STOP: AtomicBool = AtomicBool::new(false);
    let _ = ctrlc::set_handler(|| STOP.store(true, Ordering::Relaxed));
    let mut app = App::start(cfg, Box::new(KmsFactory));
    app.run(&STOP);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_events_in_one_second_is_reported_once_and_a_quiet_second_is_not() {
        let mut r = EventRate::default();
        let mut hits = vec![];
        for i in 0..30 {
            if let Some(h) = r.count("now", 1_000_000 + i * 10) {
                hits.push(h);
            }
        }
        assert_eq!(
            hits,
            vec![("now".to_string(), BURST + 1)],
            "reported once, when the sixth arrives"
        );
        assert_eq!(r.busiest["now"], 30);
        // the next second starts again; `timing` once a second and five `now` events in a second are ordinary
        let mut r = EventRate::default();
        for s in 0..60 {
            assert!(r.count("timing", 2_000_000 + s * 1000).is_none());
        }
        for i in 0..5 {
            assert!(r.count("now", 3_000_000 + i).is_none());
        }
        assert!(r.count("timing", 3_000_100).is_none());
        for i in 0..40 {
            assert!(
                r.count("timing", 4_000_000 + i).is_none(),
                "timing is never a burst"
            );
        }
    }
}
