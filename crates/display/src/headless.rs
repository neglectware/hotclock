//! The headless renderer: a feed drawn into a PNG, with a log of every zone, glyph run and bar, so the layout
//! checks and the screenshots need no panel (`hotclock display --render-png`).

use crate::feed::Feed;
use crate::layout::Plan;
use crate::render::Renderer;
use crate::scene::{BarInfo, Report, SceneInput, Shown};
use crate::view::{Presenter, ViewMode};
use common::config::DisplayConfig;
use serde_json::{json, Value};

pub struct Options {
    pub w: i32,
    pub h: i32,
    pub cfg: DisplayConfig,
    pub view: Option<ViewMode>,
    pub station: Option<String>,
    /// The product name for the full layout's header (from `brand`, never written here).
    pub product: String,
}

/// Draw the feed once into a fresh renderer.
pub fn render(feed: &Feed, o: &Options) -> (Renderer, Shown) {
    let plan = Plan::new(o.w, o.h, o.cfg.diagonal_in, o.cfg.view_distance_cm);
    let mut r = Renderer::new(plan);
    for (u, img) in &feed.art {
        r.canvas.images.insert(u, img.clone());
    }
    let mut settings = feed.settings.clone();
    if let Some(v) = o.view {
        settings.view = v;
    }
    if o.station.is_some() {
        settings.station.clone_from(&o.station);
    }
    let mut p = Presenter::new(settings);
    // a rotation screenshot: step the presenter to the requested point of the interval
    let shown = p.shown(&feed.stations, feed.now_ms - feed.rotate_elapsed_ms);
    let shown = if feed.rotate_elapsed_ms > 0 {
        p.shown(&feed.stations, feed.now_ms)
    } else {
        shown
    };
    let imgs = &r.canvas.images as *const _;
    // `has_image` reads the store the renderer owns; the scene only asks, so a raw borrow is unnecessary:
    // collect the known URLs first.
    let _ = imgs;
    let known: std::collections::HashSet<String> =
        feed.art.iter().map(|(u, _)| u.clone()).collect();
    let has = |u: &str| known.contains(u);
    let mut bar = BarInfo::new(&o.product, &o.cfg);
    if let Some(b) = &feed.bar {
        bar.update(b);
    }
    let inp = SceneInput {
        stations: &feed.stations,
        shown: shown.clone(),
        now_ms: feed.now_ms,
        clock: feed.clock.clone(),
        tz_min: feed.tz_min,
        product: o.product.clone(),
        cfg: &o.cfg,
        plan,
        has_image: &has,
        notices: &[],
        bar: &bar,
    };
    r.frame(&inp);
    (r, shown)
}

fn rect_json(r: crate::layout::Rect) -> Value {
    json!([r.x, r.y, r.w, r.h])
}

/// Everything the renderer drew, as JSON: the plan, the zones, every glyph run with its size, the artwork and the
/// bars and the curve.
pub fn log_json(r: &Renderer) -> Value {
    let rep: &Report = &r.report;
    let lay = r.layout.as_ref();
    json!({
        "size": [r.plan.w, r.plan.h],
        "plan": {"variant": format!("{:?}", r.plan.variant), "ppi": r.plan.ppi, "min_px": r.plan.min_px, "floor_px": r.plan.floor_px, "lines": r.plan.lines, "degraded": r.plan.degraded},
        "zones": lay.map(|l| l.zones.iter().map(|z| json!({"name": z.name, "rect": rect_json(z.rect)})).collect::<Vec<_>>()),
        "divider": lay.and_then(|l| l.divider).map(rect_json),
        "glyphs": r.canvas.log.glyphs.iter().map(|g| json!({
            "zone": g.zone, "text": g.text, "size": g.size, "font": format!("{:?}", g.font), "baseline": g.baseline, "bbox": rect_json(g.bbox)})).collect::<Vec<_>>(),
        "art": r.canvas.log.art.iter().map(|a| json!({"zone": a.zone, "rect": rect_json(a.rect), "kind": a.kind})).collect::<Vec<_>>(),
        "bars": rep.bars.iter().map(|b| json!({"zone": b.zone, "track": rect_json(b.track), "db": b.db, "fill_x": b.fill_x, "px_per_db": b.px_per_db})).collect::<Vec<_>>(),
        "stages": rep.stages.iter().map(|s| json!({"name": s.name, "rect": rect_json(s.rect), "bypassed": s.bypassed, "gr_db": s.gr_db, "bar": rect_json(s.bar), "track": rect_json(s.track)})).collect::<Vec<_>>(),
        "proc": rep.proc.as_ref().map(|p| json!({
            "t": p.t, "range_db": p.range_db, "ceiling": p.ceiling, "spectrum": p.spectrum,
            "grid": p.grid.iter().map(|(d, y)| json!([d, y])).collect::<Vec<_>>(),
            "cols": p.cols.iter().map(|c| json!({"name": c.name, "label": c.label, "rect": rect_json(c.rect), "track": rect_json(c.track), "bar": rect_json(c.bar),
                "bypassed": c.bypassed, "gr_db": c.gr_db, "bar_db": c.bar_db, "range_db": c.range_db, "centered": c.centered, "hold_y": c.hold_y, "led": c.led})).collect::<Vec<_>>()})),
        "curve": rep.curve.iter().map(|c| json!({"f": c.f, "db": c.db, "x": c.x, "y": c.y})).collect::<Vec<_>>(),
        "crossovers": rep.crossovers.iter().map(|(f, x)| json!({"f": f, "x": x})).collect::<Vec<_>>(),
        "plot": rep.plot.map(rect_json),
        "layers": rep.layers,
        "bar_items": rep.bar_items.iter().map(|(n, t, r)| json!({"item": n, "text": t, "rect": rect_json(*r)})).collect::<Vec<_>>(),
        "lc": rep.lc.iter().map(|l| json!({"zone": l.zone, "track": rect_json(l.track), "gain_db": l.gain_db, "state": l.state, "range_db": l.range_db,
            "x0": l.x0, "fill_x": l.fill_x, "color": [l.color.0, l.color.1, l.color.2], "limit_x": [l.limit_x.0, l.limit_x.1]})).collect::<Vec<_>>(),
        "notes": rep.notes,
    })
}
