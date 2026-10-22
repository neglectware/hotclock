//! The scene: from the stations' state to the zones' operation lists, for whichever layout the screen and the
//! view call for (architecture 11.2, 11.2a, 11.2b). Nothing here touches the clock or a screen: the caller says
//! what time it is, and the result is a list of [`ZoneOps`] plus a [`Report`] the tests read.

use crate::canvas::{q8, theme, Align, Color, FontKind, Fonts, Op, TextOp, ZoneOps, CAP_RATIO};
use crate::layout::{layout, Kind, Layout, Plan, Rect, Variant};
use crate::model::{QueueEntry, StationModel};
use common::config::DisplayConfig;
use dspmodel::BandTable;

mod compact;
mod full;
pub mod proc;

/// Which stations the screen shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// One station fills the screen; `other` is the second station's header indicator; `rotate` is the fraction of
    /// the rotation interval that has passed (`rotate` view only).
    Single {
        idx: usize,
        other: Option<usize>,
        rotate: Option<f64>,
        pinned: bool,
    },
    Split {
        left: usize,
        right: usize,
    },
    /// One station's processing view (11.2c): the gain-reduction meters of its chain, with the same header
    /// indicator and rotation bar as `Single`.
    Processing {
        idx: usize,
        other: Option<usize>,
        rotate: Option<f64>,
        pinned: bool,
    },
}

pub struct SceneInput<'a> {
    pub stations: &'a [StationModel],
    pub shown: Shown,
    pub now_ms: i64,
    /// `HH:MM:SS` local time.
    pub clock: String,
    /// Minutes east of UTC, for the times of day in the lists.
    pub tz_min: i64,
    /// The product name for the full layout's header.
    pub product: String,
    pub cfg: &'a DisplayConfig,
    pub plan: Plan,
    /// Which artwork URLs are decoded and ready.
    pub has_image: &'a dyn Fn(&str) -> bool,
    /// Alarms of the display itself (the panel refused the mode, 7d), shown above the stations' own.
    pub notices: &'a [String],
    /// What the shared top bar of `split` shows besides the clock.
    pub bar: &'a BarInfo,
}

/// Which items the top bar of `split` shows (`display.bar_*`, as `GET /api/v1/display/bar` answers them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarItems {
    pub product: bool,
    pub clock: bool,
    pub date: bool,
    pub cpu: bool,
    pub network: bool,
}

impl BarItems {
    pub fn from_cfg(d: &DisplayConfig) -> BarItems {
        BarItems {
            product: d.bar_product,
            clock: d.bar_clock,
            date: d.bar_date,
            cpu: d.bar_cpu,
            network: d.bar_network,
        }
    }
}

/// The top bar's content: the library service's `GET /api/v1/display/bar` (the same source as the web display), and
/// until it answers, the product name and the items of the box's config.
#[derive(Clone, Debug, PartialEq)]
pub struct BarInfo {
    pub product: String,
    /// The short version, like `0.1.0-25`; empty until the library service has said it.
    pub version: String,
    /// The whole box's CPU use in percent, averaged over 5 s; `None` when the box cannot tell.
    pub cpu_percent: Option<u8>,
    /// A network route out (`Some(true)`), none (`Some(false)`), or unknown.
    pub network: Option<bool>,
    pub items: BarItems,
}

impl BarInfo {
    pub fn new(product: &str, d: &DisplayConfig) -> BarInfo {
        BarInfo {
            product: product.into(),
            version: String::new(),
            cpu_percent: None,
            network: None,
            items: BarItems::from_cfg(d),
        }
    }

    /// Take the endpoint's answer; `Null` (the library service did not answer) keeps the items and forgets the
    /// readings, as the web page does.
    pub fn update(&mut self, v: &serde_json::Value) {
        if !v.is_object() {
            self.cpu_percent = None;
            self.network = None;
            return;
        }
        if let Some(p) = v["product"].as_str() {
            self.product = p.into();
        }
        if let Some(x) = v["version"].as_str() {
            self.version = x.into();
        }
        self.cpu_percent = v["cpu_percent"].as_u64().map(|x| x.min(100) as u8);
        self.network = match v["network"].as_str() {
            Some("up") => Some(true),
            Some("down") => Some(false),
            _ => None,
        };
        let it = &v["items"];
        let b = |k: &str, cur: bool| it[k].as_bool().unwrap_or(cur);
        self.items = BarItems {
            product: b("product", self.items.product),
            clock: b("clock", self.items.clock),
            date: b("date", self.items.date),
            cpu: b("cpu", self.items.cpu),
            network: b("network", self.items.network),
        };
    }
}

/// `SAT 10 OCT`: the local date of a Unix millisecond instant.
pub fn date_text(ms: i64, tz_min: i64) -> String {
    let days = (ms.div_euclid(60_000) + tz_min).div_euclid(1440);
    // days since 1970-01-01 to a civil date (H. Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    const DAYS: [&str; 7] = ["THU", "FRI", "SAT", "SUN", "MON", "TUE", "WED"];
    const MONTHS: [&str; 12] = [
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ];
    format!(
        "{} {d} {}",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(m - 1) as usize]
    )
}

#[derive(Clone, Debug)]
pub struct CurvePoint {
    pub f: f64,
    pub db: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug)]
pub struct MeterBar {
    pub zone: String,
    pub track: Rect,
    pub db: Option<f64>,
    /// The pixel column where the fill ends, and the pixel step of the scale (one dB in pixels).
    pub fill_x: i32,
    pub px_per_db: f64,
    pub tick_x: Vec<(f64, i32)>,
    pub hold_x: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct StageCol {
    pub name: String,
    pub rect: Rect,
    pub bypassed: bool,
    pub gr_db: Option<f64>,
    /// The hanging bar (or, for the AGC, the center-zero bar) and the track it sits in.
    pub track: Rect,
    pub bar: Rect,
    pub range_db: f64,
    pub has_numerals: bool,
}

#[derive(Clone, Debug)]
pub struct SpecBar {
    pub band: usize,
    pub x: i32,
    pub w: i32,
    pub top: i32,
    pub cap: i32,
}

/// One column of the processing view, as drawn: the tests read the bar against the frame.
#[derive(Clone, Debug)]
pub struct ProcCol {
    /// `agc`, `band1` to `band6`, `multiband` (a lone, grey or unbanded one), `wideband`, `limiter`, `clipper`.
    pub name: String,
    /// What is written over it (`LOW`, `B2`, `WB`).
    pub label: String,
    pub rect: Rect,
    /// The track the bar lives in, and the bar itself (hanging from the top of the track; the AGC's from its middle).
    pub track: Rect,
    pub bar: Rect,
    pub bypassed: bool,
    /// What the frame says: reduction in dB (0 or negative), or the AGC's signed gain.
    pub gr_db: Option<f64>,
    /// What the bar shows: the reduction with the gain-reduction ballistics, positive; the AGC's signed gain.
    pub bar_db: f64,
    /// The dB at the far end of the track (for the AGC, each way from the middle).
    pub range_db: f64,
    pub centered: bool,
    /// The peak tick's top row.
    pub hold_y: Option<i32>,
    /// The LED of the AGC (gate frozen) and the clipper (clipping now).
    pub led: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcReport {
    /// The text size of the view.
    pub t: i32,
    pub range_db: f64,
    pub cols: Vec<ProcCol>,
    /// `(dB, row)` of each line of the shared scale.
    pub grid: Vec<(f64, i32)>,
    /// The limiter's ceiling, drawn as a tick on the output bars and written in the footer.
    pub ceiling: Option<f64>,
    pub spectrum: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub proc: Option<ProcReport>,
    /// The mini processing strips drawn in the compact views, by station.
    pub mini: Vec<(String, ProcReport)>,
    pub layout: Option<Layout>,
    pub bars: Vec<MeterBar>,
    pub stages: Vec<StageCol>,
    pub curve: Vec<CurvePoint>,
    pub crossovers: Vec<(f64, f64)>,
    /// `(zone rect)` of the processing plot, the curve range and which layers were drawn.
    pub plot: Option<Rect>,
    pub curve_range_db: f64,
    pub layers: Vec<&'static str>,
    pub spectrum_post: Vec<SpecBar>,
    pub spectrum_pre: Vec<SpecBar>,
    pub spectrum_bands: usize,
    pub loudness: Vec<(String, String)>,
    pub segue: Vec<(String, String)>,
    pub upcoming: Vec<String>,
    pub notes: Vec<String>,
    pub header_red: Vec<(String, bool)>,
    /// The other station's header indicator: `(station name, color)`.
    pub indicators: Vec<(String, Color)>,
    /// The items of `split`'s top bar as drawn: `(item, text, rect)`; the network dot's text is empty.
    pub bar_items: Vec<(String, String, Rect)>,
    /// The loudness controller bars drawn.
    pub lc: Vec<LcBar>,
}

/// The loudness controller's bar, as drawn.
#[derive(Clone, Debug)]
pub struct LcBar {
    pub zone: String,
    pub track: Rect,
    pub gain_db: f64,
    pub state: u8,
    /// The dB at each end of the track (the larger of the two limits, whole dB).
    pub range_db: f64,
    /// The centre column, and the column where the fill ends.
    pub x0: i32,
    pub fill_x: i32,
    pub color: Color,
    /// The columns of the two limit ticks.
    pub limit_x: (i32, i32),
}

/// State kept between frames only to avoid recomputing: the band responses of the current chain.
#[derive(Default)]
pub struct SceneCache {
    table: Option<((Vec<u64>, usize), BandTable)>,
}

impl SceneCache {
    pub fn table(&mut self, crossovers: &[f64], points: usize) -> &BandTable {
        let key = (
            crossovers.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
            points,
        );
        if self.table.as_ref().is_none_or(|(k, _)| *k != key) {
            self.table = Some((
                key.clone(),
                BandTable::new(crossovers, dspmodel::log_freqs(points)),
            ));
        }
        &self.table.as_ref().expect("table").1
    }
}

pub fn build(
    inp: &SceneInput,
    fonts: &Fonts,
    cache: &mut SceneCache,
) -> (Layout, Vec<ZoneOps>, Report) {
    if let Shown::Processing { .. } = inp.shown {
        let (lay, m, spec) = proc::layout(&inp.plan, inp.cfg.proc_spectrum);
        let mut rep = Report {
            layout: Some(lay.clone()),
            ..Report::default()
        };
        if inp.cfg.proc_spectrum && !spec {
            rep.notes
                .push("spectrum: no room at this size, not drawn".into());
        }
        let zones = proc::build(inp, fonts, &lay, &m, &mut rep);
        if let Some(p) = rep.proc.as_mut() {
            p.spectrum = spec;
        }
        return (lay, zones, rep);
    }
    let kind = match inp.shown {
        Shown::Single { .. } => Kind::Single,
        Shown::Split { .. } => Kind::Split,
        Shown::Processing { .. } => unreachable!("handled above"),
    };
    let lay = layout(&inp.plan, kind);
    let mut rep = Report {
        layout: Some(lay.clone()),
        ..Report::default()
    };
    let zones = match lay.variant {
        Variant::Full => full::build(inp, fonts, &lay, cache, &mut rep),
        _ => compact::build(inp, fonts, &lay, &mut rep),
    };
    (lay, zones, rep)
}

// ---- helpers shared by both builders --------------------------------------------------------------------------------

pub(crate) struct Ops<'a> {
    pub fonts: &'a Fonts,
    pub ops: Vec<Op>,
}

impl<'a> Ops<'a> {
    pub fn new(fonts: &'a Fonts) -> Ops<'a> {
        Ops { fonts, ops: vec![] }
    }

    pub fn rect(&mut self, r: Rect, c: Color) {
        if r.w > 0 && r.h > 0 {
            self.ops.push(Op::Rect(r, c));
        }
    }

    pub fn rrect(&mut self, r: Rect, radius: i32, c: Color) {
        if r.w > 0 && r.h > 0 {
            self.ops.push(Op::RoundRect(r, radius, c));
        }
    }

    /// Text on a baseline. The text is fitted (ellipsized) to `max_w` when given.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        x: i32,
        baseline: i32,
        text: &str,
        size: i32,
        font: FontKind,
        color: Color,
        align: Align,
        max_w: Option<i32>,
    ) -> i32 {
        let t = match max_w {
            Some(m) => self.fonts.fit(font, text, size, m),
            None => text.to_string(),
        };
        let w = self.fonts.width(font, &t, size);
        self.ops.push(Op::Text(TextOp {
            x,
            baseline,
            text: t,
            size,
            font,
            color,
            align,
        }));
        w
    }

    /// Text centered on the cap height inside a line box starting at `top` and `lh` tall.
    #[allow(clippy::too_many_arguments)]
    pub fn line(
        &mut self,
        x: i32,
        top: i32,
        lh: i32,
        text: &str,
        size: i32,
        font: FontKind,
        color: Color,
        align: Align,
        max_w: Option<i32>,
    ) -> i32 {
        self.text(
            x,
            baseline_in(top, lh, size),
            text,
            size,
            font,
            color,
            align,
            max_w,
        )
    }

    pub fn width(&self, font: FontKind, text: &str, size: i32) -> i32 {
        self.fonts.width(font, text, size)
    }

    pub fn path(
        &mut self,
        pts: Vec<(i32, i32)>,
        closed: bool,
        fill: Option<Color>,
        stroke: Option<(Color, i32)>,
        dash: Option<(i32, i32)>,
    ) {
        self.ops.push(Op::Path {
            pts,
            closed,
            fill,
            stroke,
            dash,
        });
    }

    pub fn finish(self, name: &str, rect: Rect, bg: Color) -> ZoneOps {
        ZoneOps {
            name: name.into(),
            rect,
            bg,
            ops: self.ops,
        }
    }
}

/// The baseline that centers a cap-height line of `size` px in a box `lh` tall at `top`.
pub fn baseline_in(top: i32, lh: i32, size: i32) -> i32 {
    top + (lh + (size as f32 * CAP_RATIO).round() as i32) / 2
}

/// `m:ss`, or `h:mm:ss` from an hour up; counts down, so a partial second rounds up.
pub fn countdown(ms: i64) -> String {
    let s = (ms.max(0) + 999) / 1000;
    hms(s)
}

/// Elapsed time: a partial second rounds down.
pub fn elapsed(ms: i64) -> String {
    hms(ms.max(0) / 1000)
}

fn hms(s: i64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `HH:MM` local time of a Unix millisecond instant.
pub fn hhmm(ms: i64, tz_min: i64) -> String {
    let m = (ms.div_euclid(60_000) + tz_min).rem_euclid(1440);
    format!("{:02}:{:02}", m / 60, m % 60)
}

pub fn fmt_lu(v: Option<f64>) -> String {
    v.map_or("--".into(), |x| format!("{x:.1}"))
}

/// The color of a countdown: amber under `amber_ms`, red under `red_ms`.
pub fn urgency(ms: i64, amber_ms: i64, red_ms: i64) -> Color {
    if ms < red_ms {
        theme::RED
    } else if ms < amber_ms {
        theme::AMBER
    } else {
        theme::TEXT
    }
}

pub fn pill_colors(label: &str) -> (Color, Color) {
    // (background, text)
    match label {
        "PLAYING" => (Color::rgb(0x14, 0x4a, 0x35), theme::GREEN),
        "ASSIST" | "HELD" => (Color::rgb(0x55, 0x3d, 0x0c), theme::AMBER),
        "SAFE MODE" | "NO CONNECTION" => {
            (Color::rgb(0x7a, 0x1d, 0x22), Color::rgb(0xff, 0xd6, 0xd6))
        }
        _ => (theme::PANEL2, theme::DIM),
    }
}

/// The severity color of a station's header indicator: red on a critical alert or no connection, amber on a warning.
pub fn alert_color(s: &StationModel) -> Color {
    if s.alarming() {
        theme::RED
    } else if s.warning() {
        theme::AMBER
    } else {
        theme::GREEN
    }
}

/// The alarm lines to show for `idx`: its own alerts (critical first, newest first), preceded by a line for any
/// other station that cannot be reached.
pub fn alarm_lines(inp: &SceneInput, idx: usize, include_others: bool) -> Vec<(Color, String)> {
    let stations = inp.stations;
    let mut out: Vec<(Color, String)> = inp
        .notices
        .iter()
        .map(|n| (theme::AMBER, n.clone()))
        .collect();
    let me = &stations[idx];
    if include_others {
        for (i, s) in stations.iter().enumerate() {
            if i != idx && !s.connected {
                out.push((theme::RED, format!("{}: NO CONNECTION", s.name())));
            }
        }
    }
    if !me.connected {
        out.insert(inp.notices.len(), (theme::RED, "NO CONNECTION".into()));
        return out;
    }
    let mut a: Vec<_> = me.alerts.iter().collect();
    a.sort_by_key(|x| {
        (
            !x.critical(),
            std::cmp::Reverse(x.raised_ms.unwrap_or(0)),
            std::cmp::Reverse(x.id),
        )
    });
    for x in a {
        let c = if x.critical() {
            theme::RED
        } else {
            theme::AMBER
        };
        out.push((c, format!("{}  {}", x.code, x.message)));
    }
    out
}

/// A queue entry as one row of text: the title, and the artist when it fits.
pub fn row_title(e: &QueueEntry) -> String {
    if e.kind == "block" {
        let base = if e.name.is_empty() {
            e.block_type.to_uppercase()
        } else {
            e.name.clone()
        };
        // The back-timing plan of the block that ends a back-timed fill: `END +3 s` (4.9).
        if e.end_label.is_empty() {
            base
        } else {
            format!("{base}  {}", e.end_label)
        }
    } else {
        e.title.clone()
    }
}

pub fn q(v: f64) -> i32 {
    q8(v)
}

// ---- parts both layouts use ------------------------------------------------------------------------------------------

/// The scale of a PPM bar: -60 dBFS at the left, 0 at the right.
pub const PPM_LO: f64 = -60.0;

pub fn db_x(track: Rect, db: f64) -> i32 {
    let f = ((db - PPM_LO) / -PPM_LO).clamp(0.0, 1.0);
    track.x + (f * f64::from(track.w)).round() as i32
}

/// One horizontal PPM bar with its peak-hold tick and scale ticks, and its entry in the report.
pub(crate) fn ppm_bar(
    o: &mut Ops,
    rep: &mut Report,
    zone: &str,
    track: Rect,
    db: Option<f64>,
    hold: Option<f64>,
    ticks: &[f64],
) {
    o.rect(track, theme::BG);
    let fill_x = db.map_or(track.x, |v| db_x(track, v));
    // green up to -12 dB, amber to -3, red above: the segments of one fill
    let seg = |lo: f64, hi: f64| (db_x(track, lo), db_x(track, hi));
    for (lo, hi, c) in [
        (-60.0, -12.0, theme::GREEN),
        (-12.0, -3.0, theme::AMBER),
        (-3.0, 0.0, theme::RED),
    ] {
        let (a, b) = seg(lo, hi);
        let b = b.min(fill_x);
        if b > a {
            o.rect(Rect::new(a, track.y, b - a, track.h), c);
        }
    }
    let hold_x = hold.map(|h| db_x(track, h).min(track.right() - 2));
    if let Some(x) = hold_x {
        o.rect(Rect::new(x, track.y, 2, track.h), Color::rgb(255, 255, 255));
    }
    let mut tick_x = vec![];
    for &t in ticks {
        let x = db_x(track, t).min(track.right() - 2);
        tick_x.push((t, x));
        o.rect(
            Rect::new(x, track.y, 2, track.h),
            theme::LINE.with_alpha(230),
        );
    }
    rep.bars.push(MeterBar {
        zone: zone.into(),
        track,
        db,
        fill_x,
        px_per_db: f64::from(track.w) / -PPM_LO,
        tick_x,
        hold_x,
    });
}

/// Artwork or the placeholder, for a queue entry or the item on air.
pub(crate) fn art_op(o: &mut Ops, inp: &SceneInput, rect: Rect, url: Option<&str>, size: u32) {
    match url {
        Some(u) => {
            let u = sized(u, size);
            let loaded = (inp.has_image)(&u);
            o.ops.push(Op::Image {
                rect,
                url: u,
                loaded,
            });
        }
        None => o.ops.push(Op::Placeholder(rect)),
    }
}

/// The artwork URL at a size: the station's `art_url` carries `size=144`.
pub fn sized(url: &str, size: u32) -> String {
    match url.find("size=") {
        Some(i) => format!("{}size={size}", &url[..i]),
        None => url.to_string(),
    }
}

pub fn origin_badge(o: &str) -> String {
    match o {
        "rotation" => "ROT".into(),
        "liner" => "LINER".into(),
        "id" => "ID".into(),
        "block" => "BLK".into(),
        "manual" => "MAN".into(),
        "" => String::new(),
        x => x.to_uppercase().chars().take(5).collect(),
    }
}

/// The end-reason badge of the as-run log.
pub fn end_badge(r: &str) -> String {
    match r {
        "end" => "END".into(),
        "next" | "fade" => "SEG".into(),
        "cut" => "CUT".into(),
        "stop" => "HARD".into(),
        "error" | "restart" => "ERR".into(),
        "" => String::new(),
        x => x.to_uppercase().chars().take(4).collect(),
    }
}

/// A small filled label (a pill) with text; returns its width.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pill(
    o: &mut Ops,
    x: i32,
    top: i32,
    h: i32,
    text: &str,
    size: i32,
    bg: Color,
    fg: Color,
    font: FontKind,
) -> i32 {
    let pad = (h / 4).max(6);
    let w = o.width(font, text, size) + 2 * pad;
    o.rrect(Rect::new(x, top, w, h), h / 3, bg);
    o.line(x + pad, top, h, text, size, font, fg, Align::Left, None);
    w
}

/// The progress bar of the item on air with the segue tick.
pub(crate) fn progress_bar(o: &mut Ops, r: Rect, st: &StationModel, now_ms: i64) {
    o.rect(r, theme::PANEL2);
    if let Some(n) = &st.now {
        let span = (n.ends_ms - n.started_ms).max(1) as f64;
        let el = st.elapsed_ms(now_ms).unwrap_or(0) as f64;
        let w = ((el / span).clamp(0.0, 1.0) * f64::from(r.w)).round() as i32;
        o.rect(Rect::new(r.x, r.y, w, r.h), theme::BLUE_LINE);
        if let Some(sg) = n.segue_ms {
            if !n.continuity_next {
                let x = r.x
                    + (((sg - n.started_ms) as f64 / span).clamp(0.0, 1.0) * f64::from(r.w)).round()
                        as i32;
                o.rect(
                    Rect::new((x - 1).min(r.right() - 3).max(r.x), r.y, 3, r.h),
                    theme::AMBER,
                );
            }
        }
    }
}

/// The segue line of the item on air: the text and its color.
pub fn segue_text(st: &StationModel, now_ms: i64) -> Option<(String, Color)> {
    let n = st.now.as_ref()?;
    if st.idle(now_ms) {
        // held or stopped: say so, and for how long the item has been over; no countdown, nothing extrapolated
        let label = match st.state.as_str() {
            "held" => "HELD",
            "stopped" => "STOPPED",
            "safe" => "SAFE MODE",
            _ => return None,
        };
        let over = st.over_ms(now_ms);
        return Some((
            if over > 0 {
                format!("{label} {}", elapsed(over))
            } else {
                label.into()
            },
            theme::AMBER,
        ));
    }
    if n.continuity_next {
        return Some((
            format!("ALBUM, ENDS {}", countdown(st.remaining_ms(now_ms)?)),
            theme::DIM,
        ));
    }
    let left = st.segue_left_ms(now_ms)?;
    Some((
        format!("SEGUE {}", countdown(left)),
        urgency(left, 30_000, 10_000),
    ))
}

/// The colour of the loudness controller's state: grey OFF, blue CONVERGING, green ON TARGET, amber FROZEN and AT
/// LIMIT (the web display's colours).
pub fn lc_color(state: u8) -> Color {
    match state {
        1 => Color::rgb(0x5a, 0xa9, 0xff),
        2 => theme::GREEN,
        3 | 4 => theme::AMBER,
        _ => Color::rgb(0x4a, 0x52, 0x62),
    }
}

/// The loudness controller's reading while its bar shows: a live frame with a target (the controller runs) and the
/// station's gain range known. `(gain dB, state, min dB, max dB)`.
pub fn lc_reading(st: &StationModel, now_ms: i64) -> Option<(f64, u8, f64, f64)> {
    let f = st.live_frame(now_ms)?;
    f.target?;
    let (lo, hi) = st.lc_limits?;
    Some((f.gain_db, f.loudness_state, lo, hi))
}

/// `+3.2`: the controller's gain as the meters write it.
pub fn lc_number(gain: f64) -> String {
    format!("{gain:+.1}")
}

/// The loudness controller's bar: centred on 0 dB, right for gain added and left for gain taken, on a scale of the
/// larger of the two limits (whole dB); the limits as grey ticks, the centre as a white one.
pub(crate) fn lc_bar(
    o: &mut Ops,
    rep: &mut Report,
    zone: &str,
    track: Rect,
    (gain, state, lo, hi): (f64, u8, f64, f64),
) {
    o.rect(track, theme::BG);
    let range = lo.abs().max(hi.abs()).ceil().max(1.0);
    let x0 = track.x + track.w / 2;
    let xof =
        |db: f64| x0 + ((db / range).clamp(-1.0, 1.0) * f64::from(track.w) / 2.0).round() as i32;
    let fill_x = xof(gain);
    let color = lc_color(state);
    o.rect(
        Rect::new(x0.min(fill_x), track.y, (fill_x - x0).abs(), track.h),
        color,
    );
    let tick = |x: i32| (x - 1).clamp(track.x, track.right() - 2);
    let limit_x = (tick(xof(lo)), tick(xof(hi)));
    for x in [limit_x.0, limit_x.1] {
        o.rect(Rect::new(x, track.y, 2, track.h), theme::DIM);
    }
    o.rect(Rect::new(x0 - 1, track.y, 2, track.h), theme::TEXT);
    rep.lc.push(LcBar {
        zone: zone.into(),
        track,
        gain_db: gain,
        state,
        range_db: range,
        x0,
        fill_x,
        color,
        limit_x,
    });
}

/// The loudness text of the compact layouts: which of S, W, I and the target are shown.
pub fn loudness_line(
    st: &StationModel,
    now_ms: i64,
    with_s: bool,
    with_i: bool,
    with_target: bool,
) -> String {
    let f = st.live_frame(now_ms);
    let mut s = String::new();
    if with_s {
        s += &format!("S {}  ", fmt_lu(f.and_then(|f| f.short_term)));
    }
    s += &format!("W {}", fmt_lu(f.and_then(|f| f.window)));
    let target = f.and_then(|f| f.target);
    if with_i {
        s += &format!("  I {}", fmt_lu(f.and_then(|f| f.integrated)));
    }
    if let Some(t) = target.filter(|_| with_target) {
        s += &format!(" / {t:.0}");
    }
    s
}

pub fn controller_text(st: &StationModel, now_ms: i64) -> String {
    let Some(f) = st.live_frame(now_ms) else {
        return String::new();
    };
    let state = ["OFF", "CONVERGING", "ON TARGET", "FROZEN", "AT LIMIT"]
        .get(f.loudness_state as usize)
        .copied()
        .unwrap_or("");
    if f.loudness_state == 0 {
        String::new()
    } else {
        format!("{:+.1} dB {state}", f.gain_db)
    }
}
