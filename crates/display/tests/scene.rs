//! Acceptance 7, 8, 10, 11 and 12 of M7 (milestones.md): layouts, glyph minimums, what drops, meters against
//! the frame, the processing view's curve against the reference model, the stage meters. They run on the
//! offscreen renderer, so no panel is needed; the checks read the renderer's log and the drawn pixels.

use common::config::DisplayConfig;
use display::canvas::{theme, Color, FontKind};
use display::feed::{demo, Feed};
use display::headless::{render, Options};
use display::layout::{layout, Kind, Plan, Rect, Variant, FULL_ZONES};
use display::render::Renderer;
use display::scene::{db_x, Shown};
use display::view::ViewMode;
use serde_json::{json, Value};

fn feed_from(v: &Value) -> Feed {
    Feed::from_json(v, std::path::Path::new(".")).expect("feed")
}

fn opts(w: i32, h: i32, diag: f64, view: ViewMode) -> Options {
    let cfg = DisplayConfig {
        diagonal_in: diag,
        ..DisplayConfig::default()
    };
    Options {
        w,
        h,
        cfg,
        view: Some(view),
        station: None,
        product: "Product".into(),
    }
}

fn draw(feed: &Value, o: &Options) -> Renderer {
    render(&feed_from(feed), o).0
}

fn px(r: &Renderer, x: i32, y: i32) -> [u8; 4] {
    let i = (y as usize * r.plan.w as usize + x as usize) * 4;
    let d = r.canvas.rgba();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

fn rgb(c: Color) -> [u8; 4] {
    [c.0, c.1, c.2, 255]
}

fn zones(r: &Renderer) -> Vec<(String, Rect)> {
    r.layout
        .as_ref()
        .unwrap()
        .zones
        .iter()
        .map(|z| (z.name.clone(), z.rect))
        .collect()
}

const CONFIGS: [(i32, i32, f64); 5] = [
    (1024, 600, 7.0),
    (800, 480, 9.0),
    (1280, 720, 21.5),
    (1920, 1080, 21.5),
    (1920, 1080, 15.6),
];
const FEEDS: [&str; 7] = [
    "both",
    "chain5",
    "chain3",
    "alert",
    "untagged",
    "longtitle",
    "disconnected",
];

// ---- 7: layouts -----------------------------------------------------------------------------------------------------

#[test]
fn every_zone_is_at_its_rectangle_and_the_rectangles_tile_the_screen() {
    for (w, h, d) in CONFIGS {
        for view in [ViewMode::Single, ViewMode::Split, ViewMode::Rotate] {
            let r = draw(&demo("both").unwrap(), &opts(w, h, d, view));
            let zs = zones(&r);
            let plan = r.plan;
            // against the independent computation of the spec's tables
            let kind = if view == ViewMode::Split {
                Kind::Split
            } else {
                Kind::Single
            };
            let want = layout(&plan, kind);
            for (n, rect) in &zs {
                assert_eq!(*rect, want.zone(n), "{w}x{h} {view:?} {n}");
            }
            if plan.variant == Variant::Full && kind == Kind::Single {
                for &(n, x, y, zw, zh) in &FULL_ZONES {
                    let z = zs
                        .iter()
                        .find(|z| z.0 == n)
                        .unwrap_or_else(|| panic!("zone {n}"))
                        .1;
                    let near = |a: i32, frac: f64, tot: i32| {
                        (f64::from(a) - frac / 100.0 * f64::from(tot)).abs() <= 1.0
                    };
                    assert!(
                        near(z.x, x, w) && near(z.y, y, h) && near(z.w, zw, w) && near(z.h, zh, h),
                        "{w}x{h} {n} {z:?}"
                    );
                }
            }
            // an exact tiling: the zones and the divider cover every pixel once
            let mut cover = vec![0u8; (w * h) as usize];
            let divider = r.layout.as_ref().unwrap().divider;
            for z in zs.iter().map(|z| z.1).chain(divider) {
                for y in z.y..z.bottom() {
                    for x in z.x..z.right() {
                        cover[(y * w + x) as usize] += 1;
                    }
                }
            }
            assert!(
                cover.iter().all(|&c| c == 1),
                "{w}x{h} {view:?}: zones overlap or leave a gap"
            );
            // the heights of a column sum to the screen
            let col_x = 3;
            let sum: i32 = zs
                .iter()
                .filter(|z| z.1.x <= col_x && col_x < z.1.right())
                .map(|z| z.1.h)
                .sum();
            assert_eq!(sum, h, "{w}x{h} {view:?}");
        }
    }
}

#[test]
fn no_text_is_drawn_below_the_minimum_and_every_glyph_run_stays_in_its_zone() {
    for (w, h, d) in CONFIGS {
        for view in [ViewMode::Single, ViewMode::Split] {
            for name in FEEDS {
                let r = draw(&demo(name).unwrap(), &opts(w, h, d, view));
                let floor = r.plan.floor_px;
                assert!(!r.canvas.log.glyphs.is_empty());
                let zs = zones(&r);
                for g in &r.canvas.log.glyphs {
                    assert!(
                        g.size >= floor
                            || display::scene::proc::MINI_LABELS.contains(&g.text.as_str()),
                        "{w}x{h} {view:?} {name}: `{}` at {} px < {floor}",
                        g.text,
                        g.size
                    );
                    let z = zs.iter().find(|z| z.0 == g.zone).unwrap().1;
                    assert!(
                        z.contains(&g.bbox) || g.bbox.w == 0,
                        "{w}x{h} {view:?} {name}: `{}` {:?} outside {} {:?}",
                        g.text,
                        g.bbox,
                        g.zone,
                        z
                    );
                }
            }
        }
    }
    // the primary: the minimum is 31 px (cap height 22) and the smallest text drawn is exactly that
    let r = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    assert_eq!(r.plan.min_px, 31);
    assert_eq!(r.canvas.log.glyphs.iter().map(|g| g.size).min(), Some(31));
    // the fallback draws 19 px
    let r = draw(
        &demo("both").unwrap(),
        &opts(800, 480, 9.0, ViewMode::Single),
    );
    assert_eq!(
        (
            r.plan.min_px,
            r.canvas.log.glyphs.iter().map(|g| g.size).min()
        ),
        (19, Some(19))
    );
}

/// The ink height of a glyph run measured on the drawn picture: the rows, between the zone's top and the baseline
/// plus a descender, in the run's columns where any pixel differs from the zone's background.
fn measured_height(r: &Renderer, g: &display::canvas::GlyphRun, bg: Color) -> i32 {
    let (x0, x1) = (g.bbox.x.max(0), g.bbox.right().min(r.plan.w));
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for y in (g.baseline - g.size).max(0)..(g.baseline + g.size / 3).min(r.plan.h) {
        if (x0..x1).any(|x| px(r, x, y) != rgb(bg)) {
            lo = lo.min(y);
            hi = hi.max(y);
        }
    }
    if hi < lo {
        0
    } else {
        hi - lo + 1
    }
}

#[test]
fn the_primary_screenshot_measures_22_px_caps_for_the_digits_title_and_artist() {
    let r = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    let find = |t: &str| {
        r.canvas
            .log
            .glyphs
            .iter()
            .find(|g| g.text.starts_with(t))
            .unwrap_or_else(|| panic!("no run `{t}`"))
            .clone()
    };
    // the remaining-time digits (88 px), the title (44 px) and the artist (31 px)
    for (g, min_h) in [
        (find("0:38"), 61),
        (find("But I Do"), 31),
        (find("Demo Artists"), 22),
    ] {
        let hgt = measured_height(&r, &g, theme::BG);
        assert!(
            hgt >= min_h,
            "`{}` ({} px): measured {hgt} rows, wanted at least {min_h}",
            g.text,
            g.size
        );
    }
}

#[test]
fn what_each_size_drops_is_absent_and_nothing_else() {
    let texts = |r: &Renderer| {
        r.canvas
            .log
            .glyphs
            .iter()
            .map(|g| g.text.clone())
            .collect::<Vec<_>>()
    };
    let has = |v: &[String], s: &str| v.iter().any(|t| t.contains(s));
    let both = demo("both").unwrap();
    // 1024 x 600 single: no album line, no elapsed, no category chip, no as-run, no product name, no S, two queue rows
    let r = draw(&both, &opts(1024, 600, 7.0, ViewMode::Single));
    let t = texts(&r);
    for absent in [
        "Drift, Volume",
        "ELAPSED",
        "4:11",
        "MUSIC",
        "Spectrotemporal",
        "From Silence (RC1)  ",
        "Product",
        "S -14",
        "NO ALARMS PENDING",
    ] {
        assert!(
            !has(&t, absent),
            "1024 single should not draw `{absent}`: {t:?}"
        );
    }
    for present in [
        "Office",
        "PLAYING",
        "16:21:14",
        "Eve",
        "But I Do",
        "Demo Artists",
        "0:38",
        "SEGUE 0:23",
        "January Man",
        "Top of hour ID",
        "0:41",
        "W -14.1  I -14.0 / -14",
        "restarted",
    ] {
        assert!(
            has(&t, present),
            "1024 single should draw `{present}`: {t:?}"
        );
    }
    assert_eq!(r.report.upcoming.len(), 2);
    assert!(
        !r.layout.as_ref().unwrap().has("stages")
            && !r.layout.as_ref().unwrap().has("processing")
            && !r.layout.as_ref().unwrap().has("asrun")
    );
    // 1024 split: also no I, one queue row, a 96 px artwork, the other station is the whole second half
    let r = draw(&both, &opts(1024, 600, 7.0, ViewMode::Split));
    let t = texts(&r);
    assert!(!has(&t, "I -14") && !has(&t, "S -14"));
    assert!(has(&t, "W -14.1 / -14"));
    assert_eq!(r.report.upcoming.len(), 2, "one row in each half");
    assert!(r
        .canvas
        .log
        .art
        .iter()
        .filter(|a| a.zone.ends_with(".now"))
        .all(|a| a.rect.w == 96 && a.rect.h == 96));
    assert!(has(&t, "Office") && has(&t, "Eve"));
    // 800 x 480 single keeps the category chip, S and the controller's gain; no I-less line
    let r = draw(&both, &opts(800, 480, 9.0, ViewMode::Single));
    let t = texts(&r);
    assert!(
        has(&t, "MUSIC")
            && has(&t, "S -14.2  W -14.1  I -14.0 / -14")
            && has(&t, "ON TARGET")
            && has(&t, "-20")
    );
    assert!(!has(&t, "Drift, Volume") && !has(&t, "ELAPSED") && !has(&t, "Product"));
    // 800 split: S and W, no I, no scale labels
    let r = draw(&both, &opts(800, 480, 9.0, ViewMode::Split));
    let t = texts(&r);
    assert!(has(&t, "S -14.2  W -14.1 / -14") && !has(&t, "I -14") && !has(&t, "-20"));
    // the full layout has everything the compact one drops
    let r = draw(&both, &opts(1920, 1080, 21.5, ViewMode::Single));
    let t = texts(&r);
    for present in [
        "Drift, Volume 1",
        "ELAPSED",
        "4:11",
        "MUSIC",
        "Product",
        "From Silence (RC1)",
        "TARGET -14",
        "NO CONNECTION",
    ]
    .iter()
    .take(7)
    {
        assert!(has(&t, present), "full should draw `{present}`");
    }
    assert_eq!(
        r.report.upcoming.len(),
        1 + 5,
        "the card and the rows of the queue"
    );
}

#[test]
fn artwork_for_a_tagged_item_and_the_placeholder_for_an_untagged_one() {
    let r = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    let now_art: Vec<_> = r
        .canvas
        .log
        .art
        .iter()
        .filter(|a| a.zone == "now")
        .collect();
    assert_eq!(now_art.len(), 1);
    assert_eq!(
        (now_art[0].kind, now_art[0].rect.w, now_art[0].rect.h),
        ("image", 200, 200)
    );
    let r = draw(
        &demo("untagged").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    let now_art: Vec<_> = r
        .canvas
        .log
        .art
        .iter()
        .filter(|a| a.zone == "now")
        .collect();
    assert_eq!(now_art.len(), 1);
    assert_eq!(now_art[0].kind, "placeholder");
    // an image that has not arrived yet draws the placeholder too, and the zone redraws when it arrives
    let mut f = feed_from(&demo("both").unwrap());
    f.art.clear();
    let (r, _) = render(&f, &opts(1024, 600, 7.0, ViewMode::Single));
    assert!(r
        .canvas
        .log
        .art
        .iter()
        .filter(|a| a.zone == "now")
        .all(|a| a.kind == "placeholder"));
}

fn title_runs<'a>(r: &'a Renderer, zone: &str, needle: &str) -> Vec<&'a display::canvas::GlyphRun> {
    r.canvas
        .log
        .glyphs
        .iter()
        .filter(|g| g.zone == zone && g.size >= 31 && (g.text.contains(needle)))
        .collect()
}

#[test]
fn a_60_character_title_ellipsizes_in_both_halves_of_split_without_overlap() {
    let long: String = "Symphony of the Unquiet Hours in Seven Movements, Op12"
        .chars()
        .cycle()
        .take(60)
        .collect();
    assert_eq!(long.chars().count(), 60);
    let mut v = demo("both").unwrap();
    for i in 0..2 {
        v["stations"][i]["snapshot"]["now"]["title"] = json!(long);
    }
    for (w, h, d) in [(1024, 600, 7.0), (800, 480, 9.0)] {
        let r = draw(&v, &opts(w, h, d, ViewMode::Split));
        for side in ["L", "R"] {
            let zone = format!("{side}.now");
            let z = zones(&r).into_iter().find(|z| z.0 == zone).unwrap().1;
            let runs: Vec<_> = r
                .canvas
                .log
                .glyphs
                .iter()
                .filter(|g| g.zone == zone)
                .collect();
            let art = r
                .canvas
                .log
                .art
                .iter()
                .find(|a| a.zone == zone)
                .unwrap()
                .rect;
            // the title's lines are the bold runs above the digits
            let title: Vec<_> = runs
                .iter()
                .filter(|g| g.font == FontKind::Bold && g.baseline <= art.bottom())
                .collect();
            assert!(!title.is_empty());
            let joined: String = title
                .iter()
                .map(|g| g.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                joined.ends_with('\u{2026}'),
                "{w}x{h} {side}: `{joined}` should end in an ellipsis"
            );
            assert!(title.len() <= 2, "two lines at most");
            for g in &runs {
                assert!(z.contains(&g.bbox), "{} {:?}", g.text, g.bbox);
                if g.text
                    != runs
                        .iter()
                        .find(|x| x.text.contains(':'))
                        .map_or("", |x| x.text.as_str())
                {
                    // text beside the artwork never touches it
                    if g.baseline < art.bottom() + 2 {
                        assert!(!g.bbox.intersects(&art), "{} overlaps the artwork", g.text);
                    }
                }
            }
            // no two runs overlap
            for (i, a) in runs.iter().enumerate() {
                for b in &runs[i + 1..] {
                    assert!(
                        !a.bbox.intersects(&b.bbox),
                        "{w}x{h} {side}: `{}` overlaps `{}`",
                        a.text,
                        b.text
                    );
                }
            }
        }
    }
    // single too: one line, ellipsized
    let r = draw(&v, &opts(1024, 600, 7.0, ViewMode::Single));
    let t = title_runs(&r, "now", "Symphony");
    assert_eq!(t.len(), 1);
    assert!(t[0].text.ends_with('\u{2026}'));
}

// ---- 8: the frame on the screen --------------------------------------------------------------------------------------

fn tone_feed(db: f64) -> Value {
    let mut v = demo("both").unwrap();
    v["stations"][0]["frame"] = json!({"ppm": [db, db], "peak": [db, db], "momentary": db, "short_term": db, "integrated": -19.95, "window": db, "target": -20.0,
        "gain_db": 0.0, "loudness_state": 2, "bypassed": true, "post_tone": {"hz": 1000.0, "db": db}, "floor": -85.0});
    v
}

#[test]
fn a_1k_tone_at_minus_20_lights_the_bars_at_minus_20_and_the_cap_sits_in_the_1k_band() {
    for (w, h, d, zone) in [
        (1024, 600, 7.0, "meters"),
        (1920, 1080, 21.5, "meters"),
        (1280, 720, 21.5, "meters"),
    ] {
        let r = draw(&tone_feed(-20.0), &opts(w, h, d, ViewMode::Single));
        let bars: Vec<_> = r.report.bars.iter().filter(|b| b.zone == zone).collect();
        assert_eq!(bars.len(), 2);
        for b in bars {
            let want = db_x(b.track, -20.0);
            assert!(
                (b.fill_x - want).abs() <= 1,
                "{w}: fill {} vs {want}",
                b.fill_x
            );
            // on the picture: the last lit column of the bar (a tick or the hold tick may sit beyond it)
            let mid = b.track.y + b.track.h / 2;
            let lit = |x: i32| px(&r, x, mid) != rgb(theme::BG);
            let at = (b.track.x..b.track.right())
                .filter(|&x| px(&r, x, mid) == rgb(theme::GREEN))
                .max()
                .unwrap();
            assert!(
                lit(at) && (at + 1 - want).abs() <= 1,
                "{w}: the green ends at column {at}, the scale puts -20 at {want}"
            );
            // one dB is this many pixels; the check is within a pixel step of the scale
            assert!(b.px_per_db * 1.0 >= 1.0 || w < 1500);
        }
    }
    // the spectrum: in the full layout the 1 kHz band holds the highest bar, post and pre
    let mut v = tone_feed(-20.0);
    v["stations"][0]["frame"]["pre_tone"] = json!({"hz": 1000.0, "db": -20.0});
    let mut o = opts(1920, 1080, 21.5, ViewMode::Single);
    o.cfg.spectrum_source = "both".into();
    let r = draw(&v, &o);
    let band = (64.0 * (1000.0f64 / 20.0).log10() / 3.0).floor() as usize;
    for bars in [&r.report.spectrum_post, &r.report.spectrum_pre] {
        let top = bars.iter().min_by_key(|b| b.top).unwrap();
        assert_eq!(top.band, band);
        let cap = bars.iter().min_by_key(|b| b.cap).unwrap();
        assert_eq!(cap.band, band, "the cap sits in the 1 kHz band");
        assert!(bars.len() == 64);
    }
    let plot = r.report.plot.unwrap();
    let db_of = |y: i32| -60.0 + f64::from(plot.bottom() - y) / f64::from(plot.h) * 60.0;
    let top = r.report.spectrum_post.iter().min_by_key(|b| b.top).unwrap();
    assert!((db_of(top.top) + 20.0).abs() < 0.5, "{}", db_of(top.top));
    // 1280 x 720 draws 32 bands, and the 1 kHz band is still the highest
    let r = draw(&v, &opts(1280, 720, 21.5, ViewMode::Single));
    assert_eq!(r.report.spectrum_bands, 32);
    assert_eq!(
        r.report
            .spectrum_post
            .iter()
            .min_by_key(|b| b.top)
            .unwrap()
            .band,
        band / 2
    );
}

#[test]
fn the_loudness_numerals_match_the_frame_to_a_tenth_of_an_lu() {
    let mut v = demo("both").unwrap();
    v["stations"][0]["frame"].as_object_mut().unwrap().extend(
        [
            ("momentary", json!(-13.84)),
            ("short_term", json!(-14.26)),
            ("window", json!(-14.07)),
            ("integrated", json!(-13.96)),
            ("target", json!(-14.0)),
        ]
        .map(|(k, v)| (k.to_string(), v)),
    );
    let r = draw(&v, &opts(1920, 1080, 21.5, ViewMode::Single));
    let get = |k: &str| {
        r.report
            .loudness
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    // the frame carries 0.01 dB; the numerals are the same numbers to 0.1 LU
    for (k, want) in [("M", -13.84), ("S", -14.26), ("W", -14.07), ("I", -13.96)] {
        let shown: f64 = get(k).parse().unwrap();
        assert!(
            (shown - want).abs() <= 0.05 + 1e-9,
            "{k}: {shown} vs {want}"
        );
    }
    let t: Vec<_> = r.canvas.log.glyphs.iter().map(|g| g.text.clone()).collect();
    assert!(t.iter().any(|x| x.starts_with("TARGET -14")));
    // with the controller off the target marker and its text go, W and I stay
    v["stations"][0]["frame"]["target"] = Value::Null;
    v["stations"][0]["frame"]["loudness_state"] = json!(0);
    let r = draw(&v, &opts(1920, 1080, 21.5, ViewMode::Single));
    let t: Vec<_> = r.canvas.log.glyphs.iter().map(|g| g.text.clone()).collect();
    assert!(t.iter().any(|x| x == "CONTROLLER OFF") && !t.iter().any(|x| x.starts_with("TARGET")));
    assert!(r
        .report
        .loudness
        .iter()
        .any(|(k, v)| k == "W" && v == "-14.1"));
}

#[test]
fn a_gapless_run_shows_album_and_no_segue_countdown() {
    let v = demo("gapless").unwrap();
    for (w, h, d) in [(1024, 600, 7.0), (800, 480, 9.0), (1920, 1080, 21.5)] {
        let r = draw(&v, &opts(w, h, d, ViewMode::Single));
        let t: Vec<_> = r.canvas.log.glyphs.iter().map(|g| g.text.clone()).collect();
        assert!(t.iter().any(|x| x == "ALBUM, ENDS 0:38"), "{w}: {t:?}");
        assert!(!t.iter().any(|x| x.starts_with("SEGUE")));
    }
    // and the progress bar has no segue tick: the amber tick pixel is absent
    let r = draw(&v, &opts(1024, 600, 7.0, ViewMode::Single));
    let bar = zones(&r).into_iter().find(|z| z.0 == "now").unwrap().1;
    let y = bar.bottom() - 4;
    assert!((0..1024).all(|x| px(&r, x, y) != rgb(theme::AMBER)));
}

#[test]
fn upcoming_blocks_show_their_badge_and_a_countdown_amber_under_60_s_red_under_10() {
    for (secs, color) in [(90, theme::TEXT), (41, theme::AMBER), (8, theme::RED)] {
        let mut v = demo("both").unwrap();
        v["stations"][0]["snapshot"]["queue"][1]["in_ms"] = json!(secs * 1000);
        for (w, h, d) in [(1024, 600, 7.0), (1920, 1080, 21.5)] {
            let r = draw(&v, &opts(w, h, d, ViewMode::Single));
            let want = format!("{}:{:02}", secs / 60, secs % 60);
            let run = r
                .canvas
                .log
                .glyphs
                .iter()
                .find(|g| g.text == want && g.font == FontKind::Mono)
                .unwrap_or_else(|| panic!("{w}: no countdown {want}"));
            assert_eq!(run.color, color, "{w}: {secs} s");
            assert!(
                r.canvas.log.glyphs.iter().any(|g| g.text == "ID"),
                "the badge"
            );
        }
    }
}

#[test]
fn a_critical_alert_turns_the_header_red_and_the_other_stations_indicator_red() {
    // on the station itself
    let mut v = demo("both").unwrap();
    v["stations"][0]["snapshot"]["alerts"] = json!([{"id": 9, "code": "silence", "severity": "critical", "message": "m", "raised_at": "2026-10-06T20:21:00.000Z"}]);
    let r = draw(&v, &opts(1024, 600, 7.0, ViewMode::Single));
    let hd = zones(&r).into_iter().find(|z| z.0 == "header").unwrap().1;
    assert_eq!(px(&r, hd.x + 300, hd.y + 3), rgb(theme::RED_BG));
    let calm = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    assert_eq!(px(&calm, hd.x + 300, hd.y + 3), rgb(theme::PANEL));
    // on the other station: its indicator in the header turns red, the header itself does not
    let r = draw(
        &demo("alert").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    assert_eq!(px(&r, hd.x + 300, hd.y + 3), rgb(theme::PANEL));
    let ind = r
        .canvas
        .log
        .glyphs
        .iter()
        .find(|g| g.zone == "header" && g.text == "Eve")
        .unwrap();
    assert_eq!(
        px(&r, ind.bbox.x - 3, ind.bbox.y + ind.bbox.h / 2),
        rgb(theme::RED)
    );
    // an unreachable station is red as well, and says so
    let r = draw(
        &demo("disconnected").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Single),
    );
    let ind = r
        .canvas
        .log
        .glyphs
        .iter()
        .find(|g| g.zone == "header" && g.text == "Eve")
        .unwrap();
    assert_eq!(
        px(&r, ind.bbox.x - 3, ind.bbox.y + ind.bbox.h / 2),
        rgb(theme::RED)
    );
    assert!(r
        .canvas
        .log
        .glyphs
        .iter()
        .any(|g| g.zone == "alarm" && g.text == "Eve: NO CONNECTION"));
    let r = draw(
        &demo("disconnected").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Split),
    );
    assert!(r
        .canvas
        .log
        .glyphs
        .iter()
        .any(|g| g.zone == "R.header" && g.text == "NO CONNECTION"));
    assert!(r
        .canvas
        .log
        .glyphs
        .iter()
        .any(|g| g.zone == "R.alarms" && g.text == "NO CONNECTION"));
}

#[test]
fn only_the_changed_zones_are_redrawn() {
    let feed = feed_from(&demo("both").unwrap());
    let o = opts(1024, 600, 7.0, ViewMode::Single);
    let (mut r, shown) = render(&feed, &o);
    let known: std::collections::HashSet<String> =
        feed.art.iter().map(|(u, _)| u.clone()).collect();
    let has = |u: &str| known.contains(u);
    let cfg = o.cfg.clone();
    let stations = feed.stations.clone();
    // the same frame again: nothing to redraw
    assert!(r
        .frame(&input(
            &stations,
            &shown,
            feed.now_ms,
            &feed.clock,
            &cfg,
            &has
        ))
        .is_empty());
    // a meter moves: only the meters zone is damaged
    let mut st = stations.clone();
    let mut f = st[0].frame.clone().unwrap();
    f.ppm = [Some(-6.0), Some(-6.0)];
    st[0].apply_frame(f, feed.now_ms);
    let d = r.frame(&input(&st, &shown, feed.now_ms, &feed.clock, &cfg, &has));
    let meters = zones(&r).into_iter().find(|z| z.0 == "meters").unwrap().1;
    // only the pixels that changed: the bars, a few rows of the meters zone
    assert_eq!(d.len(), 1);
    assert!(
        meters.contains(&d[0]) && d[0].w * d[0].h < meters.w * meters.h / 4,
        "{d:?}"
    );
    // the clock ticks: the header only (and the countdown's zone when the second changes)
    let d = r.frame(&input(
        &st,
        &shown,
        feed.now_ms + 400,
        "16:21:15",
        &cfg,
        &has,
    ));
    let hd = zones(&r).into_iter().find(|z| z.0 == "header").unwrap().1;
    // the clock, and the progress bar of the item on air grew a pixel
    let now_zone = zones(&r).into_iter().find(|z| z.0 == "now").unwrap().1;
    assert!(
        d.iter().all(|x| hd.contains(x) || now_zone.contains(x))
            && d.iter().any(|x| hd.contains(x)),
        "{d:?}"
    );
}

fn input<'a>(
    stations: &'a [display::model::StationModel],
    shown: &Shown,
    now_ms: i64,
    clock: &str,
    cfg: &'a DisplayConfig,
    has: &'a dyn Fn(&str) -> bool,
) -> display::scene::SceneInput<'a> {
    display::scene::SceneInput {
        stations,
        shown: shown.clone(),
        now_ms,
        clock: clock.into(),
        tz_min: -240,
        product: "Product".into(),
        cfg,
        plan: Plan::new(1024, 600, cfg.diagonal_in, cfg.view_distance_cm),
        has_image: has,
        notices: &[],
        bar: Box::leak(Box::new(display::scene::BarInfo::new("Product", cfg))),
    }
}

// ---- 10 and 11: the curve and the view selector ---------------------------------------------------------------------

fn chain_feed(bands: usize, gr: &[f64]) -> Value {
    let mut v = demo(if bands == 3 { "chain3" } else { "chain5" }).unwrap();
    let mut g = gr.to_vec();
    g.resize(6, 0.0);
    v["stations"][0]["frame"]["band_gr"] = json!(g);
    v
}

fn crossovers(bands: usize) -> Vec<f64> {
    if bands == 3 {
        vec![200.0, 3000.0]
    } else {
        vec![120.0, 500.0, 2000.0, 6000.0]
    }
}

#[test]
fn the_drawn_curve_is_the_reference_curve() {
    let cases: [(usize, Vec<f64>); 5] = [
        (5, vec![-6.0, 0.0, 0.0, 0.0, 0.0]),
        (5, vec![-3.0, -6.0, -9.0, -6.0, -3.0]),
        (5, vec![-12.0; 5]),
        (5, vec![0.0; 5]),
        (3, vec![-4.0, -8.0, -2.0]),
    ];
    for (w, h) in [(1280, 720), (1920, 1080)] {
        for (bands, gr) in &cases {
            let r = draw(&chain_feed(*bands, gr), &opts(w, h, 21.5, ViewMode::Single));
            let plot = r.report.plot.unwrap();
            let range = r.report.curve_range_db;
            assert!(r.report.layers.contains(&"curve"), "{:?}", r.report.layers);
            assert!(!r.report.curve.is_empty());
            let xo = crossovers(*bands);
            for p in &r.report.curve {
                let reference = dspmodel::h_db(&xo, gr, &[], p.f);
                assert!(
                    (p.db - reference).abs() <= 0.25,
                    "{w} {gr:?} at {} Hz: plotted {} vs {reference}",
                    p.f,
                    p.db
                );
                // the vertical position: 0 dB at the top edge, `range` dB down to the bottom of the plot
                let want_y =
                    f64::from(plot.y) + (-reference / range).clamp(0.0, 1.0) * f64::from(plot.h);
                assert!(
                    (p.y - want_y).abs() <= 1.0,
                    "{w} {gr:?} at {} Hz: y {} vs {want_y}",
                    p.f,
                    p.y
                );
                // and the same dB read back from the pixel row
                let back = -((p.y - f64::from(plot.y)) / f64::from(plot.h)) * range;
                assert!((back - reference.max(-range)).abs() <= 0.25);
            }
            // the picture: the amber line crosses sampled columns where the reference puts it
            let labels: Vec<Rect> = r
                .canvas
                .log
                .glyphs
                .iter()
                .filter(|g| {
                    g.zone == "processing"
                        && g.text.len() == 1
                        && g.text.chars().all(|c| c.is_ascii_digit())
                })
                .map(|g| g.bbox)
                .collect();
            for k in [20, 120, 250, 380, 500] {
                let p = &r.report.curve[k.min(r.report.curve.len() - 1)];
                let x = p.x.round() as i32;
                if labels.iter().any(|b| x >= b.x - 3 && x <= b.right() + 3) {
                    continue;
                }
                let want = f64::from(plot.y)
                    + (-dspmodel::h_db(&xo, gr, &[], p.f) / range).clamp(0.0, 1.0)
                        * f64::from(plot.h);
                // the stroke's pixels in this column, avoiding the flat fill: the strongest amber
                let col: Vec<i32> = (plot.y..plot.bottom())
                    .filter(|&y| {
                        let q = px(&r, x, y);
                        i32::from(q[0]) - i32::from(q[2]) > 90 && q[0] > 140
                    })
                    .collect();
                assert!(!col.is_empty(), "{w} {gr:?} x {x}: no curve pixel");
                let mean = col.iter().map(|&y| f64::from(y)).sum::<f64>() / col.len() as f64;
                // a steep part of the curve spans several rows in one column; allow the slope
                let slope = (col.last().unwrap() - col.first().unwrap()) as f64;
                assert!(
                    (mean + 0.5 - want).abs() <= 1.0 + slope / 2.0,
                    "{w} {gr:?} x {x}: line at {mean}, reference {want}"
                );
            }
            // the crossover lines at log10(f / 20) / 3 of the zone width, within a pixel
            assert_eq!(r.report.crossovers.len(), xo.len());
            for (f, x) in &r.report.crossovers {
                let want = f64::from(plot.x) + (f / 20.0).log10() / 3.0 * f64::from(plot.w);
                assert!((x - want).abs() <= 1.0, "crossover {f}: {x} vs {want}");
                // dashes in the picture: lighter pixels in the column
                let xi = x.round() as i32;
                let sum = |p: [u8; 4]| i32::from(p[0]) + i32::from(p[1]) + i32::from(p[2]);
                let lit = (plot.y..plot.bottom())
                    .filter(|&y| {
                        (xi - 1..=xi + 1).any(|c| sum(px(&r, c, y)) - sum(px(&r, xi + 6, y)) > 70)
                    })
                    .count();
                assert!(
                    lit > plot.h as usize / 5,
                    "{w}: crossover {f} not drawn ({lit} lit rows)"
                );
            }
        }
    }
}

#[test]
fn the_processing_view_selector_draws_the_right_layers() {
    let v = chain_feed(5, &[-3.0, -6.0, -9.0, -6.0, -3.0]);
    let layers = |view: &str, src: &str| {
        let mut o = opts(1920, 1080, 21.5, ViewMode::Single);
        o.cfg.processing_view = view.into();
        o.cfg.spectrum_source = src.into();
        let r = draw(&v, &o);
        let mut l = r.report.layers.clone();
        l.sort_unstable();
        (l, r.report.crossovers.len(), r.report.curve.len())
    };
    let (l, xo, c) = layers("curve", "post");
    assert_eq!((l, xo > 0, c > 0), (vec!["curve"], true, true));
    let (l, xo, c) = layers("spectrum", "post");
    assert_eq!((l, xo, c), (vec!["spectrum_post"], 0, 0));
    assert_eq!(layers("both", "post").0, vec!["curve", "spectrum_post"]);
    assert_eq!(layers("both", "pre").0, vec!["curve", "spectrum_pre"]);
    assert_eq!(
        layers("both", "both").0,
        vec!["curve", "spectrum_post", "spectrum_pre"]
    );
    assert_eq!(
        layers("spectrum", "both").0,
        vec!["spectrum_post", "spectrum_pre"]
    );
}

#[test]
fn a_tone_under_gain_reduction_reads_lower_in_the_post_spectrum_by_the_reduction() {
    // pre -30 dBFS at 1 kHz, the band 500 Hz to 2 kHz reduced 9 dB: post sits 9 dB below pre
    let mut v = chain_feed(5, &[-3.0, -6.0, -9.0, -6.0, -3.0]);
    v["stations"][0]["frame"]["pre_tone"] = json!({"hz": 1000.0, "db": -30.0});
    v["stations"][0]["frame"]["post_tone"] = json!({"hz": 1000.0, "db": -39.0});
    v["stations"][0]["frame"]["floor"] = json!(-85.0);
    for (w, h) in [(1280, 720), (1920, 1080)] {
        let mut o = opts(w, h, 21.5, ViewMode::Single);
        o.cfg.spectrum_source = "both".into();
        let r = draw(&v, &o);
        let plot = r.report.plot.unwrap();
        let db_of = |y: i32| -60.0 + f64::from(plot.bottom() - y) / f64::from(plot.h) * 60.0;
        let post = r.report.spectrum_post.iter().min_by_key(|b| b.top).unwrap();
        let pre = r.report.spectrum_pre.iter().min_by_key(|b| b.top).unwrap();
        let diff = db_of(pre.top) - db_of(post.top);
        assert!(
            (diff - 9.0).abs() <= 1.0,
            "{w}: pre {} post {} diff {diff}",
            db_of(pre.top),
            db_of(post.top)
        );
    }
}

#[test]
fn a_new_chain_configuration_moves_the_crossover_lines() {
    let mut f = feed_from(&chain_feed(5, &[-3.0, -6.0, -9.0, -6.0, -3.0]));
    let o = opts(1920, 1080, 21.5, ViewMode::Single);
    let (r1, _) = render(&f, &o);
    // the station sends a `chain` event with new crossovers (chain_cfg_rev changed): the model takes it
    assert!(f.stations[0].chain_rev_seen.is_some());
    let mut fr = f.stations[0].frame.clone().unwrap();
    fr.chain_cfg_rev = 4;
    f.stations[0].apply_frame(fr, f.now_ms);
    assert!(
        f.stations[0].needs_chain(),
        "a new revision in a frame asks for the configuration again"
    );
    let mut cfg = chain_json(5);
    cfg["chain_cfg_rev"] = json!(4);
    cfg["multiband"]["crossovers_hz"] = json!([100.0, 400.0, 1500.0, 5000.0]);
    f.stations[0].apply_json(&json!({"type": "chain", "data": cfg}), f.now_ms);
    assert!(!f.stations[0].needs_chain());
    let (r2, _) = render(&f, &o);
    let a: Vec<i64> = r1.report.crossovers.iter().map(|c| c.0 as i64).collect();
    let b: Vec<i64> = r2.report.crossovers.iter().map(|c| c.0 as i64).collect();
    assert_eq!(a, vec![120, 500, 2000, 6000]);
    assert_eq!(b, vec![100, 400, 1500, 5000]);
}

fn chain_json(bands: usize) -> Value {
    demo(if bands == 3 { "chain3" } else { "chain5" }).unwrap()["stations"][0]["chain"].clone()
}

// ---- 12: stage meters -------------------------------------------------------------------------------------------------

fn stage_feed(bands: usize, mask: u8, hide: bool) -> (Value, Options) {
    let mut v = demo("chain5").unwrap();
    let xo: Vec<f64> = match bands {
        3 => vec![200.0, 3000.0],
        6 => vec![100.0, 400.0, 1200.0, 3500.0, 9000.0],
        _ => vec![120.0, 500.0, 2000.0, 6000.0],
    };
    v["stations"][0]["chain"]["multiband"] =
        json!({"bands": bands, "crossovers_hz": xo, "slope_db_per_octave": 24});
    let f = &mut v["stations"][0]["frame"];
    f["band_count"] = json!(bands);
    f["stage_mask"] = json!(mask);
    f["band_gr"] = json!([-1.0, -2.5, -4.0, -5.5, -7.0, -11.0]);
    let mut o = opts(1920, 1080, 21.5, ViewMode::Single);
    o.cfg.bypassed_stages = if hide { "hide".into() } else { "show".into() };
    (v, o)
}

#[test]
fn stage_columns_show_numerals_and_a_hanging_bar_that_matches_the_frame() {
    for (w, h) in [(1280, 720), (1920, 1080)] {
        let (v, mut o) = stage_feed(5, 0b11111, false);
        o.w = w;
        o.h = h;
        let r = draw(&v, &o);
        assert_eq!(
            r.report.stages.len(),
            5 + 1 + 1 + 1 + 1,
            "AGC, 5 bands, wideband, limiter, clipper"
        );
        let t: Vec<_> = r
            .canvas
            .log
            .glyphs
            .iter()
            .filter(|g| g.zone == "stages")
            .map(|g| g.text.clone())
            .collect();
        for lab in ["AGC", "B1", "B5", "WB", "LIM", "CLP", "SAFE"] {
            assert!(t.iter().any(|x| x == lab), "{w}: {lab} in {t:?}");
        }
        for c in &r.report.stages {
            assert!(!c.bypassed && c.has_numerals, "{w} {}", c.name);
            if let Some(gr) = c.gr_db {
                // the bar hangs from the top of its track (the AGC's is centered), the length matches the frame within a pixel step
                if c.name == "AGC" {
                    let half = f64::from(c.track.h) / 2.0;
                    assert!(
                        (f64::from(c.bar.h) - (gr.abs() / c.range_db) * half).abs() <= 1.0,
                        "AGC"
                    );
                } else {
                    assert_eq!(c.bar.y, c.track.y, "{} hangs from the top", c.name);
                    let want = (-gr / c.range_db).clamp(0.0, 1.0) * f64::from(c.track.h);
                    assert!(
                        (f64::from(c.bar.h) - want).abs() <= 1.0,
                        "{w} {}: bar {} vs {want}",
                        c.name,
                        c.bar.h
                    );
                    // on the picture
                    let x = c.track.x + c.track.w / 2;
                    let lit = (c.track.y..c.track.bottom())
                        .filter(|&y| px(&r, x, y) == rgb(theme::AMBER))
                        .count();
                    assert!(
                        (lit as f64 - want).abs() <= 1.5,
                        "{w} {}: {lit} lit rows vs {want}",
                        c.name
                    );
                }
            }
        }
    }
}

#[test]
fn a_bypassed_stage_is_grey_with_byp_under_show_and_gone_under_hide_and_the_columns_reflow() {
    // the limiter (bit 3) and the clipper (bit 4) are bypassed
    let (v, o) = stage_feed(5, 0b00111, false);
    let r = draw(&v, &o);
    let byp: Vec<_> = r
        .canvas
        .log
        .glyphs
        .iter()
        .filter(|g| g.zone == "stages" && g.text == "BYP")
        .collect();
    assert_eq!(byp.len(), 2);
    assert!(byp.iter().all(|g| g.color == theme::GREY));
    assert_eq!(r.report.stages.iter().filter(|c| c.bypassed).count(), 2);
    let (v, o) = stage_feed(5, 0b00111, true);
    let r2 = draw(&v, &o);
    assert!(r2
        .canvas
        .log
        .glyphs
        .iter()
        .all(|g| !(g.zone == "stages" && g.text == "BYP")));
    assert_eq!(r2.report.stages.len(), r.report.stages.len() - 2);
    // reflowed: the remaining columns are wider and still in a row without overlap
    assert!(r2.report.stages[0].rect.w > r.report.stages[0].rect.w);
    for w in r2.report.stages.windows(2) {
        assert!(!w[0].rect.intersects(&w[1].rect));
    }
    // the whole chain bypassed (the M1 stream): every column grey
    let r = draw(
        &demo("both").unwrap(),
        &opts(1920, 1080, 21.5, ViewMode::Single),
    );
    assert!(r.report.stages.iter().all(|c| c.bypassed));
}

#[test]
fn three_five_and_six_band_chains_lay_out_without_overlap_and_without_small_text() {
    for bands in [3usize, 5, 6] {
        for (w, h) in [(1280, 720), (1920, 1080)] {
            let (v, mut o) = stage_feed(bands, 0b11111, false);
            o.w = w;
            o.h = h;
            let r = draw(&v, &o);
            let zone = zones(&r).into_iter().find(|z| z.0 == "stages").unwrap().1;
            let cols = &r.report.stages;
            assert_eq!(cols.len(), 1 + bands + 3, "{bands} bands at {w}");
            for (i, a) in cols.iter().enumerate() {
                assert!(zone.contains(&a.rect));
                for b in &cols[i + 1..] {
                    assert!(
                        !a.rect.intersects(&b.rect),
                        "{bands} bands at {w}: {} overlaps {}",
                        a.name,
                        b.name
                    );
                }
            }
            let runs: Vec<_> = r
                .canvas
                .log
                .glyphs
                .iter()
                .filter(|g| g.zone == "stages")
                .collect();
            for g in &runs {
                assert!(g.size >= r.plan.min_px, "{} at {} px", g.text, g.size);
                assert!(
                    zone.contains(&g.bbox),
                    "{} {:?} outside {:?}",
                    g.text,
                    g.bbox,
                    zone
                );
            }
            for (i, a) in runs.iter().enumerate() {
                for b in &runs[i + 1..] {
                    assert!(
                        !a.bbox.intersects(&b.bbox),
                        "{bands} bands at {w}: `{}` overlaps `{}`",
                        a.text,
                        b.text
                    );
                }
            }
        }
    }
}

#[test]
fn the_screenshots_are_stable() {
    // the same feed twice is the same picture (the demos are deterministic)
    let a = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Split),
    );
    let b = draw(
        &demo("both").unwrap(),
        &opts(1024, 600, 7.0, ViewMode::Split),
    );
    assert_eq!(a.canvas.rgba(), b.canvas.rgba());
    let _ = Shown::Split { left: 0, right: 1 };
}

#[test]
fn the_bars_draw_the_ballisticked_peak_when_the_preset_says_so() {
    let mut f = feed_from(&tone_feed(-20.0));
    // the frame says PPM -20 and a sample peak of -3: the default shows the PPM, a peak preset shows the peak
    let mut fr = f.stations[0].frame.clone().unwrap();
    fr.peak = [Some(-3.0), Some(-3.0)];
    f.stations[0].apply_frame(fr, f.now_ms);
    let o = opts(1024, 600, 7.0, ViewMode::Single);
    let (r, _) = render(&f, &o);
    let bar = |r: &Renderer| {
        r.report
            .bars
            .iter()
            .find(|b| b.zone == "meters")
            .unwrap()
            .clone()
    };
    assert!((bar(&r).fill_x - db_x(bar(&r).track, -20.0)).abs() <= 1);
    f.stations[0].dynamics.params = common::meters::Ballistics::preset("digital_fast").unwrap();
    let fr = f.stations[0].frame.clone();
    f.stations[0].dynamics.update(fr.as_ref(), f.now_ms);
    let (r, _) = render(&f, &o);
    assert!(
        (bar(&r).fill_x - db_x(bar(&r).track, -3.0)).abs() <= 1,
        "{}",
        bar(&r).fill_x
    );
}

#[test]
fn a_held_station_shows_held_and_how_long_with_nothing_negative() {
    let mut v = demo("both").unwrap();
    // the item ended 32 s ago and the station is held
    v["stations"][0]["snapshot"]["state"] = json!("held");
    v["stations"][0]["snapshot"]["now"]["ends_at"] = json!("2026-10-06T20:20:42.000Z");
    v["stations"][0]["snapshot"]["now"]["segue_at"] = json!("2026-10-06T20:20:30.000Z");
    for (w, h, d) in [(1024, 600, 7.0), (1920, 1080, 21.5)] {
        let r = draw(&v, &opts(w, h, d, ViewMode::Single));
        let t: Vec<_> = r.canvas.log.glyphs.iter().map(|g| g.text.clone()).collect();
        assert!(t.iter().any(|x| x == "HELD 0:32"), "{t:?}");
        assert!(t.iter().any(|x| x == "0:00"), "remaining reads 0:00");
        assert!(
            !t.iter().any(|x| x.starts_with("SEGUE")),
            "no segue countdown while held"
        );
    }
}

#[test]
fn no_alarms_pending_fits_its_zone_at_the_minimum_size() {
    for (w, h, d, view) in [
        (1024, 600, 7.0, ViewMode::Split),
        (800, 480, 9.0, ViewMode::Split),
        (1024, 600, 7.0, ViewMode::Single),
        (1920, 1080, 21.5, ViewMode::Single),
    ] {
        let mut v = demo("both").unwrap();
        v["stations"][0]["snapshot"]["alerts"] = json!([]);
        let r = draw(&v, &opts(w, h, d, view));
        let g = r
            .canvas
            .log
            .glyphs
            .iter()
            .find(|g| g.text == "NO ALARMS PENDING")
            .unwrap_or_else(|| panic!("{w}x{h} {view:?}"));
        let z = zones(&r).into_iter().find(|z| z.0 == g.zone).unwrap().1;
        assert!(
            z.contains(&g.bbox) && g.size >= r.plan.floor_px,
            "{w}x{h} {view:?}"
        );
        println!(
            "NO ALARMS PENDING at {} px is {} px wide in a {} px zone ({w}x{h} {view:?})",
            g.size, g.bbox.w, z.w
        );
    }
}

#[test]
fn two_stations_with_different_presets_fall_differently_in_split() {
    // both stations get a 0 dB peak, then silence for 250 ms: office on digital fast (60 dB/s), eve on the BBC PPM
    // (8.6 dB/s); the bars drawn in the two halves differ by what each station's own setting says
    let mut f = feed_from(&tone_feed(-20.0));
    for (i, name) in ["digital_fast", "ppm_bbc"].iter().enumerate() {
        let st = &mut f.stations[i];
        st.dynamics.params = common::meters::Ballistics::preset(name).unwrap();
        let mut fr = st.frame.clone().unwrap();
        fr.peak = [Some(0.0), Some(0.0)];
        st.apply_frame(fr.clone(), f.now_ms);
        for k in 0..10 {
            st.dynamics.update(Some(&fr), f.now_ms - 400 + k * 33);
        }
        let mut t = f.now_ms - 70;
        while t < f.now_ms + 250 {
            t += 33;
            st.dynamics.update(None, t);
        }
        st.frame = None;
    }
    let (r, _) = render(&f, &opts(1024, 600, 7.0, ViewMode::Split));
    let bar = |zone: &str| {
        r.report
            .bars
            .iter()
            .find(|b| b.zone == zone)
            .unwrap()
            .clone()
    };
    let (l, rt) = (bar("L.meters"), bar("R.meters"));
    let db = |b: &display::scene::MeterBar| b.db.unwrap_or(-90.0);
    // 0.33 s of silence: office fell about 60 dB/s x 0.33 s = 20 dB, eve about 8.6 x 0.33 = 2.8 dB
    assert!((db(&l) + 21.0).abs() < 3.0, "office {}", db(&l));
    assert!((db(&rt) + 2.8).abs() < 1.5, "eve {}", db(&rt));
    // and the drawn bars say so: the right half's fill is longer in its own scale
    let frac =
        |b: &display::scene::MeterBar| f64::from(b.fill_x - b.track.x) / f64::from(b.track.w);
    assert!(frac(&rt) > frac(&l) + 0.15, "{} vs {}", frac(&rt), frac(&l));
    println!("split: office (digital fast) bar at {:.1} dB, eve (BBC) at {:.1} dB after 0.33 s of silence", db(&l), db(&rt));
}
