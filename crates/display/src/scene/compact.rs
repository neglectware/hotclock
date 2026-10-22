//! The compact layouts of 11.2b: 1024 x 600 (primary) and 800 x 480 (fallback), in `single` (also `rotate`) and
//! `split`. Type sizes and what drops out are the spec's; nothing smaller than the plan's floor is ever drawn.

use super::*;

/// The type sizes of one compact layout.
struct Sz {
    digits: i32,
    title: i32,
    name: i32,
    clock: i32,
    artist: i32,
    loud: i32,
    small: i32,
    k: f64,
    c800: bool,
}

impl Sz {
    fn new(plan: &Plan, v: Variant, kind: Kind) -> Sz {
        let (_, _, k) = plan.scales(v);
        let p = |d: f64| plan.px(d, k);
        let c800 = v == Variant::C800;
        let split = kind == Kind::Split;
        if c800 {
            Sz {
                digits: p(if split { 48.0 } else { 64.0 }),
                title: p(if split { 24.0 } else { 32.0 }),
                name: p(22.0),
                clock: p(22.0),
                artist: p(22.0),
                loud: p(22.0),
                small: p(19.0),
                k,
                c800,
            }
        } else {
            Sz {
                digits: p(if split { 56.0 } else { 88.0 }),
                title: p(if split { 31.0 } else { 44.0 }),
                name: p(31.0),
                clock: p(31.0),
                artist: p(31.0),
                loud: p(31.0),
                small: p(31.0),
                k,
                c800,
            }
        }
    }
    fn sc(&self, d: f64) -> i32 {
        (d * self.k).round() as i32
    }
}

fn lh(size: i32, ratio: f64) -> i32 {
    (f64::from(size) * ratio).round() as i32
}

pub(super) fn build(
    inp: &SceneInput,
    fonts: &Fonts,
    lay: &Layout,
    rep: &mut Report,
) -> Vec<ZoneOps> {
    let sz = Sz::new(&inp.plan, lay.variant, lay.kind);
    let mut zones = vec![];
    match &inp.shown {
        Shown::Processing { .. } => {}
        Shown::Single {
            idx,
            other,
            rotate,
            pinned,
        } => {
            let st = &inp.stations[*idx];
            let hdr = header_single(
                inp,
                fonts,
                lay.zone("header"),
                &sz,
                st,
                other.map(|i| &inp.stations[i]),
                *rotate,
                *pinned,
                rep,
            );
            rep.header_red.push(("header".into(), st.alarming()));
            zones.push(hdr);
            zones.push(now_single(inp, fonts, lay.zone("now"), &sz, st, rep));
            zones.push(next_zone(
                inp,
                fonts,
                "next",
                lay.zone("next"),
                &sz,
                st,
                2,
                rep,
                false,
            ));
            zones.push(meters_single(inp, fonts, lay.zone("meters"), &sz, st, rep));
            zones.push(alarm_single(inp, fonts, lay.zone("alarm"), &sz, *idx));
        }
        Shown::Split { left, right } => {
            zones.push(top_bar(inp, fonts, lay.zone("bar"), &sz, rep));
            for (side, idx) in [("L", *left), ("R", *right)] {
                let st = &inp.stations[idx];
                let p = |n: &str| format!("{side}.{n}");
                rep.header_red.push((p("header"), st.alarming()));
                zones.push(header_half(
                    fonts,
                    &p("header"),
                    lay.zone(&p("header")),
                    &sz,
                    st,
                ));
                zones.push(now_half(
                    inp,
                    fonts,
                    &p("now"),
                    lay.zone(&p("now")),
                    &sz,
                    st,
                    rep,
                ));
                zones.push(next_zone(
                    inp,
                    fonts,
                    &p("next"),
                    lay.zone(&p("next")),
                    &sz,
                    st,
                    1,
                    rep,
                    true,
                ));
                zones.push(meters_half(
                    inp,
                    fonts,
                    &p("meters"),
                    lay.zone(&p("meters")),
                    &sz,
                    st,
                    rep,
                ));
                zones.push(alarms_half(
                    inp,
                    fonts,
                    &p("alarms"),
                    lay.zone(&p("alarms")),
                    &sz,
                    idx,
                    rep,
                ));
            }
            if let Some(d) = lay.divider {
                zones.push(Ops::new(fonts).finish("divider", d, theme::LINE));
            }
        }
    }
    zones
}

/// The shared top bar of `split`: the network dot and the product name and version at the left, the clock in the
/// middle, the date and the box's CPU use at the right, each item by its `display.bar_*` setting, all at the clock's
/// size. The left text gives way (ellipsized) before it reaches the clock.
fn top_bar(inp: &SceneInput, fonts: &Fonts, bar: Rect, sz: &Sz, rep: &mut Report) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let b = inp.bar;
    let it = b.items;
    let size = sz.clock;
    let pad = sz.sc(12.0);
    let gap = sz.sc(19.0);
    let gap_r = sz.sc(37.0);
    let base = baseline_in(bar.y, bar.h, size);
    // a text run's box: its drawn width over the line's ascent and descent
    let rect_of = |x: i32, w: i32| {
        let (a, d) = (
            (size as f32 * 0.95).round() as i32,
            (size as f32 * 0.25).round() as i32,
        );
        Rect::new(x, base - a, w, a + d)
    };
    let item = |rep: &mut Report, name: &str, text: &str, r: Rect| {
        rep.bar_items.push((name.into(), text.into(), r));
    };
    let mid = bar.x + bar.w / 2;
    let (clock_l, clock_r) = if it.clock {
        let w = o.line(
            mid,
            bar.y,
            bar.h,
            &inp.clock,
            size,
            FontKind::Mono,
            theme::TEXT,
            Align::Center,
            None,
        );
        item(rep, "clock", &inp.clock, rect_of(mid - w / 2, w));
        (mid - w / 2, mid - w / 2 + w)
    } else {
        (mid, mid)
    };
    // the right side first, so the left knows where it must stop when the clock is off
    let mut x = bar.right() - pad;
    if it.cpu {
        let t = b
            .cpu_percent
            .map_or("CPU --".to_string(), |p| format!("CPU {p}%"));
        let c = if b.cpu_percent.is_some_and(|p| p >= 85) {
            theme::AMBER
        } else {
            theme::TEXT
        };
        let w = o.line(
            x,
            bar.y,
            bar.h,
            &t,
            size,
            FontKind::Mono,
            c,
            Align::Right,
            None,
        );
        item(rep, "cpu", &t, rect_of(x - w, w));
        x -= w + gap_r;
    }
    if it.date {
        let t = date_text(inp.now_ms, inp.tz_min);
        let max = (x - clock_r - gap).max(0);
        let shown = fonts.fit(FontKind::Mono, &t, size, max);
        let w = o.line(
            x,
            bar.y,
            bar.h,
            &shown,
            size,
            FontKind::Mono,
            theme::TEXT,
            Align::Right,
            None,
        );
        item(rep, "date", &shown, rect_of(x - w, w));
        x -= w + gap;
    }
    let right_l = if it.clock { clock_l } else { x + gap };
    let mut lx = bar.x + pad;
    if it.network {
        let d = sz.sc(18.0);
        let r = Rect::new(lx, bar.y + (bar.h - d) / 2, d, d);
        let c = match b.network {
            Some(true) => theme::GREEN,
            Some(false) => theme::RED,
            None => theme::DIM,
        };
        o.rrect(r, d / 2, c);
        item(rep, "network", "", r);
        lx += d + gap;
    }
    if it.product {
        let t = format!("{} {}", b.product.to_uppercase(), b.version)
            .trim()
            .to_string();
        let max = (right_l - gap - lx).max(0);
        let shown = fonts.fit(FontKind::Mono, &t, size, max);
        let w = o.line(
            lx,
            bar.y,
            bar.h,
            &shown,
            size,
            FontKind::Mono,
            theme::DIM,
            Align::Left,
            None,
        );
        item(rep, "product", &shown, rect_of(lx, w));
    }
    o.finish("bar", bar, theme::PANEL)
}

#[allow(clippy::too_many_arguments)]
fn header_single(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    other: Option<&StationModel>,
    rotate: Option<f64>,
    pinned: bool,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(12.0);
    let bg = if st.alarming() {
        theme::RED_BG
    } else {
        theme::PANEL
    };
    let bar_h = if rotate.is_some() {
        sz.sc(3.0).max(2)
    } else {
        0
    };
    let body_h = r.h - bar_h;
    let name_w = o.line(
        r.x + pad,
        r.y,
        body_h,
        st.name(),
        sz.name,
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
    let ph = body_h - sz.sc(8.0);
    pill(
        &mut o,
        r.x + pad + name_w + sz.sc(16.0),
        r.y + (body_h - ph) / 2,
        ph,
        label,
        sz.small,
        pb,
        pf,
        FontKind::Bold,
    );
    let clock_w = o.width(FontKind::Mono, &inp.clock, sz.clock);
    o.line(
        r.right() - pad,
        r.y,
        body_h,
        &inp.clock,
        sz.clock,
        FontKind::Mono,
        theme::TEXT,
        Align::Right,
        None,
    );
    if let Some(ot) = other {
        let name = fonts.fit(FontKind::Bold, ot.name(), sz.small, sz.sc(180.0));
        let w = o.width(FontKind::Bold, &name, sz.small) + 2 * (ph / 4).max(6);
        let x = r.right() - pad - clock_w - sz.sc(20.0) - w;
        let c = alert_color(ot);
        rep.indicators.push((ot.name().to_string(), c));
        o.rrect(Rect::new(x, r.y + (body_h - ph) / 2, w, ph), ph / 3, c);
        o.line(
            x + (ph / 4).max(6),
            r.y,
            body_h,
            &name,
            sz.small,
            FontKind::Bold,
            theme::BG,
            Align::Left,
            None,
        );
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

fn now_single(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(12.0);
    let bar_h = sz.sc(8.0);
    let art = if sz.c800 { sz.sc(160.0) } else { sz.sc(200.0) };
    let art_rect = Rect::new(r.x + pad, r.y + (r.h - bar_h - art) / 2, art, art);
    let text_x = art_rect.right() + if sz.c800 { sz.sc(16.0) } else { sz.sc(20.0) };
    let max_w = r.right() - pad - text_x;
    let now = &inp.now_ms;
    if !st.connected {
        o.line(
            text_x,
            r.y,
            r.h - bar_h,
            "NO CONNECTION",
            sz.title,
            FontKind::Bold,
            theme::RED,
            Align::Left,
            Some(max_w),
        );
        return o.finish("now", r, theme::BG);
    }
    let n = st.now.as_ref();
    art_op(
        &mut o,
        inp,
        art_rect,
        n.and_then(|n| n.art_url.as_deref()),
        600,
    );
    let (title, artist) = n.map_or(("Nothing on air".to_string(), String::new()), |n| {
        (n.title.clone(), n.artist.clone())
    });
    let tc = if n.is_some() { theme::TEXT } else { theme::DIM };
    let lines = [
        lh(sz.title, 1.18),
        lh(sz.artist, 1.22),
        if sz.c800 { lh(sz.small, 1.3) } else { 0 },
        lh(sz.digits, 1.1),
        lh(sz.small, 1.22),
    ];
    let total: i32 = lines.iter().sum();
    let mut y = r.y + (r.h - bar_h - total).max(0) / 2;
    o.line(
        text_x,
        y,
        lines[0],
        &title,
        sz.title,
        FontKind::Bold,
        tc,
        Align::Left,
        Some(max_w),
    );
    y += lines[0];
    o.line(
        text_x,
        y,
        lines[1],
        &artist,
        sz.artist,
        FontKind::Sans,
        theme::TEXT,
        Align::Left,
        Some(max_w),
    );
    y += lines[1];
    if sz.c800 {
        // 800 x 480 keeps the category chip of `single`
        if let Some(n) = n {
            if !n.category.is_empty() {
                let ch = lines[2] - sz.sc(4.0);
                pill(
                    &mut o,
                    text_x,
                    y + (lines[2] - ch) / 2,
                    ch,
                    &n.category.to_uppercase(),
                    sz.small,
                    theme::PANEL2,
                    theme::DIM,
                    FontKind::Sans,
                );
            }
        }
        y += lines[2];
    }
    let rem = st.remaining_ms(*now);
    let digits = rem.map_or("0:00".to_string(), countdown);
    let digits_w = o.line(
        text_x,
        y,
        lines[3],
        &digits,
        sz.digits,
        FontKind::Mono,
        if rem.is_some() {
            theme::TEXT
        } else {
            theme::DIM
        },
        Align::Left,
        None,
    );
    rep.segue.push((st.id.clone(), digits));
    let mini_top = y;
    y += lines[3];
    let mut segue_w = 0;
    if let Some((t, c)) = segue_text(st, *now) {
        segue_w = o.width(FontKind::Bold, &t, sz.small).min(max_w);
        o.line(
            text_x,
            y,
            lines[4],
            &t,
            sz.small,
            FontKind::Bold,
            c,
            Align::Left,
            Some(max_w),
        );
        rep.segue.push((st.id.clone(), t));
    }
    // the mini processing strip: in the free corner right of the time left and the segue line
    let mx = text_x + digits_w.max(segue_w) + sz.sc(28.0);
    let mr = Rect::new(
        mx,
        mini_top + sz.sc(4.0),
        r.right() - pad - mx,
        (r.bottom() - bar_h - sz.sc(6.0)) - (mini_top + sz.sc(4.0)),
    );
    super::proc::mini(&mut o, mr, true, sz.sc(12.0).max(9), inp, st, rep);
    progress_bar(
        &mut o,
        Rect::new(r.x, r.bottom() - bar_h, r.w, bar_h),
        st,
        *now,
    );
    o.finish("now", r, theme::BG)
}

#[allow(clippy::too_many_arguments)]
fn next_zone(
    inp: &SceneInput,
    fonts: &Fonts,
    name: &str,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    rows: usize,
    rep: &mut Report,
    half: bool,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = if half { sz.sc(10.0) } else { sz.sc(12.0) };
    let rh = r.h / rows as i32;
    if !st.connected {
        return o.finish(name, r, theme::BG);
    }
    if st.queue.is_empty() {
        o.line(
            r.x + pad,
            r.y,
            rh,
            "Queue empty",
            sz.small,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
        return o.finish(name, r, theme::BG);
    }
    for (i, e) in st.queue.iter().take(rows).enumerate() {
        let top = r.y + i as i32 * rh;
        let th = (rh - sz.sc(2.0)).min(if sz.c800 { sz.sc(40.0) } else { sz.sc(36.0) });
        let mut x = r.x + pad;
        let right = r.right() - pad;
        let is_block = e.kind == "block";
        // right edge: the countdown of a block, the planned start of a track
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
                top,
                rh,
                &rt,
                sz.small,
                FontKind::Mono,
                rc,
                Align::Right,
                None,
            )
        };
        let mut max_right = right - rw - sz.sc(12.0);
        if is_block {
            let bh = rh - sz.sc(8.0);
            let w = pill(
                &mut o,
                x,
                top + (rh - bh) / 2,
                bh,
                &e.block_type.to_uppercase(),
                sz.small,
                theme::PANEL2,
                theme::AMBER,
                FontKind::Bold,
            );
            x += w + sz.sc(10.0);
        } else if !half {
            let thumb = Rect::new(x, top + (rh - th) / 2, th, th);
            art_op(&mut o, inp, thumb, e.art_url.as_deref(), 144);
            x += th + sz.sc(12.0);
            if sz.c800 {
                let ob = origin_badge(&e.origin);
                if !ob.is_empty() {
                    let w = o.width(FontKind::Bold, &ob, sz.small) + 12;
                    max_right -= w + sz.sc(8.0);
                    let bh = rh - sz.sc(14.0);
                    pill(
                        &mut o,
                        max_right + sz.sc(8.0),
                        top + (rh - bh) / 2,
                        bh,
                        &ob,
                        sz.small,
                        theme::PANEL2,
                        theme::DIM,
                        FontKind::Sans,
                    );
                }
            }
        }
        let text = row_title(e);
        rep.upcoming.push(text.clone());
        o.line(
            x,
            top,
            rh,
            &text,
            sz.small,
            FontKind::Sans,
            theme::TEXT,
            Align::Left,
            Some((max_right - x).max(0)),
        );
    }
    o.finish(name, r, theme::BG)
}

fn meters_single(
    inp: &SceneInput,
    fonts: &Fonts,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(12.0);
    // the loudness controller's bar makes three: each a little thinner
    let lc = lc_reading(st, inp.now_ms);
    let (bar_h, gap) = match (sz.c800, lc.is_some()) {
        (true, false) => (sz.sc(14.0), sz.sc(10.0)),
        (true, true) => (sz.sc(12.0), sz.sc(6.0)),
        (false, false) => (sz.sc(24.0), sz.sc(8.0)),
        (false, true) => (sz.sc(20.0), sz.sc(7.0)),
    };
    // 1024 x 600: the controller's gain takes the target's place (`LC +3.2`); 800 x 480 says it on its second line
    let lc_cells = lc
        .filter(|_| !sz.c800)
        .map(|(g, stt, _, _)| (lc_number(g), lc_color(stt)));
    let text = loudness_line(st, inp.now_ms, sz.c800, true, lc_cells.is_none());
    rep.loudness.push((st.id.clone(), text.clone()));
    let cell_gap = sz.sc(14.0);
    let lc_w = lc_cells.as_ref().map_or(0, |(n, _)| {
        2 * cell_gap + o.width(FontKind::Mono, "LC", sz.loud) + o.width(FontKind::Mono, n, sz.loud)
    });
    // the spec's bars are 460 px (1024 x 600); the text keeps its width and the bars give way when they must
    let text_w = o.width(FontKind::Mono, &text, sz.loud) + lc_w;
    let want = if sz.c800 { sz.sc(340.0) } else { sz.sc(460.0) };
    let bar_w = want
        .min(r.w - 2 * pad - sz.sc(28.0) - text_w - sz.sc(4.0))
        .max(sz.sc(100.0));
    meter_pair(
        inp,
        &mut o,
        rep,
        "meters",
        r,
        st,
        r.x + pad,
        bar_w,
        bar_h,
        gap,
        sz.c800.then_some(sz.small),
        lc,
    );
    let tx = r.x + pad + bar_w + sz.sc(28.0);
    let max_w = r.right() - pad - tx;
    if sz.c800 {
        let ctl = controller_text(st, inp.now_ms);
        let ctl_c = lc.map_or(theme::DIM, |(_, stt, _, _)| lc_color(stt));
        let (l1, l2) = (lh(sz.loud, 1.3), lh(sz.small, 1.3));
        let top = r.y + (r.h - l1 - l2) / 2;
        o.line(
            tx,
            top,
            l1,
            &text,
            sz.loud,
            FontKind::Mono,
            theme::TEXT,
            Align::Left,
            Some(max_w),
        );
        o.line(
            tx,
            top + l1,
            l2,
            &ctl,
            sz.small,
            FontKind::Sans,
            ctl_c,
            Align::Left,
            Some(max_w),
        );
    } else {
        let w = o.line(
            tx,
            r.y,
            r.h,
            &text,
            sz.loud,
            FontKind::Mono,
            theme::TEXT,
            Align::Left,
            Some(max_w),
        );
        if let Some((n, c)) = &lc_cells {
            let lx = tx + w + cell_gap;
            let lw = o.line(
                lx,
                r.y,
                r.h,
                "LC",
                sz.loud,
                FontKind::Mono,
                theme::DIM,
                Align::Left,
                None,
            );
            o.line(
                lx + lw + cell_gap,
                r.y,
                r.h,
                n,
                sz.loud,
                FontKind::Mono,
                *c,
                Align::Left,
                None,
            );
        }
    }
    o.finish("meters", r, theme::BG)
}

/// The two PPM bars with their peak-hold ticks, vertically centered in `r`.
#[allow(clippy::too_many_arguments)]
fn meter_pair(
    inp: &SceneInput,
    o: &mut Ops,
    rep: &mut Report,
    zone: &str,
    r: Rect,
    st: &StationModel,
    x: i32,
    w: i32,
    bar_h: i32,
    gap: i32,
    label_size: Option<i32>,
    lc: Option<(f64, u8, f64, f64)>,
) {
    let f = st.live_frame(inp.now_ms);
    let lab = label_size.map_or(0, |sz| lh(sz, 1.2));
    let total = 2 * bar_h + gap + lab + if lc.is_some() { bar_h + gap } else { 0 };
    let top = r.y + (r.h - total) / 2;
    for i in 0..2 {
        let track = Rect::new(x, top + i as i32 * (bar_h + gap), w, bar_h);
        let hold = st.dynamics.peak_hold[i].map(|h| h.0);
        ppm_bar(
            o,
            rep,
            zone,
            track,
            st.dynamics.level(i, f),
            hold,
            &[-20.0, 0.0],
        );
    }
    if let Some(size) = label_size {
        // the 800 x 480 fallback labels the scale only at -20 and 0 (11.2b)
        let y = top + 2 * bar_h + gap;
        let t20 = Rect::new(x, 0, w, 0);
        o.line(
            db_x(t20, -20.0),
            y,
            lab,
            "-20",
            size,
            FontKind::Sans,
            theme::DIM,
            Align::Center,
            None,
        );
        o.line(
            x + w,
            y,
            lab,
            "0",
            size,
            FontKind::Sans,
            theme::DIM,
            Align::Right,
            None,
        );
    }
    // the loudness controller's bar under the level bars (and under the 800 x 480 scale labels)
    if let Some(reading) = lc {
        let y = top + 2 * bar_h + 2 * gap + lab;
        lc_bar(o, rep, zone, Rect::new(x, y, w, bar_h), reading);
    }
}

fn alarm_single(inp: &SceneInput, fonts: &Fonts, r: Rect, sz: &Sz, idx: usize) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(12.0);
    let lines = alarm_lines(inp, idx, true);
    if sz.c800 {
        // the newest open alarm and `+N more`
        let lh_ = r.h;
        if let Some((c, t)) = lines.first() {
            let more = lines.len().saturating_sub(1);
            let tail = if more > 0 {
                format!("+{more} more")
            } else {
                String::new()
            };
            let tw = if tail.is_empty() {
                0
            } else {
                o.width(FontKind::Sans, &tail, sz.small) + sz.sc(12.0)
            };
            o.line(
                r.x + pad,
                r.y,
                lh_,
                t,
                sz.small,
                FontKind::Sans,
                *c,
                Align::Left,
                Some(r.w - 2 * pad - tw),
            );
            if !tail.is_empty() {
                o.line(
                    r.right() - pad,
                    r.y,
                    lh_,
                    &tail,
                    sz.small,
                    FontKind::Sans,
                    theme::DIM,
                    Align::Right,
                    None,
                );
            }
        } else {
            o.line(
                r.x + pad,
                r.y,
                lh_,
                "NO ALARMS PENDING",
                sz.small,
                FontKind::Sans,
                theme::DIM,
                Align::Left,
                None,
            );
        }
    } else {
        let line_h = sz.sc(44.0);
        let top = r.y + (r.h - 2 * line_h) / 2;
        if lines.is_empty() {
            o.line(
                r.x + pad,
                top,
                line_h,
                "NO ALARMS PENDING",
                sz.small,
                FontKind::Sans,
                theme::DIM,
                Align::Left,
                None,
            );
        }
        for (i, (c, t)) in lines.iter().take(2).enumerate() {
            o.line(
                r.x + pad,
                top + i as i32 * line_h,
                line_h,
                t,
                sz.small,
                FontKind::Sans,
                *c,
                Align::Left,
                Some(r.w - 2 * pad),
            );
        }
    }
    o.finish("alarm", r, theme::BG)
}

// ---- split halves ----------------------------------------------------------------------------------------------------

fn header_half(fonts: &Fonts, name: &str, r: Rect, sz: &Sz, st: &StationModel) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(10.0);
    let bg = if st.alarming() {
        theme::RED_BG
    } else {
        theme::PANEL
    };
    let label = if st.connected {
        st.pill()
    } else {
        "NO CONNECTION"
    };
    let (pb, pf) = pill_colors(label);
    let ph = r.h - sz.sc(8.0);
    let pw = o.width(FontKind::Bold, label, sz.small) + 2 * (ph / 4).max(6);
    let name_w = o.line(
        r.x + pad,
        r.y,
        r.h,
        st.name(),
        sz.name,
        FontKind::Bold,
        theme::TEXT,
        Align::Left,
        Some(r.w - 2 * pad - pw - sz.sc(12.0)),
    );
    let _ = name_w;
    pill(
        &mut o,
        r.right() - pad - pw,
        r.y + (r.h - ph) / 2,
        ph,
        label,
        sz.small,
        pb,
        pf,
        FontKind::Bold,
    );
    o.finish(name, r, bg)
}

fn now_half(
    inp: &SceneInput,
    fonts: &Fonts,
    name: &str,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(10.0);
    if !st.connected {
        return o.finish(name, r, theme::BG);
    }
    let bar_h = sz.sc(6.0);
    let art = sz.sc(96.0);
    let n = st.now.as_ref();
    let art_rect = Rect::new(r.x + pad, r.y + sz.sc(8.0), art, art);
    art_op(
        &mut o,
        inp,
        art_rect,
        n.and_then(|n| n.art_url.as_deref()),
        600,
    );
    let tx = art_rect.right() + sz.sc(12.0);
    let max_w = r.right() - pad - tx;
    let (title, artist) = n.map_or(("Nothing on air".to_string(), String::new()), |n| {
        (n.title.clone(), n.artist.clone())
    });
    let tl = lh(sz.title, 1.2);
    let mut y = art_rect.y;
    let lines = fonts.wrap(FontKind::Bold, &title, sz.title, max_w, 2);
    for l in &lines {
        o.line(
            tx,
            y,
            tl,
            l,
            sz.title,
            FontKind::Bold,
            if n.is_some() { theme::TEXT } else { theme::DIM },
            Align::Left,
            Some(max_w),
        );
        y += tl;
    }
    let y = art_rect.y + 2 * tl;
    o.line(
        tx,
        y,
        lh(sz.artist, 1.22),
        &artist,
        sz.artist,
        FontKind::Sans,
        theme::TEXT,
        Align::Left,
        Some(max_w),
    );
    // remaining time and the segue line side by side below the artwork row
    let row_top = art_rect.bottom() + sz.sc(10.0);
    let row_h = lh(sz.digits, 1.1);
    let rem = st.remaining_ms(inp.now_ms);
    let digits = rem.map_or("0:00".to_string(), countdown);
    let dw = o.line(
        r.x + pad,
        row_top,
        row_h,
        &digits,
        sz.digits,
        FontKind::Mono,
        if rem.is_some() {
            theme::TEXT
        } else {
            theme::DIM
        },
        Align::Left,
        None,
    );
    rep.segue.push((st.id.clone(), digits));
    if let Some((t, c)) = segue_text(st, inp.now_ms) {
        let sx = r.x + pad + dw + sz.sc(14.0);
        o.line(
            sx,
            row_top,
            row_h,
            &t,
            sz.small,
            FontKind::Bold,
            c,
            Align::Left,
            Some(r.right() - pad - sx),
        );
        rep.segue.push((st.id.clone(), t));
    }
    progress_bar(
        &mut o,
        Rect::new(r.x, r.bottom() - bar_h, r.w, bar_h),
        st,
        inp.now_ms,
    );
    o.finish(name, r, theme::BG)
}

fn meters_half(
    inp: &SceneInput,
    fonts: &Fonts,
    name: &str,
    r: Rect,
    sz: &Sz,
    st: &StationModel,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    if !st.connected {
        return o.finish(name, r, theme::BG);
    }
    let pad = sz.sc(10.0);
    let lc = lc_reading(st, inp.now_ms);
    let (bar_h, gap) = match (sz.c800, lc.is_some()) {
        (true, false) => (sz.sc(12.0), sz.sc(8.0)),
        (true, true) => (sz.sc(10.0), sz.sc(5.0)),
        (false, false) => (sz.sc(20.0), sz.sc(8.0)),
        (false, true) => (sz.sc(16.0), sz.sc(5.0)),
    };
    // split drops I; the 800 layout keeps S. A half has room for the controller's gain only in the target's place,
    // without its label, in the colour of its bar.
    let lc_cell = lc.map(|(g, stt, _, _)| (lc_number(g), lc_color(stt)));
    let text = loudness_line(st, inp.now_ms, sz.c800, false, lc_cell.is_none());
    let cell_gap = sz.sc(14.0);
    let lc_w = lc_cell
        .as_ref()
        .map_or(0, |(n, _)| cell_gap + o.width(FontKind::Mono, n, sz.loud));
    // the spec's bars are 260 px; the text at the minimum size needs its width, so the bars give way when they must
    let text_w = o.width(FontKind::Mono, &text, sz.loud) + lc_w;
    let want = if sz.c800 { sz.sc(170.0) } else { sz.sc(260.0) };
    let bar_w = want
        .min(r.w - 2 * pad - sz.sc(16.0) - text_w - sz.sc(4.0))
        .max(sz.sc(60.0));
    meter_pair(
        inp,
        &mut o,
        rep,
        name,
        r,
        st,
        r.x + pad,
        bar_w,
        bar_h,
        gap,
        None,
        lc,
    );
    let tx = r.x + pad + bar_w + sz.sc(16.0);
    rep.loudness.push((st.id.clone(), text.clone()));
    let w = o.line(
        tx,
        r.y,
        r.h,
        &text,
        sz.loud,
        FontKind::Mono,
        theme::TEXT,
        Align::Left,
        Some(r.right() - pad - tx),
    );
    if let Some((n, c)) = &lc_cell {
        o.line(
            tx + w + cell_gap,
            r.y,
            r.h,
            n,
            sz.loud,
            FontKind::Mono,
            *c,
            Align::Left,
            None,
        );
    }
    o.finish(name, r, theme::BG)
}

fn alarms_half(
    inp: &SceneInput,
    fonts: &Fonts,
    name: &str,
    r: Rect,
    sz: &Sz,
    idx: usize,
    rep: &mut Report,
) -> ZoneOps {
    let mut o = Ops::new(fonts);
    let pad = sz.sc(10.0);
    let lines = alarm_lines(inp, idx, false);
    let lh_ = lh(sz.small, 1.22);
    let mut y = r.y + sz.sc(6.0);
    if lines.is_empty() {
        o.line(
            r.x + pad,
            y,
            lh_,
            "NO ALARMS PENDING",
            sz.small,
            FontKind::Sans,
            theme::DIM,
            Align::Left,
            None,
        );
        y += lh_;
    }
    for (c, t) in lines.iter().take(2) {
        for l in fonts.wrap(FontKind::Sans, t, sz.small, r.w - 2 * pad, 2) {
            if y + lh_ > r.bottom() {
                break;
            }
            o.line(
                r.x + pad,
                y,
                lh_,
                &l,
                sz.small,
                FontKind::Sans,
                *c,
                Align::Left,
                Some(r.w - 2 * pad),
            );
            y += lh_;
        }
    }
    // the mini processing strip in what the alarm text leaves of the zone (all of the lower part with none)
    let mr = Rect::new(
        r.x + pad,
        y + sz.sc(6.0),
        r.w - 2 * pad,
        r.bottom() - y - sz.sc(10.0),
    );
    super::proc::mini(
        &mut o,
        mr,
        false,
        sz.sc(12.0).max(9),
        inp,
        &inp.stations[idx],
        rep,
    );
    o.finish(name, r, theme::BG)
}
