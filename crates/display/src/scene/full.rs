//! The full layout of 11.2 (1280 x 720 and up): now playing with artwork, next and queue, program and loudness
//! meters, stage meters, the processing view of 11.2a, the as-run log and the alarms. Every length is scaled by
//! `s = min(width / 1920, height / 1080)`, and text never goes under the plan's minimum: where it would, the
//! elements are dropped in the order of 11.2 instead (`layout::drops`).

use super::*;
use crate::layout::{drops, Drops};
use dspmodel::x_frac;

struct F {
    s: f64,
    min: i32,
    d: Drops,
}

impl F {
    /// A reference size at 1080p, scaled, never under the minimum.
    fn fs(&self, r: f64) -> i32 {
        ((r * self.s).round() as i32).max(self.min)
    }
    /// A length (not text) scaled.
    fn l(&self, r: f64) -> i32 {
        ((r * self.s).round() as i32).max(1)
    }
}

pub(super) fn build(
    inp: &SceneInput,
    fonts: &Fonts,
    lay: &Layout,
    cache: &mut SceneCache,
    rep: &mut Report,
) -> Vec<ZoneOps> {
    let f = F {
        s: inp.plan.s(),
        min: inp.plan.min_px,
        d: drops(&inp.plan),
    };
    let Shown::Single {
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
    if f.d.album {
        rep.notes.push("dropped: album line".into());
    }
    vec![
        header(
            inp,
            fonts,
            lay.zone("header"),
            &f,
            st,
            other.map(|i| &inp.stations[i]),
            *rotate,
            *pinned,
            rep,
        ),
        now(inp, fonts, lay.zone("now"), &f, st, rep),
        next(inp, fonts, lay.zone("next"), &f, st, rep),
        meters(inp, fonts, lay.zone("meters"), &f, st, rep),
        stages(inp, fonts, lay.zone("stages"), &f, st, rep),
        processing(inp, fonts, lay.zone("processing"), &f, st, cache, rep),
        asrun(inp, fonts, lay.zone("asrun"), &f, st),
        alarms(inp, fonts, lay.zone("alarms"), &f, *idx),
    ]
}

#[allow(clippy::too_many_arguments)]
fn header(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    other: Option<&StationModel>,
    rotate: Option<f64>,
    pinned: bool,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(16.0);
    let bg = if st.alarming() {
        theme::RED_BG
    } else {
        theme::PANEL
    };
    let bar_h = if rotate.is_some() { f.l(3.0).max(2) } else { 0 };
    let body = r.h - bar_h;
    let (sn, sp, sc, sprod) = (f.fs(34.0), f.fs(24.0), f.fs(34.0), f.fs(24.0));
    let name_w = o.line(
        r.x + pad,
        r.y,
        body,
        st.name(),
        sn,
        FontKind::Bold,
        theme::TEXT,
        Align::Left,
        Some(r.w / 4),
    );
    let label = if st.connected {
        st.pill()
    } else {
        "NO CONNECTION"
    };
    let (pb, pf) = pill_colors(label);
    let ph = (f64::from(sp) * 1.45).round() as i32;
    let pw = pill(
        &mut o,
        r.x + pad + name_w + f.l(18.0),
        r.y + (body - ph) / 2,
        ph.min(body),
        label,
        sp,
        pb,
        pf,
        FontKind::Bold,
    );
    o.line(
        r.x + pad + name_w + f.l(18.0) + pw + f.l(24.0),
        r.y,
        body,
        &inp.product,
        sprod,
        FontKind::Sans,
        theme::DIM,
        Align::Left,
        None,
    );
    let clock_w = o.line(
        r.right() - pad,
        r.y,
        body,
        &inp.clock,
        sc,
        FontKind::Mono,
        theme::TEXT,
        Align::Right,
        None,
    );
    if let Some(ot) = other {
        // the other station's strip: its name, its remaining time and its alert color
        let rem = ot
            .remaining_ms(inp.now_ms)
            .map(countdown)
            .unwrap_or_default();
        let text = if ot.connected {
            format!("{}  {}", ot.name(), rem)
        } else {
            format!("{}  NO CONNECTION", ot.name())
        };
        let ssz = f.fs(24.0);
        let w = o.width(FontKind::Bold, &text, ssz) + 2 * f.l(12.0);
        let x = r.right() - pad - clock_w - f.l(28.0) - w;
        rep.indicators
            .push((ot.name().to_string(), alert_color(ot)));
        o.rrect(
            Rect::new(x, r.y + (body - ph) / 2, w, ph.min(body)),
            ph / 3,
            alert_color(ot),
        );
        o.line(
            x + f.l(12.0),
            r.y,
            body,
            &text,
            ssz,
            FontKind::Bold,
            theme::BG,
            Align::Left,
            None,
        );
    }
    if let Some(frac) = rotate {
        let y = r.bottom() - bar_h;
        o.rect(Rect::new(r.x, y, r.w, bar_h), theme::LINE);
        let (c, fr) = if pinned {
            (theme::RED, 1.0)
        } else {
            (theme::AMBER, frac.clamp(0.0, 1.0))
        };
        o.rect(
            Rect::new(r.x, y, (fr * f64::from(r.w)).round() as i32, bar_h),
            c,
        );
    }
    o.finish("header", r, bg)
}

fn now(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(12.0);
    let bar_h = f.l(14.0);
    if !st.connected {
        o.line(
            r.x + pad,
            r.y,
            r.h,
            "NO CONNECTION",
            f.fs(52.0),
            FontKind::Bold,
            theme::RED,
            Align::Left,
            Some(r.w - 2 * pad),
        );
        return o.finish("now", r, theme::BG);
    }
    let n = st.now.as_ref();
    let art = (r.h - bar_h - 2 * pad).min(r.w / 2);
    let art_rect = Rect::new(r.x + pad, r.y + pad, art, art);
    art_op(
        &mut o,
        inp,
        art_rect,
        n.and_then(|n| n.art_url.as_deref()),
        600,
    );
    let tx = art_rect.right() + f.l(24.0);
    let max_w = r.right() - pad - tx;
    let (title, artist, album) = n.map_or(
        ("Nothing on air".to_string(), String::new(), String::new()),
        |n| (n.title.clone(), n.artist.clone(), n.album.clone()),
    );
    let (st_t, st_a, st_al, st_ch, st_rem, st_el, st_sg) = (
        f.fs(44.0),
        f.fs(36.0),
        f.fs(28.0),
        f.fs(22.0),
        f.fs(88.0),
        f.fs(48.0),
        f.fs(36.0),
    );
    let mut y = r.y + pad;
    let tl = (f64::from(st_t) * 1.15).round() as i32;
    for l in fonts.wrap(FontKind::Bold, &title, st_t, max_w, 3) {
        o.line(
            tx,
            y,
            tl,
            &l,
            st_t,
            FontKind::Bold,
            if n.is_some() { theme::TEXT } else { theme::DIM },
            Align::Left,
            Some(max_w),
        );
        y += tl;
    }
    let al = (f64::from(st_a) * 1.2).round() as i32;
    o.line(
        tx,
        y,
        al,
        &artist,
        st_a,
        FontKind::Sans,
        theme::TEXT,
        Align::Left,
        Some(max_w),
    );
    y += al;
    if !f.d.album && !album.is_empty() {
        let l = (f64::from(st_al) * 1.2).round() as i32;
        o.line(
            tx,
            y,
            l,
            &album,
            st_al,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            Some(max_w),
        );
        y += l;
    }
    // the chips: category, and ALBUM when the boundary ahead is gapless
    let chh = (f64::from(st_ch) * 1.5).round() as i32;
    let mut cx = tx;
    if let Some(n) = n {
        if !n.category.is_empty() {
            cx += pill(
                &mut o,
                cx,
                y + f.l(4.0),
                chh,
                &n.category.to_uppercase(),
                st_ch,
                theme::PANEL2,
                theme::DIM,
                FontKind::Sans,
            ) + f.l(10.0);
        }
        if n.continuity_next {
            pill(
                &mut o,
                cx,
                y + f.l(4.0),
                chh,
                "ALBUM",
                st_ch,
                Color::rgb(0x14, 0x3a, 0x5a),
                theme::BLUE_LINE,
                FontKind::Bold,
            );
        }
    }
    y += chh + f.l(10.0);
    // remaining (big) and elapsed
    let rem = st.remaining_ms(inp.now_ms);
    let rl = (f64::from(st_rem) * 1.1).round() as i32;
    let digits = rem.map_or("0:00".to_string(), countdown);
    let dw = o.line(
        tx,
        y,
        rl,
        &digits,
        st_rem,
        FontKind::Mono,
        if rem.is_some() {
            theme::TEXT
        } else {
            theme::DIM
        },
        Align::Left,
        None,
    );
    o.line(
        tx + dw + f.l(12.0),
        y,
        rl,
        "LEFT",
        st_ch,
        FontKind::Sans,
        theme::DIM,
        Align::Left,
        None,
    );
    rep.segue.push((st.id.clone(), digits));
    y += rl;
    let el = (f64::from(st_el) * 1.1).round() as i32;
    let ed = st
        .elapsed_ms(inp.now_ms)
        .map_or("0:00".to_string(), elapsed);
    let ew = o.line(
        tx,
        y,
        el,
        &ed,
        st_el,
        FontKind::Mono,
        theme::DIM,
        Align::Left,
        None,
    );
    o.line(
        tx + ew + f.l(12.0),
        y,
        el,
        "ELAPSED",
        st_ch,
        FontKind::Sans,
        theme::DIM,
        Align::Left,
        None,
    );
    y += el;
    if let Some((t, c)) = segue_text(st, inp.now_ms) {
        let sl = (f64::from(st_sg) * 1.2).round() as i32;
        o.line(
            tx,
            y,
            sl,
            &t,
            st_sg,
            FontKind::Bold,
            c,
            Align::Left,
            Some(max_w),
        );
        rep.segue.push((st.id.clone(), t));
    }
    // the progress bar spans the zone, with ticks at the segue and at the end
    let bar = Rect::new(r.x, r.bottom() - bar_h, r.w, bar_h);
    progress_bar(&mut o, bar, st, inp.now_ms);
    o.rect(Rect::new(bar.right() - 3, bar.y, 3, bar.h), theme::TEXT);
    o.finish("now", r, theme::BG)
}

fn next(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(12.0);
    if !st.connected {
        return o.finish("next", r, theme::BG);
    }
    if st.queue.is_empty() {
        o.line(
            r.x + pad,
            r.y + pad,
            f.fs(36.0),
            "Queue empty",
            f.fs(30.0),
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
        return o.finish("next", r, theme::BG);
    }
    let thumb = f.l(144.0);
    let card_h = thumb + 2 * pad;
    let foot_h = f.fs(24.0) * 3 / 2 + pad;
    let rows_h = r.h - card_h - foot_h;
    let nrows = 5usize;
    let row_h = (rows_h / nrows as i32).max(1);
    let shown_rows = if f.d.queue_rows_beyond_3 { 3 } else { nrows };
    // the next item as a card
    let e = &st.queue[0];
    rep.upcoming.push(row_title(e));
    let tr = Rect::new(r.x + pad, r.y + pad, thumb, thumb);
    if e.kind == "block" {
        o.rrect(tr, f.l(8.0), theme::PANEL2);
    } else {
        art_op(&mut o, inp, tr, e.art_url.as_deref(), 144);
    }
    let cx = tr.right() + f.l(20.0);
    let cw = r.right() - pad - cx;
    let (s1, s2, s3) = (f.fs(40.0), f.fs(30.0), f.fs(30.0));
    let lh1 = (f64::from(s1) * 1.2).round() as i32;
    let lh2 = (f64::from(s2) * 1.25).round() as i32;
    o.line(
        cx,
        r.y + pad,
        f.fs(24.0) * 3 / 2,
        "NEXT",
        f.fs(22.0),
        FontKind::Bold,
        theme::DIM,
        Align::Left,
        None,
    );
    let y1 = r.y + pad + f.fs(24.0) * 3 / 2;
    o.line(
        cx,
        y1,
        lh1,
        &row_title(e),
        s1,
        FontKind::Bold,
        theme::TEXT,
        Align::Left,
        Some(cw),
    );
    o.line(
        cx,
        y1 + lh1,
        lh2,
        &e.artist,
        s2,
        FontKind::Sans,
        theme::DIM,
        Align::Left,
        Some(cw),
    );
    let tail = match e.kind.as_str() {
        "block" => st
            .block_countdown_ms(e, inp.now_ms)
            .map(|ms| (format!("in {}", countdown(ms)), urgency(ms, 60_000, 10_000))),
        _ => e
            .planned_start_ms
            .map(|m| (format!("at {}", hhmm(m, inp.tz_min)), theme::TEXT)),
    };
    if let Some((t, c)) = tail {
        o.line(
            cx,
            y1 + lh1 + lh2,
            lh2,
            &t,
            s3,
            FontKind::Mono,
            c,
            Align::Left,
            None,
        );
    }
    // the queue rows
    let top = r.y + card_h;
    o.rect(Rect::new(r.x + pad, top - 1, r.w - 2 * pad, 1), theme::LINE);
    for (i, e) in st.queue.iter().skip(1).take(shown_rows).enumerate() {
        let ry = top + i as i32 * row_h;
        let th = row_h - f.l(8.0);
        let mut x = r.x + pad;
        let right = r.right() - pad;
        let s_row = f.fs(28.0);
        let is_block = e.kind == "block";
        let (rt, rc) = if is_block {
            match st.block_countdown_ms(e, inp.now_ms) {
                Some(ms) => (countdown(ms), urgency(ms, 60_000, 10_000)),
                None => (String::new(), theme::DIM),
            }
        } else {
            (
                e.planned_start_ms
                    .map(|m| hhmm(m, inp.tz_min))
                    .unwrap_or_default(),
                theme::DIM,
            )
        };
        let rw = if rt.is_empty() {
            0
        } else {
            o.line(
                right,
                ry,
                row_h,
                &rt,
                s_row,
                FontKind::Mono,
                rc,
                Align::Right,
                None,
            )
        };
        let mut max_right = right - rw - f.l(14.0);
        let badge = if is_block {
            e.block_type.to_uppercase()
        } else {
            origin_badge(&e.origin)
        };
        let bh = (f64::from(s_row) * 1.35).round() as i32;
        if !badge.is_empty() {
            let w = o.width(FontKind::Bold, &badge, f.fs(22.0)) + 2 * f.l(10.0);
            let bx = max_right - w;
            pill(
                &mut o,
                bx,
                ry + (row_h - bh) / 2,
                bh,
                &badge,
                f.fs(22.0),
                theme::PANEL2,
                if is_block { theme::AMBER } else { theme::DIM },
                FontKind::Bold,
            );
            max_right = bx - f.l(12.0);
        }
        if !is_block {
            let tr = Rect::new(x, ry + (row_h - th) / 2, th, th);
            art_op(&mut o, inp, tr, e.art_url.as_deref(), 144);
            x += th + f.l(12.0);
        }
        let title = row_title(e);
        rep.upcoming.push(title.clone());
        let tw = o.line(
            x,
            ry,
            row_h,
            &title,
            s_row,
            FontKind::Sans,
            theme::TEXT,
            Align::Left,
            Some((max_right - x).max(0) * 3 / 5),
        );
        if !e.artist.is_empty() {
            let ax = x + tw + f.l(14.0);
            o.line(
                ax,
                ry,
                row_h,
                &e.artist,
                s_row,
                FontKind::Sans,
                theme::DIM,
                Align::Left,
                Some((max_right - ax).max(0)),
            );
        }
    }
    // the last line: the next timed block
    if let Some(b) = st.queue.iter().find(|e| e.kind == "block") {
        let fy = r.bottom() - foot_h;
        let ms = st.block_countdown_ms(b, inp.now_ms);
        let t = match ms {
            Some(m) => format!(
                "NEXT BLOCK  {}  {}  in {}",
                b.block_type.to_uppercase(),
                b.name,
                countdown(m)
            ),
            None => format!("NEXT BLOCK  {}  {}", b.block_type.to_uppercase(), b.name),
        };
        o.line(
            r.x + pad,
            fy,
            foot_h,
            &t,
            f.fs(24.0),
            FontKind::Sans,
            ms.map_or(theme::DIM, |m| urgency(m, 60_000, 10_000)),
            Align::Left,
            Some(r.w - 2 * pad),
        );
    }
    o.finish("next", r, theme::BG)
}

fn meters(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(14.0);
    if !st.connected {
        return o.finish("meters", r, theme::BG);
    }
    let fr = st.live_frame(inp.now_ms);
    let lab = f.fs(20.0);
    let lab_h = (f64::from(lab) * 1.3).round() as i32;
    let bar_h = f.l(28.0);
    let gap = f.l(6.0);
    let x = r.x + pad;
    let w = r.w - 2 * pad;
    let mut y = r.y + f.l(6.0);
    let t0 = Rect::new(x, 0, w, 0);
    for t in [-60.0, -40.0, -20.0, -10.0, 0.0] {
        let tx = db_x(t0, t);
        let al = if t == -60.0 {
            Align::Left
        } else if t == 0.0 {
            Align::Right
        } else {
            Align::Center
        };
        let tx = if t == 0.0 { x + w } else { tx };
        o.line(
            tx,
            y,
            lab_h,
            &format!("{t:.0}"),
            lab,
            FontKind::Sans,
            theme::DIM,
            al,
            None,
        );
    }
    y += lab_h;
    for i in 0..2 {
        let track = Rect::new(x, y, w, bar_h);
        let hold = st.dynamics.peak_hold[i].map(|h| h.0);
        ppm_bar(
            &mut o,
            rep,
            "meters",
            track,
            st.dynamics.level(i, fr),
            hold,
            &[-60.0, -40.0, -20.0, -10.0, 0.0],
        );
        y += bar_h + gap;
    }
    y += f.l(6.0);
    // M, S, W, I in large digits, 2 x 2
    let ns = f.fs(36.0);
    let row_h = (f64::from(ns) * 1.25).round() as i32;
    let colw = w / 2;
    let vals = [
        ("M", fr.and_then(|f| f.momentary)),
        ("S", fr.and_then(|f| f.short_term)),
        ("W", fr.and_then(|f| f.window)),
        ("I", fr.and_then(|f| f.integrated)),
    ];
    for (i, (k, v)) in vals.iter().enumerate() {
        let (cx, ry) = (x + (i as i32 % 2) * colw, y + (i as i32 / 2) * row_h);
        let dim = matches!(*k, "M" | "S");
        o.line(
            cx,
            ry,
            row_h,
            k,
            ns,
            FontKind::Bold,
            theme::DIM,
            Align::Left,
            None,
        );
        o.line(
            cx + colw - f.l(14.0),
            ry,
            row_h,
            &fmt_lu(*v),
            ns,
            FontKind::Mono,
            if dim { theme::DIM } else { theme::TEXT },
            Align::Right,
            None,
        );
        rep.loudness.push(((*k).into(), fmt_lu(*v)));
    }
    y += 2 * row_h + f.l(8.0);
    // the loudness bar: W as the fill, the target as a marker (hidden with the controller off)
    let target = fr.and_then(|f| f.target);
    let centre = target.unwrap_or(-14.0);
    let (lo, hi) = (centre - 9.0, centre + 9.0);
    let lb = Rect::new(x, y, w, f.l(18.0));
    o.rect(lb, theme::PANEL2);
    let lx =
        |v: f64| lb.x + (((v - lo) / (hi - lo)).clamp(0.0, 1.0) * f64::from(lb.w)).round() as i32;
    if let Some(wv) = fr.and_then(|f| f.window) {
        o.rect(Rect::new(lb.x, lb.y, lx(wv) - lb.x, lb.h), theme::BLUE_LINE);
    }
    if let Some(t) = target {
        o.rect(
            Rect::new(lx(t) - 1, lb.y - f.l(4.0), 3, lb.h + f.l(8.0)),
            theme::TEXT,
        );
    }
    y += lb.h + f.l(10.0);
    let ctl = controller_text(st, inp.now_ms);
    let txt = match (target, ctl.is_empty()) {
        (Some(t), false) => format!("TARGET {t:.0}   {ctl}"),
        (Some(t), true) => format!("TARGET {t:.0}"),
        _ => "CONTROLLER OFF".into(),
    };
    o.line(
        x,
        y,
        f.fs(24.0) * 3 / 2,
        &txt,
        f.fs(24.0),
        FontKind::Sans,
        theme::DIM,
        Align::Left,
        Some(w),
    );
    o.finish("meters", r, theme::BG)
}

// ---- stage meters ---------------------------------------------------------------------------------------------------

struct Col {
    label: String,
    enabled: bool,
    input: Option<f64>,
    output: Option<f64>,
    gr: Option<f64>,
    range: f64,
    centered: bool,
}

fn stage_columns(st: &StationModel, fr: Option<&Frame>, hide: bool) -> Vec<Col> {
    use crate::frame::STAGES;
    let names: Vec<&str> = match st.chain.as_ref().filter(|c| !c.stages.is_empty()) {
        Some(c) => c
            .stages
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| STAGES[..5].contains(n))
            .collect(),
        None => STAGES[..5].to_vec(),
    };
    let chain_bypassed = fr.is_none_or(|f| f.bypassed());
    let mut cols = vec![];
    for name in names {
        let i = STAGES.iter().position(|s| *s == name).unwrap_or(0);
        let en = !chain_bypassed && fr.is_some_and(|f| f.stage_enabled(i));
        let m = fr.map(|f| &f.stages[i]);
        match name {
            "multiband" => {
                let n = if en {
                    fr.map_or(0, |f| f.band_count as usize)
                } else {
                    st.chain.as_ref().map_or(0, |c| c.bands)
                };
                if n >= 2 {
                    for b in 0..n.min(6) {
                        cols.push(Col {
                            label: format!("B{}", b + 1),
                            enabled: en,
                            input: m.and_then(|m| m.input),
                            output: m.and_then(|m| m.output),
                            gr: fr.and_then(|f| f.band_gr[b]).filter(|_| en),
                            range: 12.0,
                            centered: false,
                        });
                    }
                } else {
                    cols.push(Col {
                        label: "MB".into(),
                        enabled: en,
                        input: m.and_then(|m| m.input),
                        output: m.and_then(|m| m.output),
                        gr: m.and_then(|m| m.gr).filter(|_| en),
                        range: 12.0,
                        centered: false,
                    });
                }
            }
            other => {
                let (label, range, centered) = match other {
                    "agc" => ("AGC", 9.0, true),
                    "wideband" => ("WB", 12.0, false),
                    "limiter" => ("LIM", 6.0, false),
                    "clipper" => ("CLP", 6.0, false),
                    _ => ("?", 12.0, false),
                };
                cols.push(Col {
                    label: label.into(),
                    enabled: en,
                    input: m.and_then(|m| m.input),
                    output: m.and_then(|m| m.output),
                    gr: m.and_then(|m| m.gr).filter(|_| en),
                    range,
                    centered,
                });
            }
        }
    }
    if hide {
        cols.retain(|c| c.enabled);
    }
    cols
}

fn stages(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    if !st.connected {
        return o.finish("stages", r, theme::BG);
    }
    let fr = st.live_frame(inp.now_ms);
    let pad = f.l(10.0);
    let lab = f.fs(20.0);
    let lab_h = (f64::from(lab) * 1.35).round() as i32;
    // the strip on top: the safety limiter's light
    let strip = lab_h + f.l(4.0);
    let safe_on = fr.is_some_and(|f| f.flags & 8 != 0);
    let rad = f.l(8.0).max(5);
    let lx = r.right() - pad - rad;
    o.path(
        (0..24)
            .map(|i| {
                let a = f64::from(i) / 24.0 * std::f64::consts::TAU;
                (
                    q(f64::from(lx) + f64::from(rad) * a.cos()),
                    q(f64::from(r.y + pad + lab_h / 2) + f64::from(rad) * a.sin()),
                )
            })
            .collect(),
        true,
        Some(if safe_on {
            theme::RED
        } else {
            theme::GREEN.with_alpha(110)
        }),
        None,
        None,
    );
    o.line(
        lx - rad - f.l(8.0),
        r.y + pad,
        lab_h,
        "SAFE",
        lab,
        FontKind::Sans,
        theme::DIM,
        Align::Right,
        None,
    );
    let cols = stage_columns(st, fr, inp.cfg.bypassed_stages == "hide");
    let area = Rect::new(
        r.x + pad,
        r.y + pad + strip,
        r.w - 2 * pad,
        r.h - 2 * pad - strip,
    );
    if cols.is_empty() {
        o.line(
            r.x + pad,
            r.y + pad,
            lab_h,
            "CHAIN BYPASSED",
            lab,
            FontKind::Sans,
            theme::GREY,
            Align::Left,
            None,
        );
        return o.finish("stages", r, theme::BG);
    }
    let cw = area.w / cols.len() as i32;
    // numerals: two lines of integers under each column, unless they do not fit or have been dropped
    let wide_enough = cw >= o.width(FontKind::Sans, "-88", lab) + f.l(4.0);
    let numerals = !f.d.stage_numerals && wide_enough;
    let num_h = if numerals { 2 * lab_h } else { 0 };
    for (i, c) in cols.iter().enumerate() {
        let rect = Rect::new(area.x + i as i32 * cw, area.y, cw, area.h);
        let cx = rect.x + cw / 2;
        let col_color = if c.enabled { theme::TEXT } else { theme::GREY };
        o.line(
            cx,
            rect.y,
            lab_h,
            &c.label,
            lab,
            FontKind::Bold,
            col_color,
            Align::Center,
            None,
        );
        let track_top = rect.y + lab_h + f.l(4.0);
        let track_h = (rect.h - lab_h - f.l(4.0) - num_h - f.l(4.0)).max(4);
        let tw = (cw * 2 / 5).max(f.l(10.0));
        let track = Rect::new(cx - tw / 2, track_top, tw, track_h);
        o.rect(
            track,
            if c.enabled {
                theme::PANEL2
            } else {
                Color::rgb(0x1a, 0x1f, 0x27)
            },
        );
        let mut bar = Rect::new(track.x, track.y, track.w, 0);
        if c.enabled {
            if let Some(gr) = c.gr {
                if c.centered {
                    let mid = track.y + track.h / 2;
                    let half = f64::from(track.h) / 2.0;
                    let px = ((gr.abs() / c.range).clamp(0.0, 1.0) * half).round() as i32;
                    bar = if gr >= 0.0 {
                        Rect::new(track.x, mid - px, track.w, px)
                    } else {
                        Rect::new(track.x, mid, track.w, px)
                    };
                    o.rect(Rect::new(track.x, mid, track.w, 1), theme::DIM);
                } else {
                    let px = ((-gr / c.range).clamp(0.0, 1.0) * f64::from(track.h)).round() as i32;
                    bar = Rect::new(track.x, track.y, track.w, px);
                }
                o.rect(
                    bar,
                    if c.centered {
                        theme::BLUE_LINE
                    } else {
                        theme::AMBER
                    },
                );
            }
        } else {
            o.line(
                cx,
                track.y + track.h / 2 - lab_h / 2,
                lab_h,
                "BYP",
                lab,
                FontKind::Bold,
                theme::GREY,
                Align::Center,
                None,
            );
        }
        if numerals && c.enabled {
            let fmt = |v: Option<f64>| v.map_or("--".into(), |x| format!("{x:.0}"));
            o.line(
                cx,
                track.bottom() + f.l(4.0),
                lab_h,
                &fmt(c.input),
                lab,
                FontKind::Sans,
                theme::DIM,
                Align::Center,
                None,
            );
            o.line(
                cx,
                track.bottom() + f.l(4.0) + lab_h,
                lab_h,
                &fmt(c.output),
                lab,
                FontKind::Sans,
                theme::TEXT,
                Align::Center,
                None,
            );
        }
        rep.stages.push(StageCol {
            name: c.label.clone(),
            rect,
            bypassed: !c.enabled,
            gr_db: c.gr,
            track,
            bar,
            range_db: c.range,
            has_numerals: numerals && c.enabled,
        });
    }
    o.finish("stages", r, theme::BG)
}

// ---- the processing view (11.2a) --------------------------------------------------------------------------------------

const GRID_HZ: [(f64, &str); 8] = [
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1000.0, "1k"),
    (2000.0, "2k"),
    (5000.0, "5k"),
    (10_000.0, "10k"),
];

fn processing(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    f: &F,
    st: &StationModel,
    cache: &mut SceneCache,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    if !st.connected {
        return o.finish("processing", r, theme::BG);
    }
    let fr = st.live_frame(inp.now_ms);
    let lab = f.fs(20.0);
    let lab_h = (f64::from(lab) * 1.35).round() as i32;
    // the plot: the whole zone width, 0 dB at the top edge, a strip of axis labels under it
    let plot = Rect::new(r.x, r.y, r.w, r.h - lab_h);
    rep.plot = Some(plot);
    rep.curve_range_db = inp.cfg.curve_range_db;
    let view = inp.cfg.processing_view.as_str();
    let want_curve = view != "spectrum";
    let want_spec = view != "curve";
    let src = if f.d.pre_overlay && inp.cfg.spectrum_source == "both" {
        "post"
    } else {
        inp.cfg.spectrum_source.as_str()
    };
    let px_x = |fz: f64| f64::from(plot.x) + x_frac(fz).clamp(0.0, 1.0) * f64::from(plot.w);
    // gridlines and labels
    let few = f.s < 0.8;
    for (hz, text) in GRID_HZ {
        let x = px_x(hz).round() as i32;
        o.rect(Rect::new(x, plot.y, 1, plot.h), theme::LINE);
        if !few || matches!(hz as i64, 100 | 1000 | 10_000) {
            o.line(
                x,
                plot.bottom(),
                lab_h,
                text,
                lab,
                FontKind::Sans,
                theme::DIM,
                Align::Center,
                None,
            );
        }
    }
    let range = inp.cfg.curve_range_db;
    if want_spec {
        for db in [-20.0, -40.0] {
            let y = plot.bottom() - (((db + 60.0) / 60.0) * f64::from(plot.h)).round() as i32;
            o.rect(Rect::new(plot.x, y, plot.w, 1), theme::LINE.with_alpha(120));
            o.line(
                plot.x + f.l(4.0),
                y - lab_h,
                lab_h,
                &format!("{db:.0}"),
                lab,
                FontKind::Sans,
                theme::DIM,
                Align::Left,
                None,
            );
        }
    }
    // the spectrum
    let nb = if plot.w >= 800 && !f.d.spectrum_32 {
        64
    } else {
        32
    };
    if want_spec {
        rep.spectrum_bands = nb;
        let dy = &st.dynamics;
        let merge = |a: &[f64; 64], i: usize| {
            if nb == 64 {
                a[i]
            } else {
                a[2 * i].max(a[2 * i + 1])
            }
        };
        let merge_cap = |a: &[(f64, i64); 64], i: usize| {
            if nb == 64 {
                a[i].0
            } else {
                a[2 * i].0.max(a[2 * i + 1].0)
            }
        };
        let ypos = |db: f64| {
            plot.bottom()
                - (((db + 60.0) / 60.0).clamp(0.0, 1.0) * f64::from(plot.h)).round() as i32
        };
        let live = fr.is_some();
        let draw = |o: &mut Ops,
                    rep_bars: &mut Vec<SpecBar>,
                    vals: &dyn Fn(usize) -> f64,
                    caps: &dyn Fn(usize) -> f64,
                    fill: bool| {
            for i in 0..nb {
                let x0 = plot.x + (i as f64 / nb as f64 * f64::from(plot.w)).round() as i32;
                let x1 = plot.x + ((i + 1) as f64 / nb as f64 * f64::from(plot.w)).round() as i32;
                let (top, cap) = (
                    if live { ypos(vals(i)) } else { plot.bottom() },
                    if live { ypos(caps(i)) } else { plot.bottom() },
                );
                if fill {
                    o.rect(
                        Rect::new(x0, top, (x1 - x0 - 1).max(1), plot.bottom() - top),
                        theme::BLUE,
                    );
                    o.rect(
                        Rect::new(x0, (cap - 1).max(plot.y), (x1 - x0 - 1).max(1), 2),
                        theme::BLUE_LINE,
                    );
                }
                rep_bars.push(SpecBar {
                    band: i,
                    x: x0,
                    w: x1 - x0,
                    top,
                    cap,
                });
            }
        };
        if src == "pre" {
            draw(
                &mut o,
                &mut rep.spectrum_pre,
                &|i| merge(&dy.pre, i),
                &|i| merge_cap(&dy.cap_pre, i),
                true,
            );
            rep.layers.push("spectrum_pre");
        } else {
            draw(
                &mut o,
                &mut rep.spectrum_post,
                &|i| merge(&dy.post, i),
                &|i| merge_cap(&dy.cap_post, i),
                true,
            );
            rep.layers.push("spectrum_post");
            if src == "both" {
                // the pre-processing outline in front of the post fill: the difference is the processing
                let mut pts = vec![];
                let mut bars = vec![];
                draw(
                    &mut o,
                    &mut bars,
                    &|i| merge(&dy.pre, i),
                    &|i| merge_cap(&dy.cap_pre, i),
                    false,
                );
                for b in &bars {
                    pts.push((q(f64::from(b.x)), q(f64::from(b.top))));
                    pts.push((q(f64::from(b.x + b.w)), q(f64::from(b.top))));
                }
                o.path(
                    pts,
                    false,
                    None,
                    Some((theme::TEXT.with_alpha(200), q8(f.s.max(1.0)))),
                    None,
                );
                rep.spectrum_pre = bars;
                rep.layers.push("spectrum_pre");
            }
        }
    }
    // the gain curve and the crossovers
    if want_curve {
        let chain = st.chain.as_ref();
        let mb_on = fr.is_some_and(|f| !f.bypassed() && f.stage_enabled(1));
        let xo: Vec<f64> = chain.map(|c| c.crossovers.clone()).unwrap_or_default();
        let have = mb_on && !xo.is_empty() && chain.is_some_and(|c| c.bands == xo.len() + 1);
        let yc = |db: f64| f64::from(plot.y) + (-db / range).clamp(0.0, 1.0) * f64::from(plot.h);
        if have {
            let n = (plot.w as usize).clamp(8, 512);
            let table = cache.table(&xo, n);
            let gr: Vec<f64> = (0..xo.len() + 1)
                .map(|b| fr.and_then(|f| f.band_gr[b]).unwrap_or(0.0))
                .collect();
            let mk: Vec<f64> = if inp.cfg.curve_include_makeup {
                chain.map(|c| c.makeup_db.clone()).unwrap_or_default()
            } else {
                vec![]
            };
            let db = table.curve_db(&gr, &mk);
            let mut pts = vec![];
            for (i, d) in db.iter().enumerate() {
                let x = f64::from(plot.x) + i as f64 / (n - 1) as f64 * f64::from(plot.w);
                let y = yc(*d);
                rep.curve.push(CurvePoint {
                    f: table.freqs[i],
                    db: *d,
                    x,
                    y,
                });
                pts.push((q(x), q(y)));
            }
            let mut fill = pts.clone();
            fill.push((q(f64::from(plot.right())), q(f64::from(plot.y))));
            fill.push((q(f64::from(plot.x)), q(f64::from(plot.y))));
            o.path(fill, true, Some(theme::AMBER.with_alpha(45)), None, None);
            o.path(
                pts,
                false,
                None,
                Some((theme::AMBER, q8((f.s * 2.5).max(1.5)))),
                None,
            );
            // crossover lines with the band numbers
            let mut edges = vec![20.0];
            edges.extend(&xo);
            edges.push(20_000.0);
            for (k, &fc) in xo.iter().enumerate() {
                let x = px_x(fc);
                rep.crossovers.push((fc, x));
                o.path(
                    vec![
                        (q(x), q(f64::from(plot.y))),
                        (q(x), q(f64::from(plot.bottom()))),
                    ],
                    false,
                    None,
                    Some((theme::TEXT.with_alpha(190), q8((f.s * 1.6).max(1.5)))),
                    Some((f.l(5.0).max(3), f.l(4.0).max(2))),
                );
                let _ = k;
            }
            for b in 0..edges.len() - 1 {
                let mid = (edges[b] * edges[b + 1]).sqrt();
                o.line(
                    px_x(mid).round() as i32,
                    plot.y + f.l(4.0),
                    lab_h,
                    &format!("{}", b + 1),
                    lab,
                    FontKind::Bold,
                    theme::AMBER,
                    Align::Center,
                    None,
                );
            }
            rep.layers.push("curve");
        } else if inp.cfg.bypassed_stages != "hide" {
            // bypassed: a flat line at 0 dB labeled BYP
            let y = f64::from(plot.y) + 1.0;
            let pts = vec![
                (q(f64::from(plot.x)), q(y)),
                (q(f64::from(plot.right())), q(y)),
            ];
            o.path(
                pts,
                false,
                None,
                Some((theme::GREY, q8((f.s * 2.0).max(1.5)))),
                None,
            );
            o.line(
                plot.right() - f.l(8.0),
                plot.y + f.l(6.0),
                lab_h,
                "BYP",
                lab,
                FontKind::Bold,
                theme::GREY,
                Align::Right,
                None,
            );
            for hz in [20.0, 20_000.0] {
                rep.curve.push(CurvePoint {
                    f: hz,
                    db: 0.0,
                    x: px_x(hz),
                    y,
                });
            }
            rep.layers.push("curve_bypassed");
        }
        if want_curve && (view == "both" || have) {
            for db in [-6.0, -12.0] {
                if -db <= range {
                    let y = yc(db).round() as i32;
                    o.line(
                        plot.right() - f.l(6.0),
                        y - lab_h,
                        lab_h,
                        &format!("{db:.0}"),
                        lab,
                        FontKind::Sans,
                        theme::AMBER.with_alpha(200),
                        Align::Right,
                        None,
                    );
                }
            }
        }
    }
    o.finish("processing", r, theme::BG)
}

// ---- as-run log and alarms ----------------------------------------------------------------------------------------------

fn asrun(inp: &SceneInput, fonts: &Fonts, r: Rect, f: &F, st: &StationModel) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(14.0);
    let sz = f.fs(26.0);
    let row_h = (f64::from(sz) * 1.28).round() as i32;
    let cap = if f.d.asrun_rows_beyond_4 { 4 } else { 6 };
    let rows = (((r.h - pad) / row_h).max(1) as usize).min(cap);
    if !st.connected {
        return o.finish("asrun", r, theme::BG);
    }
    let _ = inp;
    for (i, a) in st.asrun.iter().take(rows).enumerate() {
        let y = r.y + f.l(6.0) + i as i32 * row_h;
        let right = r.right() - pad;
        let mut max_right = right;
        let badge = end_badge(&a.end_reason);
        if !badge.is_empty() {
            let w = o.width(FontKind::Bold, &badge, f.fs(22.0)) + 2 * f.l(8.0);
            let bh = row_h - f.l(6.0);
            pill(
                &mut o,
                right - w,
                y + f.l(3.0),
                bh,
                &badge,
                f.fs(22.0),
                theme::PANEL2,
                theme::DIM,
                FontKind::Bold,
            );
            max_right = right - w - f.l(10.0);
        }
        if let Some(d) = a.deviation_ms {
            let t = format!("{:+.1}s", d as f64 / 1000.0);
            let w = o.line(
                max_right,
                y,
                row_h,
                &t,
                f.fs(22.0),
                FontKind::Mono,
                theme::AMBER,
                Align::Right,
                None,
            );
            max_right -= w + f.l(12.0);
        }
        let tw = o.line(
            r.x + pad,
            y,
            row_h,
            &hhmm(a.started_ms, inp.tz_min),
            sz,
            FontKind::Mono,
            theme::DIM,
            Align::Left,
            None,
        );
        let text = if a.artist.is_empty() {
            a.title.clone()
        } else {
            format!("{}  {}", a.title, a.artist)
        };
        let tx = r.x + pad + tw + f.l(14.0);
        o.line(
            tx,
            y,
            row_h,
            &text,
            sz,
            FontKind::Sans,
            theme::TEXT,
            Align::Left,
            Some((max_right - tx).max(0)),
        );
    }
    if st.asrun.is_empty() {
        o.line(
            r.x + pad,
            r.y + f.l(6.0),
            row_h,
            "No items finished yet",
            sz,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
    }
    o.finish("asrun", r, theme::BG)
}

fn alarms(inp: &SceneInput, fonts: &Fonts, r: Rect, f: &F, idx: usize) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = f.l(14.0);
    let sz = f.fs(26.0);
    let row_h = (f64::from(sz) * 1.28).round() as i32;
    let st = &inp.stations[idx];
    let mut y = r.y + f.l(6.0);
    for n in inp.notices {
        o.line(
            r.x + pad,
            y,
            row_h,
            n,
            sz,
            FontKind::Bold,
            theme::AMBER,
            Align::Left,
            Some(r.w - 2 * pad),
        );
        y += row_h;
    }
    for (i, s) in inp.stations.iter().enumerate() {
        if i != idx && !s.connected {
            o.line(
                r.x + pad,
                y,
                row_h,
                &format!("{}: NO CONNECTION", s.name()),
                sz,
                FontKind::Bold,
                theme::RED,
                Align::Left,
                Some(r.w - 2 * pad),
            );
            y += row_h;
        }
    }
    if !st.connected {
        o.line(
            r.x + pad,
            y,
            row_h,
            "NO CONNECTION",
            sz,
            FontKind::Bold,
            theme::RED,
            Align::Left,
            Some(r.w - 2 * pad),
        );
        return o.finish("alarms", r, theme::BG);
    }
    let mut a: Vec<_> = st.alerts.iter().collect();
    a.sort_by_key(|x| {
        (
            std::cmp::Reverse(x.raised_ms.unwrap_or(0)),
            std::cmp::Reverse(x.id),
        )
    });
    if a.is_empty() {
        o.line(
            r.x + pad,
            y,
            row_h,
            "NO ALARMS PENDING",
            sz,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
    }
    for x in a.iter().take(4) {
        if y + row_h > r.bottom() {
            break;
        }
        let c = if x.critical() {
            theme::RED
        } else {
            theme::AMBER
        };
        let age = x.raised_ms.map(|m| {
            let s = ((inp.now_ms - m) / 1000).max(0);
            if s >= 3600 {
                format!("{}h", s / 3600)
            } else if s >= 60 {
                format!("{}m", s / 60)
            } else {
                format!("{s}s")
            }
        });
        let aw = age.as_ref().map_or(0, |t| {
            o.line(
                r.right() - pad,
                y,
                row_h,
                t,
                f.fs(22.0),
                FontKind::Mono,
                theme::DIM,
                Align::Right,
                None,
            ) + f.l(12.0)
        });
        let cw = o.line(
            r.x + pad,
            y,
            row_h,
            &x.code,
            sz,
            FontKind::Bold,
            c,
            Align::Left,
            Some((r.w - 2 * pad - aw) / 2),
        );
        let mx = r.x + pad + cw + f.l(14.0);
        o.line(
            mx,
            y,
            row_h,
            &x.message,
            sz,
            FontKind::Sans,
            theme::TEXT,
            Align::Left,
            Some((r.right() - pad - aw - mx).max(0)),
        );
        y += row_h;
    }
    o.finish("alarms", r, theme::BG)
}

use crate::frame::Frame;
