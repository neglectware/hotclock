//! The processing view (architecture 11.2c): one station's processing chain as meters. A gain-reduction bar per
//! multiband band, the wideband compressor, the limiter and the clipper, growing downward from the top of one shared
//! dB scale (the way an Optimod's band meters do); the AGC's gain on its own center-zero scale; the level into the
//! chain against the level out of it; and the loudness controller. Everything is sized from one number, the text
//! size `t` (31 px on the 7-inch 1024 x 600 panel, the legibility minimum there), and nothing is drawn smaller.
//! Graphics may be as fine as they like; text may not. The JavaScript twin is `crates/serve/web/procview.js`.

use super::*;
use crate::frame::{Frame, STAGES};
use crate::layout::Zone;
use crate::model::{GR_CLIPPER, GR_LIMITER, GR_WIDEBAND};

/// The AGC column's scale: boost up, cut down, +-9 dB.
pub const AGC_RANGE_DB: f64 = 9.0;
/// The cap height of the fonts as a fraction of the size (`canvas::CAP_RATIO`).
const CAP_RATIO_F: f64 = 0.729;
/// A line of the shared gain-reduction scale every this many dB.
pub const GRID_DB: f64 = 6.0;

/// Which spaces the zones of the view take, from the text size.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// The text size: every line of text in the view is this big (the title strip, the labels, the numerals).
    pub t: i32,
    pub pad: i32,
    pub pad_v: i32,
    /// A line box: 1.25 t.
    pub line: i32,
}

/// The text size: the compact layouts' own (31 px at 1024 x 600, 19 px at 800 x 480, scaled to other sizes), and
/// on a full-size screen 28 px at 1080p (the full layout's row text), never under the legibility minimum.
pub fn text_px(plan: &Plan) -> i32 {
    match plan.variant {
        Variant::Full => plan
            .min_px
            .max((28.0 * plan.s()).round() as i32)
            .max(plan.floor_px),
        v => {
            let (_, _, k) = plan.scales(v);
            plan.px(if v == Variant::C800 { 19.0 } else { 31.0 }, k)
        }
    }
}

fn scaled(t: i32, f: f64) -> i32 {
    (f64::from(t) * f).round() as i32
}

pub fn metrics(plan: &Plan) -> Metrics {
    let t = text_px(plan);
    Metrics {
        t,
        pad: scaled(t, 0.3).max(4),
        pad_v: scaled(t, 0.2).max(3),
        line: scaled(t, 1.25),
    }
}

/// The zones, top to bottom: header, now, cols, the spectrum when asked for and there is room, meters. They tile
/// the screen exactly. Returns the layout, the metrics and whether the spectrum zone exists.
pub fn layout(plan: &Plan, spectrum: bool) -> (Layout, Metrics, bool) {
    let m = metrics(plan);
    let t = m.t;
    let hdr = scaled(t, 1.42);
    let now = scaled(t, 1.3);
    let row = scaled(t, 1.22);
    let meters = 3 * row + 2 * m.pad_v;
    let cols_min = scaled(t, 10.0);
    let rest = plan.h - hdr - now - meters;
    let spec_h = if spectrum {
        let avail = rest - cols_min;
        let want = (f64::from(plan.h) * 0.3).round() as i32;
        if avail >= scaled(t, 4.0) {
            avail.min(want).max(scaled(t, 4.0))
        } else {
            0
        }
    } else {
        0
    };
    let cols = (rest - spec_h).max(1);
    let w = plan.w;
    let mut rows = vec![("header", hdr), ("now", now), ("cols", cols)];
    if spec_h > 0 {
        rows.push(("spectrum", spec_h));
    }
    let used: i32 = rows.iter().map(|r| r.1).sum();
    rows.push(("meters", (plan.h - used).max(0)));
    let mut y = 0;
    let mut zones = vec![];
    for (name, h) in rows {
        zones.push(Zone {
            name: name.into(),
            rect: Rect::new(0, y, w, h),
        });
        y += h;
    }
    (
        Layout {
            zones,
            divider: None,
            variant: plan.variant,
            kind: Kind::Processing,
        },
        m,
        spec_h > 0,
    )
}

// ---- the columns -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColKind {
    Agc,
    Band,
    Mb,
    Wide,
    Lim,
    Clip,
}

struct PCol {
    kind: ColKind,
    label: String,
    name: String,
    enabled: bool,
    /// The slot in `Dynamics::gr` (bands 0 to 5, then wideband, limiter, clipper); none for the AGC.
    slot: Option<usize>,
    /// What the frame says: reduction (0 or negative) in dB, or for the AGC the signed gain.
    raw: Option<f64>,
}

/// The processing blocks the station has, in chain order, each on or bypassed (11.2): a block the chain
/// configuration does not list is absent, a listed one that is off is bypassed (grey `BYP`, or omitted when
/// `hide`), and with the whole chain bypassed every column is.
fn columns(st: &StationModel, fr: Option<&Frame>, hide: bool) -> Vec<PCol> {
    let names: Vec<&str> = match st.chain.as_ref().filter(|c| !c.stages.is_empty()) {
        Some(c) => c
            .stages
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| STAGES[..5].contains(n))
            .collect(),
        None => STAGES[..5].to_vec(),
    };
    let chain_bypassed = fr.is_none_or(Frame::bypassed);
    let mut cols = vec![];
    for name in names {
        let i = STAGES.iter().position(|s| *s == name).unwrap_or(0);
        let en = !chain_bypassed && fr.is_some_and(|f| f.stage_enabled(i));
        let gr = fr.and_then(|f| f.stages[i].gr).filter(|_| en);
        match name {
            "agc" => cols.push(PCol {
                kind: ColKind::Agc,
                label: "AGC".into(),
                name: "agc".into(),
                enabled: en,
                slot: None,
                raw: gr,
            }),
            "multiband" => {
                let nb = if en {
                    fr.map_or(0, |f| usize::from(f.band_count))
                } else {
                    0
                };
                if nb >= 2 {
                    for b in 0..nb.min(6) {
                        cols.push(PCol {
                            kind: ColKind::Band,
                            label: format!("B{}", b + 1),
                            name: format!("band{}", b + 1),
                            enabled: true,
                            slot: Some(b),
                            raw: fr.and_then(|f| f.band_gr[b]),
                        });
                    }
                } else {
                    cols.push(PCol {
                        kind: ColKind::Mb,
                        label: "MB".into(),
                        name: "multiband".into(),
                        enabled: en,
                        slot: None,
                        raw: gr,
                    });
                }
            }
            "wideband" => cols.push(PCol {
                kind: ColKind::Wide,
                label: "WB".into(),
                name: "wideband".into(),
                enabled: en,
                slot: Some(GR_WIDEBAND),
                raw: gr,
            }),
            "limiter" => cols.push(PCol {
                kind: ColKind::Lim,
                label: "LIM".into(),
                name: "limiter".into(),
                enabled: en,
                slot: Some(GR_LIMITER),
                raw: gr,
            }),
            "clipper" => cols.push(PCol {
                kind: ColKind::Clip,
                label: "CLP".into(),
                name: "clipper".into(),
                enabled: en,
                slot: Some(GR_CLIPPER),
                raw: gr,
            }),
            _ => {}
        }
    }
    if hide {
        cols.retain(|c| c.enabled);
    }
    cols
}

/// The names of the bands of an `n`-band multiband, low to high.
pub fn band_names(n: usize) -> Vec<String> {
    let v: &[&str] = match n {
        2 => &["LOW", "HIGH"],
        3 => &["LOW", "MID", "HIGH"],
        4 => &["LOW", "LMID", "HMID", "HIGH"],
        5 => &["LOW", "LMID", "MID", "HMID", "HIGH"],
        6 => &["LOW", "LMID", "MID", "HMID", "PRES", "HIGH"],
        _ => &[],
    };
    v.iter().map(|s| (*s).to_string()).collect()
}

/// A crossover frequency as a short word: `120`, `1k`, `3.5k`, `12k`.
pub fn hz_text(f: f64) -> String {
    if f >= 1000.0 {
        let k = f / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}k", k.round() as i64)
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{}", f.round() as i64)
    }
}

fn led(o: &mut Ops, cx: i32, cy: i32, r: i32, c: Color) {
    o.path(
        (0..24)
            .map(|i| {
                let a = f64::from(i) / 24.0 * std::f64::consts::TAU;
                (
                    q(f64::from(cx) + f64::from(r) * a.cos()),
                    q(f64::from(cy) + f64::from(r) * a.sin()),
                )
            })
            .collect(),
        true,
        Some(c),
        None,
        None,
    );
}

fn num1(v: Option<f64>, one_decimal: bool) -> String {
    match v {
        None => "--".into(),
        Some(x) if one_decimal => format!("{x:.1}"),
        Some(x) => format!("{x:.0}"),
    }
}

/// A reduction as a numeral: `-6.3` when that fits `room` pixels, else whole dB (`-10`), and `0.0` or `0` for none
/// (never `-0`).
fn gr_numeral(o: &Ops, g: f64, room: i32, t: i32) -> String {
    let one = if g.abs() < 0.05 {
        "0.0".to_string()
    } else {
        format!("{g:.1}")
    };
    if o.width(FontKind::Mono, &one, t) <= room {
        return one;
    }
    if g.abs() < 0.5 {
        "0".into()
    } else {
        format!("{g:.0}")
    }
}

// ---- the build ---------------------------------------------------------------------------------------------------

pub(super) fn build(
    inp: &SceneInput,
    fonts: &Fonts,
    lay: &Layout,
    m: &Metrics,
    rep: &mut Report,
) -> Vec<ZoneOps> {
    let Shown::Processing {
        idx,
        other,
        rotate,
        pinned,
    } = &inp.shown
    else {
        return vec![];
    };
    let st = &inp.stations[*idx];
    rep.header_red.push(("header".into(), st.alarming()));
    let mut zones = vec![
        header(
            inp,
            fonts,
            lay.zone("header"),
            m,
            st,
            other.map(|i| &inp.stations[i]),
            *rotate,
            *pinned,
            rep,
        ),
        now(inp, fonts, lay.zone("now"), m, st),
    ];
    zones.extend(cols(inp, fonts, lay.zone("cols"), m, st, rep));
    if lay.has("spectrum") {
        zones.push(spectrum(inp, fonts, lay.zone("spectrum"), m, st));
    }
    zones.extend(meters(inp, fonts, lay.zone("meters"), m, st, rep));
    zones
}

#[allow(clippy::too_many_arguments)]
fn header(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    m: &Metrics,
    st: &StationModel,
    other: Option<&StationModel>,
    rotate: Option<f64>,
    pinned: bool,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let t = m.t;
    let bg = if st.alarming() {
        theme::RED_BG
    } else {
        theme::PANEL
    };
    let bar_h = if rotate.is_some() {
        scaled(t, 0.1).max(2)
    } else {
        0
    };
    let body = r.h - bar_h;
    let ph = body - scaled(t, 0.26);
    let py = r.y + (body - ph) / 2;
    // the right side first: the clock, then the other station's indicator
    let clock_w = o.line(
        r.right() - m.pad - scaled(t, 0.1),
        r.y,
        body,
        &inp.clock,
        t,
        FontKind::Mono,
        theme::TEXT,
        Align::Right,
        None,
    );
    let mut right = r.right() - m.pad - clock_w - scaled(t, 0.6);
    if let Some(ot) = other {
        let name = fonts.fit(FontKind::Bold, ot.name(), t, scaled(t, 5.8));
        let pad = (ph / 4).max(6);
        let w = o.width(FontKind::Bold, &name, t) + 2 * pad;
        let x = right - w;
        let c = alert_color(ot);
        rep.indicators.push((ot.name().to_string(), c));
        o.rrect(Rect::new(x, py, w, ph), ph / 3, c);
        o.line(
            x + pad,
            r.y,
            body,
            &name,
            t,
            FontKind::Bold,
            theme::BG,
            Align::Left,
            None,
        );
        right = x - scaled(t, 0.5);
    }
    let name_w = o.line(
        r.x + m.pad,
        r.y,
        body,
        st.name(),
        t,
        FontKind::Bold,
        theme::TEXT,
        Align::Left,
        Some(r.w / 3),
    );
    let label = if st.connected {
        st.pill()
    } else {
        "NO CONNECTION"
    };
    let (pb, pf) = pill_colors(label);
    let mut x = r.x + m.pad + name_w + scaled(t, 0.5);
    x += pill(&mut o, x, py, ph, label, t, pb, pf, FontKind::Bold) + scaled(t, 0.4);
    // the newest open alert, so an alarm is never only a red header
    if st.connected {
        let mut a: Vec<_> = st.alerts.iter().collect();
        a.sort_by_key(|x| (!x.critical(), std::cmp::Reverse(x.raised_ms.unwrap_or(0))));
        if let Some(first) = a.first() {
            let more = a.len() - 1;
            let text = if more > 0 {
                format!("{} +{more}", first.code)
            } else {
                first.code.clone()
            };
            let pad = (ph / 4).max(6);
            let room = right - x - 2 * pad;
            if room > o.width(FontKind::Bold, "ab", t) {
                let text = fonts.fit(FontKind::Bold, &text, t, room);
                let (bgc, fgc) = if first.critical() {
                    (theme::RED, theme::BG)
                } else {
                    (theme::AMBER, theme::BG)
                };
                let w = o.width(FontKind::Bold, &text, t) + 2 * pad;
                o.rrect(Rect::new(x, py, w, ph), ph / 3, bgc);
                o.line(
                    x + pad,
                    r.y,
                    body,
                    &text,
                    t,
                    FontKind::Bold,
                    fgc,
                    Align::Left,
                    None,
                );
            }
        }
    }
    if let Some(frac) = rotate {
        let y = r.bottom() - bar_h;
        o.rect(Rect::new(r.x, y, r.w, bar_h), theme::LINE);
        let (c, f) = if pinned {
            (theme::RED, 1.0)
        } else {
            (theme::AMBER, frac.clamp(0.0, 1.0))
        };
        o.rect(
            Rect::new(r.x, y, (f * f64::from(r.w)).round() as i32, bar_h),
            c,
        );
    }
    o.finish("header", r, bg)
}

/// The title and the time left, so the view still says what is on air.
fn now(inp: &SceneInput, fonts: &Fonts, r: Rect, m: &Metrics, st: &StationModel) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let t = m.t;
    if !st.connected {
        o.line(
            r.x + m.pad,
            r.y,
            r.h,
            "NO CONNECTION",
            t,
            FontKind::Bold,
            theme::RED,
            Align::Left,
            None,
        );
        return o.finish("now", r, theme::BG);
    }
    let Some(n) = st.now.as_ref() else {
        o.line(
            r.x + m.pad,
            r.y,
            r.h,
            "Nothing on air",
            t,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
        return o.finish("now", r, theme::BG);
    };
    // the time at the right: the time left, or what the station is doing when it is not playing the item out
    let (txt, color) = if st.idle(inp.now_ms) {
        match segue_text(st, inp.now_ms) {
            Some((l, c)) => (l, c),
            None => ("0:00".into(), theme::DIM),
        }
    } else {
        (
            st.remaining_ms(inp.now_ms)
                .map(countdown)
                .unwrap_or_default(),
            theme::TEXT,
        )
    };
    let tw = o.line(
        r.right() - m.pad,
        r.y,
        r.h,
        &txt,
        t,
        FontKind::Mono,
        color,
        Align::Right,
        None,
    );
    let max_w = (r.w - 2 * m.pad - tw - scaled(t, 0.6)).max(0);
    let title_w = o.line(
        r.x + m.pad,
        r.y,
        r.h,
        &n.title,
        t,
        FontKind::Bold,
        theme::TEXT,
        Align::Left,
        Some(max_w),
    );
    if !n.artist.is_empty() {
        let used = title_w + scaled(t, 0.6);
        if max_w - used > o.width(FontKind::Sans, "abcd", t) {
            o.line(
                r.x + m.pad + used,
                r.y,
                r.h,
                &n.artist,
                t,
                FontKind::Sans,
                theme::DIM,
                Align::Left,
                Some(max_w - used),
            );
        }
    }
    o.finish("now", r, theme::BG)
}

/// The columns of gain-reduction bars, the AGC, the shared scale, the LEDs and the footer. They are drawn as zones of
/// their own, so that a bar that moves redraws (and damages) only its own strip of the screen: the scale's strip, the
/// footer, and for each column its label row, its bar and its numeral. Together they tile the `cols` rectangle.
fn cols(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    m: &Metrics,
    st: &StationModel,
    rep: &mut Report,
) -> Vec<ZoneOps> {
    // `o` only measures; what is drawn goes to the parts below
    let o = Ops::new(fonts);
    let t = m.t;
    let range = inp.cfg.proc_range_db.clamp(6.0, 30.0);
    let mut prep = ProcReport {
        t,
        range_db: range,
        ..ProcReport::default()
    };
    if !st.connected {
        rep.proc = Some(prep);
        return vec![Ops::new(fonts).finish("cols", r, theme::BG)];
    }
    let fr = st.live_frame(inp.now_ms);
    let cols = columns(st, fr, inp.cfg.bypassed_stages == "hide");
    if cols.is_empty() {
        let mut o = Ops::new(fonts);
        o.line(
            r.x + m.pad,
            r.y,
            m.line,
            "CHAIN BYPASSED",
            t,
            FontKind::Bold,
            theme::GREY,
            Align::Left,
            None,
        );
        rep.proc = Some(prep);
        return vec![o.finish("cols", r, theme::BG)];
    }
    let lh = m.line;
    let x0 = r.x + m.pad;
    let x1 = r.right() - m.pad;
    let track_top = r.y + m.pad_v + lh;
    let track_bot = r.bottom() - m.pad_v - 2 * lh - scaled(t, 0.12);
    let track_h = (track_bot - track_top).max(4);
    let num_top = track_bot + scaled(t, 0.06);
    let foot_top = r.bottom() - m.pad_v - lh;
    let has_agc = cols.iter().any(|c| c.kind == ColKind::Agc);
    let n_gr = cols.iter().filter(|c| c.kind != ColKind::Agc).count();
    let agc_w = if has_agc { scaled(t, 3.2) } else { 0 };
    let sw = if n_gr > 0 {
        o.width(FontKind::Sans, "-30", t) + scaled(t, 0.3)
    } else {
        0
    };
    let gx0 = x0 + agc_w + sw;
    let cw = if n_gr > 0 {
        ((x1 - gx0) / n_gr as i32).max(1)
    } else {
        1
    };
    // band names, when the widest fits its column (else B1 to Bn)
    let nb = cols.iter().filter(|c| c.kind == ColKind::Band).count();
    let names = band_names(nb);
    let use_names = !names.is_empty()
        && names
            .iter()
            .all(|n| o.width(FontKind::Bold, n, t) <= cw - scaled(t, 0.2));
    let on_bg = theme::PANEL2;
    let off_bg = Color::rgb(0x1a, 0x1f, 0x27);
    let bw = ((f64::from(cw) * 0.5).round() as i32).clamp(6, scaled(t, 1.7).max(8));
    // the zones: 0 the scale's strip, 1 the footer, then for column k: 2 + 3k its label row, 3 + 3k its bar, 4 + 3k its numeral
    let mut gi_ = 0i32;
    let xr: Vec<(i32, i32)> = cols
        .iter()
        .map(|c| {
            if c.kind == ColKind::Agc {
                (r.x, x0 + agc_w)
            } else {
                let xa = gx0 + gi_ * cw;
                gi_ += 1;
                (
                    xa,
                    if gi_ == n_gr as i32 {
                        r.right()
                    } else {
                        xa + cw
                    },
                )
            }
        })
        .collect();
    let strip_x = if has_agc { x0 + agc_w } else { r.x };
    let mut rects: Vec<(String, Rect)> = vec![
        (
            "cols.strip".into(),
            Rect::new(strip_x, r.y, (gx0 - strip_x).max(0), foot_top - r.y),
        ),
        (
            "cols.foot".into(),
            Rect::new(r.x, foot_top, r.w, r.bottom() - foot_top),
        ),
    ];
    for (k, &(xa, xb)) in xr.iter().enumerate() {
        rects.push((
            format!("cols.label.{k}"),
            Rect::new(xa, r.y, xb - xa, track_top - r.y),
        ));
        rects.push((
            format!("cols.bar.{k}"),
            Rect::new(xa, track_top, xb - xa, track_bot - track_top),
        ));
        rects.push((
            format!("cols.num.{k}"),
            Rect::new(xa, track_bot, xb - xa, foot_top - track_bot),
        ));
    }
    let mut parts: Vec<Ops> = rects.iter().map(|_| Ops::new(fonts)).collect();
    // the shared scale: a line every GRID_DB, labeled in a strip left of the first gain-reduction column
    if n_gr > 0 {
        let mut d = 0.0;
        let mut label_step = 1usize;
        while f64::from(track_h) * GRID_DB * label_step as f64 / range < f64::from(t) * 1.15 {
            label_step += 1;
        }
        let mut k = 0usize;
        while d <= range + 1e-9 {
            let y = track_top + ((d / range) * f64::from(track_h)).round() as i32;
            prep.grid.push((d, y));
            if k.is_multiple_of(label_step) {
                let txt = if d == 0.0 {
                    "0".to_string()
                } else {
                    format!("-{d:.0}")
                };
                // centered on its line, but the last one sits on top of the track's bottom edge, clear of the numerals
                let base = baseline_in(y - lh / 2, lh, t).min(track_bot);
                parts[0].text(
                    gx0 - scaled(t, 0.15),
                    base,
                    &txt,
                    t,
                    FontKind::Sans,
                    theme::DIM,
                    Align::Right,
                    None,
                );
            }
            d += GRID_DB;
            k += 1;
        }
    }
    let mut gi = 0i32;
    let mut band_i = 0usize;
    for (k, c) in cols.iter().enumerate() {
        let (pl, pb, pn) = (2 + 3 * k, 3 + 3 * k, 4 + 3 * k);
        let (cx0, cwid) = if c.kind == ColKind::Agc {
            (x0, agc_w)
        } else {
            let v = (gx0 + gi * cw, cw);
            gi += 1;
            v
        };
        let rect = Rect::new(cx0, r.y, cwid, r.h);
        let cx = cx0 + cwid / 2;
        let label = match c.kind {
            ColKind::Band => {
                let l = if use_names {
                    names[band_i].clone()
                } else {
                    c.label.clone()
                };
                band_i += 1;
                l
            }
            _ => c.label.clone(),
        };
        let lab_color = if c.enabled { theme::TEXT } else { theme::GREY };
        let mut led_state: Option<bool> = None;
        // the label, and the LED of the AGC (gate frozen) and of the clipper (clipping now)
        let has_led = matches!(c.kind, ColKind::Agc | ColKind::Clip) && c.enabled;
        let lr = scaled(t, 0.2).max(3);
        let lw = o.width(FontKind::Bold, &label, t);
        let (label_x, led_x) = if has_led {
            let total = lw + scaled(t, 0.3) + 2 * lr;
            let lx = cx - total / 2;
            (lx + lw / 2, lx + lw + scaled(t, 0.3) + lr)
        } else {
            (cx, 0)
        };
        parts[pl].line(
            label_x,
            r.y + m.pad_v,
            lh,
            &label,
            t,
            FontKind::Bold,
            lab_color,
            Align::Center,
            None,
        );
        if has_led {
            let lit = match c.kind {
                ColKind::Agc => fr.is_some_and(|f| f.flags & 2 != 0),
                _ => fr.is_some_and(|f| f.flags & 16 != 0),
            };
            led_state = Some(lit);
            let col = match (c.kind, lit) {
                (_, false) => theme::GREY,
                (ColKind::Agc, true) => theme::AMBER,
                _ => theme::RED,
            };
            led(&mut parts[pl], led_x, r.y + m.pad_v + lh / 2, lr, col);
        }
        let track_w = if c.kind == ColKind::Agc {
            scaled(t, 0.8)
        } else {
            bw
        };
        let track_x = if c.kind == ColKind::Agc {
            cx0 + cwid - track_w - scaled(t, 0.15)
        } else {
            cx - track_w / 2
        };
        let track = Rect::new(track_x, track_top, track_w, track_h);
        parts[pb].rect(track, if c.enabled { on_bg } else { off_bg });
        let mut bar = Rect::new(track.x, track.y, track.w, 0);
        let mut bar_db = 0.0;
        let mut hold_y = None;
        if !c.enabled {
            parts[pb].line(
                cx,
                track.y + track.h / 2 - lh / 2,
                lh,
                "BYP",
                t,
                FontKind::Bold,
                theme::GREY,
                Align::Center,
                None,
            );
        } else if c.kind == ColKind::Agc {
            // the AGC: center zero, boost up and cut down, with its own scale written into the strip beside it
            let mid = track.y + track.h / 2;
            let half = f64::from(track.h) / 2.0;
            for (txt, base) in [
                ("+9", Some(track.y + scaled(t, CAP_RATIO_F) + 2)),
                ("0", None),
                ("-9", Some(track.bottom() - 1)),
            ] {
                let right = track.x - scaled(t, 0.2);
                match base {
                    Some(b) => {
                        parts[pb].text(
                            right,
                            b,
                            txt,
                            t,
                            FontKind::Sans,
                            theme::DIM,
                            Align::Right,
                            None,
                        );
                    }
                    None => {
                        parts[pb].line(
                            right,
                            mid - lh / 2,
                            lh,
                            txt,
                            t,
                            FontKind::Sans,
                            theme::DIM,
                            Align::Right,
                            None,
                        );
                    }
                }
            }
            if let Some(g) = c.raw {
                bar_db = g;
                let px = ((g.abs() / AGC_RANGE_DB).clamp(0.0, 1.0) * half).round() as i32;
                bar = if g >= 0.0 {
                    Rect::new(track.x, mid - px, track.w, px)
                } else {
                    Rect::new(track.x, mid, track.w, px)
                };
                parts[pb].rect(bar, theme::BLUE_LINE);
            }
            parts[pb].rect(Rect::new(track.x, mid, track.w, 1), theme::DIM);
        } else {
            // a lone multiband column (no per-band values) has no ballistics of its own: the frame's mean
            let still = common::meters::GrMeter::default();
            let meter = c.slot.map_or(&still, |s| &st.dynamics.gr[s]);
            bar_db = c
                .slot
                .map_or_else(|| c.raw.map_or(0.0, |g| (-g).max(0.0)), |_| meter.bar);
            let px = ((bar_db / range).clamp(0.0, 1.0) * f64::from(track.h)).round() as i32;
            bar = Rect::new(track.x, track.y, track.w, px);
            parts[pb].rect(bar, theme::AMBER);
            if let Some(h) = meter.hold {
                let hy =
                    track.y + ((h / range).clamp(0.0, 1.0) * f64::from(track.h)).round() as i32;
                let th = scaled(t, 0.1).max(2);
                let y = (hy - th / 2).clamp(track.y, track.bottom() - th);
                parts[pb].rect(
                    Rect::new(track.x, y, track.w, th),
                    Color::rgb(255, 255, 255),
                );
                hold_y = Some(y);
            }
        }
        // the numeral: what the frame says, now. One decimal when the column has room for it, else whole dB.
        if c.enabled {
            let (txt, col) = match c.raw {
                None => ("--".to_string(), theme::DIM),
                Some(g) if c.kind == ColKind::Agc => (format!("{g:+.1}"), theme::TEXT),
                Some(g) => (
                    gr_numeral(&o, g, cw - scaled(t, 0.2), t),
                    if g.abs() < 0.05 {
                        theme::DIM
                    } else {
                        theme::TEXT
                    },
                ),
            };
            parts[pn].line(
                cx,
                num_top,
                lh,
                &txt,
                t,
                FontKind::Mono,
                col,
                Align::Center,
                None,
            );
        }
        prep.cols.push(ProcCol {
            name: c.name.clone(),
            label,
            rect,
            track,
            bar,
            bypassed: !c.enabled,
            gr_db: c.raw,
            bar_db,
            range_db: if c.kind == ColKind::Agc {
                AGC_RANGE_DB
            } else {
                range
            },
            centered: c.kind == ColKind::Agc,
            hold_y,
            led: led_state,
        });
    }
    // the scale's lines over the tracks and the bars (a graticule), drawn last of the column graphics
    for (k, c) in cols.iter().enumerate() {
        if c.kind == ColKind::Agc {
            continue;
        }
        let (xa, xb) = xr[k];
        for &(_, y) in &prep.grid {
            parts[3 + 3 * k].rect(
                Rect::new(xa, y.min(track_bot - 1), xb.min(x1) - xa, 1),
                theme::TEXT.with_alpha(70),
            );
        }
    }
    // the footer: the crossovers, the AGC gate, the limiter's ceiling
    let en = |n: &str| cols.iter().any(|c| c.name == n && c.enabled);
    let gate = has_agc && fr.is_some_and(|f| f.flags & 2 != 0) && en("agc");
    let ceiling = st
        .chain
        .as_ref()
        .and_then(|c| c.ceiling_dbtp)
        .filter(|_| en("limiter"));
    prep.ceiling = ceiling;
    let xo: Vec<f64> = st
        .chain
        .as_ref()
        .map(|c| c.crossovers.clone())
        .unwrap_or_default();
    let mut right_w = 0;
    if let Some(cl) = ceiling {
        let txt = format!("CEIL {cl:.1} dBTP");
        right_w = parts[1].line(
            x1,
            foot_top,
            lh,
            &txt,
            t,
            FontKind::Mono,
            theme::DIM,
            Align::Right,
            None,
        );
    }
    let mut left_end = x0;
    if gate {
        left_end =
            x0 + parts[1].line(
                x0,
                foot_top,
                lh,
                "AGC GATE",
                t,
                FontKind::Bold,
                theme::AMBER,
                Align::Left,
                None,
            ) + scaled(t, 0.8);
    }
    if en("band1") && !xo.is_empty() {
        let list: Vec<String> = xo.iter().map(|f| hz_text(*f)).collect();
        let txt = format!("XOVER {}", list.join(" "));
        let room = x1 - left_end - right_w - if right_w > 0 { scaled(t, 0.8) } else { 0 };
        if room > 0 {
            parts[1].line(
                left_end,
                foot_top,
                lh,
                &txt,
                t,
                FontKind::Mono,
                theme::DIM,
                Align::Left,
                Some(room),
            );
        }
    }
    rep.proc = Some(prep);
    rects
        .into_iter()
        .zip(parts)
        .map(|((name, rect), o)| o.finish(&name, rect, theme::BG))
        .collect()
}

/// The spectrum under the bars, on a screen with room for it.
fn spectrum(inp: &SceneInput, fonts: &Fonts, r: Rect, m: &Metrics, st: &StationModel) -> ZoneOps {
    let mut o = Ops::new(fonts);
    if !st.connected {
        return o.finish("spectrum", r, theme::BG);
    }
    let t = m.t;
    let lab_w = o.width(FontKind::Sans, "-40", t) + scaled(t, 0.4);
    let plot = Rect::new(
        r.x + m.pad + lab_w,
        r.y + m.pad_v,
        r.w - 2 * m.pad - lab_w,
        r.h - 2 * m.pad_v,
    );
    let yat = |db: f64| {
        plot.y + ((1.0 - ((db + 60.0) / 60.0).clamp(0.0, 1.0)) * f64::from(plot.h)).round() as i32
    };
    for db in [-20.0, -40.0] {
        o.rect(Rect::new(plot.x, yat(db), plot.w, 1), theme::LINE);
        o.line(
            plot.x - scaled(t, 0.2),
            yat(db) - m.line / 2,
            m.line,
            &format!("{db:.0}"),
            t,
            FontKind::Sans,
            theme::DIM,
            Align::Right,
            None,
        );
    }
    let src = inp.cfg.spectrum_source.as_str();
    let d = &st.dynamics;
    let n = 64usize;
    let bw = f64::from(plot.w) / n as f64;
    if src != "pre" {
        for i in 0..n {
            let x0 = plot.x + (i as f64 * bw).round() as i32;
            let x1 = plot.x + ((i + 1) as f64 * bw).round() as i32;
            let top = yat(d.post[i]);
            o.rect(
                Rect::new(x0, top, (x1 - x0 - 1).max(1), plot.bottom() - top),
                theme::BLUE,
            );
            o.rect(
                Rect::new(
                    x0,
                    (yat(d.cap_post[i].0) - 1).max(plot.y),
                    (x1 - x0 - 1).max(1),
                    2,
                ),
                theme::BLUE_LINE,
            );
        }
    }
    if src != "post" {
        let pts: Vec<(i32, i32)> = (0..n)
            .map(|i| {
                (
                    q(f64::from(plot.x) + (i as f64 + 0.5) * bw),
                    q(f64::from(yat(d.pre[i]))),
                )
            })
            .collect();
        o.path(
            pts,
            false,
            None,
            Some((theme::TEXT.with_alpha(200), 16)),
            None,
        );
    }
    o.finish("spectrum", r, theme::BG)
}

/// The level in and out and the loudness controller. Drawn as zones of their own (a bar and its numeral for each of the
/// three levels, and the controller), so that a moving bar damages only its own strip. They tile the `meters` rectangle.
fn meters(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    m: &Metrics,
    st: &StationModel,
    rep: &mut Report,
) -> Vec<ZoneOps> {
    // `o` only measures; what is drawn goes to the parts below
    let o = Ops::new(fonts);
    if !st.connected {
        return vec![Ops::new(fonts).finish("meters", r, theme::BG)];
    }
    let t = m.t;
    let fr = st.live_frame(inp.now_ms);
    let row = scaled(t, 1.22);
    let left_x = r.x + m.pad;
    let split_x = r.x + (f64::from(r.w) * 0.53).round() as i32;
    let left_r = split_x - scaled(t, 0.9);
    let div_x = split_x - scaled(t, 0.45);
    let lab_w = o.width(FontKind::Bold, "IN", t) + scaled(t, 0.3);
    let num_w = o.width(FontKind::Mono, "-88.8", t);
    let bar_x = left_x + lab_w;
    let bar_w = (left_r - num_w - scaled(t, 0.3) - bar_x).max(20);
    let bar_h = scaled(t, 0.78).max(6);
    // the zones: 0 to 2 the label and bar of IN, L and R, 3 to 5 their numerals, 6 the controller (with the divider)
    let row_y = |i: i32| match i {
        0 => r.y,
        _ => r.y + m.pad_v + i * row,
    };
    let row_b = |i: i32| {
        if i == 2 {
            r.bottom()
        } else {
            r.y + m.pad_v + (i + 1) * row
        }
    };
    let bars_end = bar_x + bar_w;
    let mut rects: Vec<(String, Rect)> = vec![];
    for i in 0..3 {
        rects.push((
            format!("meters.bar.{i}"),
            Rect::new(r.x, row_y(i), bars_end - r.x, row_b(i) - row_y(i)),
        ));
    }
    for i in 0..3 {
        rects.push((
            format!("meters.num.{i}"),
            Rect::new(bars_end, row_y(i), div_x - bars_end, row_b(i) - row_y(i)),
        ));
    }
    rects.push((
        "meters.loud".into(),
        Rect::new(div_x, r.y, r.right() - div_x, r.h),
    ));
    let mut parts: Vec<Ops> = rects.iter().map(|_| Ops::new(fonts)).collect();
    parts[6].rect(
        Rect::new(div_x, r.y + m.pad_v, 1, r.h - 2 * m.pad_v),
        theme::LINE,
    );
    let (in_bar, in_hold) = st.dynamics.input();
    let ceiling = st
        .chain
        .as_ref()
        .and_then(|c| c.ceiling_dbtp)
        .filter(|_| fr.is_some_and(|f| f.stage_enabled(3) && !f.bypassed()));
    for (i, (label, zone)) in [("IN", "proc.in"), ("L", "proc.L"), ("R", "proc.R")]
        .into_iter()
        .enumerate()
    {
        let top = r.y + m.pad_v + i as i32 * row;
        let (level, hold) = match i {
            0 => (in_bar, in_hold),
            c => (
                st.dynamics.level(c - 1, fr),
                st.dynamics.peak_hold[c - 1].map(|h| h.0),
            ),
        };
        parts[i].line(
            left_x,
            top,
            row,
            label,
            t,
            FontKind::Bold,
            theme::DIM,
            Align::Left,
            None,
        );
        let track = Rect::new(bar_x, top + (row - bar_h) / 2, bar_w, bar_h);
        ppm_bar(
            &mut parts[i],
            rep,
            zone,
            track,
            level,
            hold,
            &[-40.0, -20.0, 0.0],
        );
        if i > 0 {
            if let Some(cl) = ceiling {
                let x = db_x(track, cl).min(track.right() - 3);
                parts[i].rect(Rect::new(x, track.y - 3, 3, track.h + 6), theme::AMBER);
            }
        }
        parts[3 + i].line(
            left_r,
            top,
            row,
            &num1(level, true),
            t,
            FontKind::Mono,
            if level.is_some() {
                theme::TEXT
            } else {
                theme::DIM
            },
            Align::Right,
            None,
        );
    }
    // the loudness controller at the right
    let rx = split_x;
    let rw = r.right() - m.pad - rx;
    let state = fr.map_or(0u8, |f| f.loudness_state);
    let (label, bg, fg) = match state {
        1 => ("CONVERGING", theme::PANEL2, theme::BLUE_LINE),
        2 => ("ON TARGET", Color::rgb(0x14, 0x4a, 0x35), theme::GREEN),
        3 => ("FROZEN", Color::rgb(0x55, 0x3d, 0x0c), theme::AMBER),
        4 => ("AT LIMIT", theme::RED_BG, theme::RED),
        _ => ("OFF", theme::PANEL2, theme::DIM),
    };
    let top = r.y + m.pad_v;
    let ph = row - scaled(t, 0.2);
    let pw = pill(
        &mut parts[6],
        rx,
        top + (row - ph) / 2,
        ph,
        label,
        t,
        bg,
        fg,
        FontKind::Bold,
    );
    let gain = if state == 0 {
        String::new()
    } else {
        format!("{:+.1} dB", fr.map_or(0.0, |f| f.gain_db))
    };
    rep.loudness.push((st.id.clone(), gain.clone()));
    if !gain.is_empty() {
        parts[6].line(
            rx + pw + scaled(t, 0.5),
            top,
            row,
            &gain,
            t,
            FontKind::Mono,
            theme::TEXT,
            Align::Left,
            Some((rw - pw - scaled(t, 0.5)).max(0)),
        );
    }
    let f1 = |v: Option<f64>| fmt_lu(v);
    let target = fr.and_then(|f| f.target);
    let l2 = format!(
        "TARGET {}   W {}",
        target.map_or("--".into(), |v| format!("{v:.1}")),
        f1(fr.and_then(|f| f.window))
    );
    let l3 = format!(
        "S {}   I {}",
        f1(fr.and_then(|f| f.short_term)),
        f1(fr.and_then(|f| f.integrated))
    );
    for (k, text) in [l2, l3].into_iter().enumerate() {
        rep.loudness.push((st.id.clone(), text.clone()));
        parts[6].line(
            rx,
            top + (k as i32 + 1) * row,
            row,
            &text,
            t,
            FontKind::Mono,
            theme::TEXT,
            Align::Left,
            Some(rw),
        );
    }
    rects
        .into_iter()
        .zip(parts)
        .map(|((name, rect), o)| o.finish(&name, rect, theme::BG))
        .collect()
}

/// The texts of the mini processing strip, which are drawn smaller than the legibility minimum: a deliberate, documented
/// exception (the strip is learned by shape and position from the chair, and read up close when needed). The glyph-log
/// tests allow a run under the minimum only when it is in this list.
pub const MINI_LABELS: &[&str] = &[
    "AGC", "LOW", "LMID", "MID", "HMID", "HIGH", "PRES", "B1", "B2", "B3", "B4", "B5", "B6", "MB",
    "WB", "LIM", "CLP", "0", "-6", "-12", "-18", "-24", "-30",
];

/// The mini processing strip of the compact `single` and `split` views: the gain-reduction bars (bands, wideband,
/// limiter, clipper, hanging from the top, with peak ticks and a faint scale) and the AGC's centered bar, as a miniature
/// of the processing view. Its labels (band names, `WB`, `LIM`, `CLP`, `AGC`, and the scale's `0`, `-6`, `-12`) are
/// small (`ls` px, under the legibility minimum, see `MINI_LABELS`); everything else on the screen keeps the minimum.
/// Shown by `display.mini_processing`: `on` always (dark tracks when the chain is off), `auto` while the chain is not
/// fully bypassed, `off` never. Returns whether it drew.
pub(super) fn mini(
    o: &mut Ops,
    r: Rect,
    right_aligned: bool,
    ls: i32,
    inp: &SceneInput,
    st: &StationModel,
    rep: &mut Report,
) -> bool {
    let mode = inp.cfg.mini_processing.as_str();
    let fr = st.live_frame(inp.now_ms);
    let chain_on = fr.is_some_and(|f| !f.bypassed());
    if !st.connected || mode == "off" || (mode != "on" && !chain_on) {
        return false;
    }
    let cols = columns(st, fr, false);
    let lab_h = ls + 6;
    let gutter = o.width(FontKind::Bold, "-12", ls) + 4;
    if cols.is_empty() || r.h < lab_h + 24 || r.w < gutter + 8 * cols.len() as i32 {
        return false;
    }
    let range = inp.cfg.proc_range_db.clamp(6.0, 30.0);
    let n = cols.len();
    let has_agc = cols.iter().any(|c| c.kind == ColKind::Agc);
    let units = n as f64 + if has_agc { 0.5 } else { 0.0 };
    // at most 40 px a column: a wide space gets a strip of its own width, aligned to its side
    let total = (gutter + (units * 40.0) as i32).min(r.w);
    let x0 = if right_aligned {
        r.right() - total
    } else {
        r.x + (r.w - total) / 2
    };
    let tr = Rect::new(x0 + gutter, r.y + 2, total - gutter, r.h - lab_h - 2);
    let pitch = f64::from(tr.w) / units;
    let bw = ((pitch * 0.58).round() as i32).clamp(3, 22);
    let mut prep = ProcReport {
        t: ls,
        range_db: range,
        ..ProcReport::default()
    };
    let off_bg = Color::rgb(0x1a, 0x1f, 0x27);
    // the band names, when the widest fits a column
    let nb = cols.iter().filter(|c| c.kind == ColKind::Band).count();
    let names = band_names(nb);
    let use_names = !names.is_empty()
        && names
            .iter()
            .all(|nm| o.width(FontKind::Bold, nm, ls) <= pitch as i32 - 2);
    let mut band_i = 0usize;
    let mut gr_n = 0usize;
    for (k, c) in cols.iter().enumerate() {
        let slot_x =
            f64::from(tr.x) + pitch * (k as f64 + if has_agc && k > 0 { 0.5 } else { 0.0 });
        let cx = (slot_x + pitch / 2.0).round() as i32;
        let track = Rect::new(cx - bw / 2, tr.y, bw, tr.h);
        o.rect(track, if c.enabled { theme::PANEL2 } else { off_bg });
        let mut bar = Rect::new(track.x, track.y, track.w, 0);
        let mut bar_db = 0.0;
        let mut hold_y = None;
        if c.enabled && c.kind == ColKind::Agc {
            let mid = track.y + track.h / 2;
            if let Some(g) = c.raw {
                bar_db = g;
                let px = ((g.abs() / AGC_RANGE_DB).clamp(0.0, 1.0) * f64::from(track.h) / 2.0)
                    .round() as i32;
                bar = if g >= 0.0 {
                    Rect::new(track.x, mid - px, track.w, px)
                } else {
                    Rect::new(track.x, mid, track.w, px)
                };
                o.rect(bar, theme::BLUE_LINE);
            }
            o.rect(Rect::new(track.x, mid, track.w, 1), theme::DIM);
        } else if c.enabled {
            let still = common::meters::GrMeter::default();
            let meter = c.slot.map_or(&still, |sl| &st.dynamics.gr[sl]);
            bar_db = c
                .slot
                .map_or_else(|| c.raw.map_or(0.0, |g| (-g).max(0.0)), |_| meter.bar);
            let px = ((bar_db / range).clamp(0.0, 1.0) * f64::from(track.h)).round() as i32;
            bar = Rect::new(track.x, track.y, track.w, px);
            o.rect(bar, theme::AMBER);
            if let Some(h) = meter.hold {
                let hy =
                    track.y + ((h / range).clamp(0.0, 1.0) * f64::from(track.h)).round() as i32;
                let y = (hy - 1).clamp(track.y, track.bottom() - 2);
                o.rect(Rect::new(track.x, y, track.w, 2), Color::rgb(255, 255, 255));
                hold_y = Some(y);
            }
        }
        if c.kind != ColKind::Agc {
            gr_n += 1;
        }
        let label = match c.kind {
            ColKind::Band => {
                let l = if use_names {
                    names[band_i].clone()
                } else {
                    c.label.clone()
                };
                band_i += 1;
                l
            }
            _ => c.label.clone(),
        };
        o.text(
            cx,
            tr.bottom() + ls + 3,
            &label,
            ls,
            FontKind::Bold,
            if c.enabled { theme::DIM } else { theme::GREY },
            Align::Center,
            None,
        );
        prep.cols.push(ProcCol {
            name: c.name.clone(),
            label,
            rect: Rect::new(slot_x.round() as i32, r.y, pitch.round() as i32, r.h),
            track,
            bar,
            bypassed: !c.enabled,
            gr_db: c.raw,
            bar_db,
            range_db: if c.kind == ColKind::Agc {
                AGC_RANGE_DB
            } else {
                range
            },
            centered: c.kind == ColKind::Agc,
            hold_y,
            led: None,
        });
    }
    // the scale: a faint line every 6 dB over the gain-reduction bars, with `0`, `-6`, `-12` in the gutter
    if gr_n > 0 {
        let first = prep
            .cols
            .iter()
            .find(|c| !c.centered)
            .map_or(tr.x, |c| c.track.x - 3);
        let mut d = 0.0;
        while d < range - 1e-9 {
            let y = tr.y + ((d / range) * f64::from(tr.h)).round() as i32;
            prep.grid.push((d, y));
            if d > 0.0 {
                o.rect(
                    Rect::new(first, y, tr.right() - first, 1),
                    theme::TEXT.with_alpha(60),
                );
            }
            let txt = if d == 0.0 {
                "0".to_string()
            } else {
                format!("-{d:.0}")
            };
            if f64::from(tr.h) * GRID_DB / range >= f64::from(ls) + 2.0 {
                let base = if d == 0.0 {
                    y + ls * 3 / 4 + 1
                } else {
                    y + ls / 3 + 1
                };
                o.text(
                    x0 + gutter - 3,
                    base,
                    &txt,
                    ls,
                    FontKind::Bold,
                    theme::DIM,
                    Align::Right,
                    None,
                );
            }
            d += GRID_DB;
        }
    }
    rep.mini.push((st.id.clone(), prep));
    true
}
