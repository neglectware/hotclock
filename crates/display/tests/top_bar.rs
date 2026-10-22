//! The shared top bar of `split` (product and version, network dot, clock, date, CPU use) and the loudness
//! controller's bar under the meters, on the offscreen renderer: what is drawn, where, in which colour, measured from
//! the renderer's own log and pixels. With `HOTCLOCK_SNAPSHOT_DIR` set, every case also writes its PNG there.

use common::config::DisplayConfig;
use display::canvas::{theme, Color};
use display::feed::{demo, Feed};
use display::headless::{render, Options};
use display::layout::Rect;
use display::render::Renderer;
use display::scene::date_text;
use display::view::ViewMode;
use serde_json::{json, Value};

const ALL_ON: [&str; 5] = ["network", "product", "clock", "date", "cpu"];

fn opts(w: i32, h: i32, view: ViewMode, cfg: DisplayConfig) -> Options {
    let diagonal_in = if w == 800 { 9.0 } else { 7.0 };
    Options {
        w,
        h,
        cfg: DisplayConfig { diagonal_in, ..cfg },
        view: Some(view),
        station: None,
        product: "Hotclock".into(),
    }
}

/// The `both` demo with a top bar answer and, when `lc` is given, each station's controller gain and state and its
/// gain range (-10 to +8 dB, the default).
fn feed(bar: Option<Value>, lc: Option<(f64, u8, bool)>) -> Value {
    let mut v = demo("both").expect("demo");
    if let Some(b) = bar {
        v["bar"] = b;
    }
    for s in v["stations"].as_array_mut().unwrap() {
        s["loudness"] = json!({"settings": {"gain_min_db": -10.0, "gain_max_db": 8.0}});
        if let Some((g, st, on)) = lc {
            s["frame"]["gain_db"] = json!(g);
            s["frame"]["loudness_state"] = json!(st);
            s["frame"]["target"] = if on { json!(-14.0) } else { Value::Null };
        }
    }
    v
}

fn bar_answer(items: [bool; 5], cpu: Option<u8>, net: Option<&str>, version: &str) -> Value {
    json!({"product": "Hotclock", "version": version, "cpu_percent": cpu, "network": net,
        "items": {"network": items[0], "product": items[1], "clock": items[2], "date": items[3], "cpu": items[4]}})
}

fn draw(v: &Value, o: &Options, snap: &str) -> Renderer {
    let f = Feed::from_json(v, std::path::Path::new(".")).expect("feed");
    let r = render(&f, o).0;
    if let Some(d) = std::env::var_os("HOTCLOCK_SNAPSHOT_DIR") {
        let p = std::path::Path::new(&d).join(format!("{snap}.png"));
        r.canvas.save_png(&p).expect("png");
    }
    r
}

fn px(r: &Renderer, x: i32, y: i32) -> [u8; 3] {
    let i = (y as usize * r.plan.w as usize + x as usize) * 4;
    let d = r.canvas.rgba();
    [d[i], d[i + 1], d[i + 2]]
}

fn rgb(c: Color) -> [u8; 3] {
    [c.0, c.1, c.2]
}

fn zone(r: &Renderer, name: &str) -> Rect {
    r.layout.as_ref().unwrap().zone(name)
}

/// The bar's items as drawn: name, text and the ink box (the glyph log's for text, the report's for the dot).
fn bar_items(r: &Renderer) -> Vec<(String, String, Rect)> {
    r.report
        .bar_items
        .iter()
        .map(|(n, t, rect)| {
            let ink = r
                .canvas
                .log
                .glyphs
                .iter()
                .find(|g| g.zone == "bar" && g.text == *t && !t.is_empty())
                .map_or(*rect, |g| g.bbox);
            (n.clone(), t.clone(), ink)
        })
        .collect()
}

/// Every item inside the bar, none cut, at least half a character apart, at least the clock's size.
fn check_bar(r: &Renderer, want: &[&str], what: &str) -> Vec<(String, String, Rect)> {
    let bar = zone(r, "bar");
    let mut it = bar_items(r);
    let mut names: Vec<&str> = it.iter().map(|x| x.0.as_str()).collect();
    names.sort_unstable();
    let mut w: Vec<&str> = want.to_vec();
    w.sort_unstable();
    assert_eq!(names, w, "{what}: items");
    it.sort_by_key(|x| x.2.x);
    for (n, t, b) in &it {
        assert!(
            b.x >= bar.x && b.right() <= bar.right() && b.y >= bar.y && b.bottom() <= bar.bottom(),
            "{what}: {n} {b:?} leaves the bar {bar:?}"
        );
        assert!(!t.contains('…'), "{what}: {n} is cut: {t}");
    }
    let clock = r.report.bar_items.iter().find(|x| x.0 == "clock");
    for g in r.canvas.log.glyphs.iter().filter(|g| g.zone == "bar") {
        let size = clock.map_or(31, |_| {
            r.canvas
                .log
                .glyphs
                .iter()
                .find(|c| c.zone == "bar" && Some(&c.text) == clock.map(|c| &c.1))
                .map_or(0, |c| c.size)
        });
        assert!(g.size >= size, "{what}: {} at {} px", g.text, g.size);
    }
    let half_char = r.plan.min_px / 4;
    for p in it.windows(2) {
        assert!(
            p[1].2.x - p[0].2.right() >= half_char,
            "{what}: {} and {} are {} px apart",
            p[0].0,
            p[1].0,
            p[1].2.x - p[0].2.right()
        );
    }
    it
}

#[test]
fn the_date_is_the_local_day_month_and_weekday() {
    let ms = |s: &str| display::model::parse_iso_ms(s).unwrap();
    assert_eq!(date_text(ms("2026-10-10T12:00:00Z"), 0), "SAT 10 OCT");
    assert_eq!(date_text(ms("1970-01-01T00:00:00Z"), 0), "THU 1 JAN");
    // a minute before midnight in New York is the next day in UTC
    assert_eq!(date_text(ms("2026-10-11T03:59:00Z"), -240), "SAT 10 OCT");
    assert_eq!(date_text(ms("2028-02-29T10:00:00Z"), 0), "TUE 29 FEB");
    assert_eq!(date_text(ms("2026-12-31T23:30:00Z"), 60), "FRI 1 JAN");
}

#[test]
fn the_split_bar_shows_every_item_inside_its_40_px_apart_and_whole() {
    let o = opts(1024, 600, ViewMode::Split, DisplayConfig::default());
    for (cpu, version, snap) in [
        (Some(7), "0.1.0-26", "native-split-bar-all-1024x600"),
        // the widest: CPU 100% and a three-digit package revision
        (Some(100), "0.1.0-125", "native-split-bar-widest-1024x600"),
    ] {
        let v = feed(Some(bar_answer([true; 5], cpu, Some("up"), version)), None);
        let r = draw(&v, &o, snap);
        let bar = zone(&r, "bar");
        assert_eq!(bar, Rect::new(0, 0, 1024, 40));
        let it = check_bar(&r, &ALL_ON, snap);
        let text = |n: &str| it.iter().find(|x| x.0 == n).unwrap().1.clone();
        assert_eq!(text("product"), format!("HOTCLOCK {version}"));
        assert_eq!(text("cpu"), format!("CPU {}%", cpu.unwrap()));
        assert_eq!(text("date"), "TUE 6 OCT");
        // the clock in the middle, the dot green
        let c = it.iter().find(|x| x.0 == "clock").unwrap().2;
        assert!((c.x + c.w / 2 - 512).abs() <= 2, "{c:?}");
        let d = it.iter().find(|x| x.0 == "network").unwrap().2;
        assert_eq!(px(&r, d.x + d.w / 2, d.y + d.h / 2), rgb(theme::GREEN));
        // CPU from 85 % in amber
        let g = r
            .canvas
            .log
            .glyphs
            .iter()
            .find(|g| g.text.starts_with("CPU"))
            .unwrap();
        assert_eq!(
            g.color,
            if cpu.unwrap() >= 85 {
                theme::AMBER
            } else {
                theme::TEXT
            }
        );
        let layout: Vec<String> = it
            .iter()
            .map(|(n, t, b)| {
                format!(
                    "{n} \"{t}\" {}..{} y {}..{}",
                    b.x,
                    b.right(),
                    b.y,
                    b.bottom()
                )
            })
            .collect();
        eprintln!("{snap}: {}", layout.join(", "));
    }
}

#[test]
fn the_bar_defaults_follow_the_config_until_the_library_answers_and_its_items_then() {
    // nothing from the library service yet: the config's items (network off), the product without a version, CPU --
    let o = opts(1024, 600, ViewMode::Split, DisplayConfig::default());
    let r = draw(&feed(None, None), &o, "native-split-bar-defaults-1024x600");
    let it = check_bar(&r, &["product", "clock", "date", "cpu"], "defaults");
    assert_eq!(it.iter().find(|x| x.0 == "cpu").unwrap().1, "CPU --");
    assert_eq!(it.iter().find(|x| x.0 == "product").unwrap().1, "HOTCLOCK");
    // the defaults as the library service answers them (network off)
    let v = feed(
        Some(bar_answer(
            [false, true, true, true, true],
            Some(12),
            Some("up"),
            "0.1.0-26",
        )),
        None,
    );
    let r = draw(&v, &o, "native-split-bar-defaults-answered-1024x600");
    check_bar(
        &r,
        &["product", "clock", "date", "cpu"],
        "answered defaults",
    );
    // a config with every item off and none answered: an empty bar
    let off = DisplayConfig {
        bar_product: false,
        bar_clock: false,
        bar_date: false,
        bar_cpu: false,
        bar_network: false,
        ..DisplayConfig::default()
    };
    let r = draw(
        &feed(None, None),
        &opts(1024, 600, ViewMode::Split, off),
        "x",
    );
    assert!(r.report.bar_items.is_empty());
}

#[test]
fn each_bar_setting_hides_its_item_and_the_others_stay() {
    let o = opts(1024, 600, ViewMode::Split, DisplayConfig::default());
    for i in 0..5 {
        let mut items = [true; 5];
        items[i] = false;
        let v = feed(
            Some(bar_answer(items, Some(40), Some("down"), "0.1.0-26")),
            None,
        );
        let r = draw(&v, &o, "x");
        let want: Vec<&str> = ALL_ON.iter().copied().filter(|n| *n != ALL_ON[i]).collect();
        check_bar(&r, &want, &format!("without {}", ALL_ON[i]));
        if i != 0 {
            let d = bar_items(&r)
                .into_iter()
                .find(|x| x.0 == "network")
                .unwrap()
                .2;
            assert_eq!(
                px(&r, d.x + d.w / 2, d.y + d.h / 2),
                rgb(theme::RED),
                "a red dot when down"
            );
        }
    }
    // no reading: CPU -- and a grey dot
    let v = feed(Some(bar_answer([true; 5], None, None, "0.1.0-26")), None);
    let r = draw(&v, &o, "x");
    let it = check_bar(&r, &ALL_ON, "no reading");
    assert_eq!(it.iter().find(|x| x.0 == "cpu").unwrap().1, "CPU --");
    let d = it.iter().find(|x| x.0 == "network").unwrap().2;
    assert_eq!(px(&r, d.x + d.w / 2, d.y + d.h / 2), rgb(theme::DIM));
}

#[test]
fn the_bar_fits_the_800_x_480_fallback_and_single_keeps_its_own_header() {
    let v = feed(
        Some(bar_answer([true; 5], Some(100), Some("up"), "0.1.0-125")),
        None,
    );
    let r = draw(
        &v,
        &opts(800, 480, ViewMode::Split, DisplayConfig::default()),
        "native-split-bar-all-800x480",
    );
    assert_eq!(zone(&r, "bar").h, 32);
    check_bar(&r, &ALL_ON, "800 x 480");
    // single and rotate have no shared bar: their header is unchanged
    for view in [ViewMode::Single, ViewMode::Rotate] {
        let r = draw(&v, &opts(1024, 600, view, DisplayConfig::default()), "x");
        assert!(r.report.bar_items.is_empty());
        assert!(!r.layout.as_ref().unwrap().has("bar"));
    }
}

// ---- the loudness controller's bar ---------------------------------------------------------------------------------

const CASES: [(&str, f64, u8, bool); 7] = [
    ("boost", 4.5, 2, true),
    ("cut", -6.0, 1, true),
    ("at-limit", 8.0, 4, true),
    ("cut-at-limit", -10.0, 4, true),
    ("frozen", -2.0, 3, true),
    ("grey-off", 0.5, 0, true),
    ("off", 0.0, 0, false),
];

fn lc_color(state: u8) -> [u8; 3] {
    match state {
        1 => [0x5a, 0xa9, 0xff],
        2 => rgb(theme::GREEN),
        3 | 4 => rgb(theme::AMBER),
        _ => [0x4a, 0x52, 0x62],
    }
}

/// The meter zones of a picture: each one's LC bar against the gain, the colour of its fill, the bars and the text
/// inside the zone and apart.
fn check_lc(r: &Renderer, zones: &[&str], case: (&str, f64, u8, bool), what: &str) -> Vec<String> {
    let (name, gain, state, on) = case;
    let mut out = vec![];
    for z in zones {
        let zr = zone(r, z);
        let lc: Vec<_> = r.report.lc.iter().filter(|l| l.zone == *z).collect();
        let runs: Vec<_> = r
            .canvas
            .log
            .glyphs
            .iter()
            .filter(|g| g.zone == *z)
            .collect();
        let num = format!("{gain:+.1}");
        if !on {
            assert!(
                lc.is_empty(),
                "{what} {name} {z}: a bar with the controller off"
            );
            assert!(
                runs.iter().all(|g| g.text != num || gain == 0.0),
                "{what} {name} {z}"
            );
            continue;
        }
        assert_eq!(lc.len(), 1, "{what} {name} {z}: one LC bar");
        let l = lc[0];
        assert_eq!(l.range_db, 10.0);
        let want =
            l.x0 + ((gain / 10.0).clamp(-1.0, 1.0) * f64::from(l.track.w) / 2.0).round() as i32;
        assert_eq!(l.fill_x, want, "{what} {name} {z}");
        assert_eq!(gain > 0.0, l.fill_x > l.x0, "{what} {name} {z}: direction");
        // the drawn colour halfway along the fill
        let mx = (l.x0 + l.fill_x) / 2;
        assert_eq!(
            px(r, mx, l.track.y + l.track.h / 2),
            lc_color(state),
            "{what} {name} {z}: colour"
        );
        // the bar inside the zone, under the level bars and clear of them
        assert!(
            zr.contains(&l.track),
            "{what} {name} {z}: {:?} leaves {zr:?}",
            l.track
        );
        for b in r.report.bars.iter().filter(|b| b.zone == *z) {
            assert!(
                b.track.bottom() <= l.track.y,
                "{what} {name} {z}: LC overlaps a level bar"
            );
        }
        // the number, in the bar's colour (800 x 480 single: its controller line, which says nothing in OFF)
        let n = runs
            .iter()
            .find(|g| g.text == num || g.text.starts_with(&format!("{num} dB")));
        match n {
            Some(n) => assert_eq!(
                rgb(n.color),
                lc_color(state),
                "{what} {name} {z}: number colour"
            ),
            None => assert!(
                r.plan.w == 800 && *z == "meters" && state == 0,
                "{what} {name} {z}: no {num}"
            ),
        }
        let mut prev = l.track.right();
        for (i, g) in runs.iter().enumerate() {
            assert!(
                zr.contains(&g.bbox),
                "{what} {name} {z}: {} leaves the zone",
                g.text
            );
            assert!(
                !g.text.contains('…'),
                "{what} {name} {z}: {} is cut",
                g.text
            );
            // the 800 x 480 scale labels under the level bars: clear of the LC bar
            if g.bbox.right() <= l.track.right() {
                assert!(
                    g.bbox.bottom() <= l.track.y || g.bbox.y >= l.track.bottom(),
                    "{what} {name} {z}: {} {:?} overlaps the LC bar {:?}",
                    g.text,
                    g.bbox,
                    l.track
                );
                continue;
            }
            // the text right of the bars, no two runs on each other
            assert!(
                g.bbox.x >= l.track.right(),
                "{what} {name} {z}: {} reaches into the bars",
                g.text
            );
            for h in &runs[i + 1..] {
                let apart = g.bbox.right() <= h.bbox.x
                    || h.bbox.right() <= g.bbox.x
                    || g.bbox.bottom() <= h.bbox.y
                    || h.bbox.bottom() <= g.bbox.y;
                assert!(
                    apart,
                    "{what} {name} {z}: {} and {} overlap",
                    g.text, h.text
                );
            }
            prev = prev.max(g.bbox.right());
        }
        out.push(format!(
            "{z}: LC bar {}x{} at y {} (zone {}..{}), fill {}..{}, text ends at {} of {}",
            l.track.w,
            l.track.h,
            l.track.y,
            zr.y,
            zr.bottom(),
            l.x0.min(l.fill_x),
            l.x0.max(l.fill_x),
            prev,
            zr.right()
        ));
    }
    out
}

#[test]
fn the_loudness_controller_bar_in_single_split_and_the_800_x_480_layouts() {
    for (w, h, view, zones) in [
        (1024, 600, ViewMode::Single, vec!["meters"]),
        (1024, 600, ViewMode::Split, vec!["L.meters", "R.meters"]),
        (800, 480, ViewMode::Single, vec!["meters"]),
        (800, 480, ViewMode::Split, vec!["L.meters", "R.meters"]),
    ] {
        let o = opts(w, h, view, DisplayConfig::default());
        for case in CASES {
            let v = feed(
                Some(bar_answer([true; 5], Some(7), Some("up"), "0.1.0-26")),
                Some((case.1, case.2, case.3)),
            );
            let snap = format!("native-{}-lc-{}-{w}x{h}", view.name(), case.0);
            let r = draw(&v, &o, &snap);
            let lines = check_lc(&r, &zones, case, &snap);
            if case.0 == "boost" || case.0 == "cut-at-limit" {
                eprintln!("{snap}: {}", lines.join("; "));
            }
            // the label in single at 1024 x 600; the 800 x 480 single keeps the gain on its second line
            let single = view == ViewMode::Single;
            let has_lc_label = r
                .canvas
                .log
                .glyphs
                .iter()
                .any(|g| g.zone == "meters" && g.text == "LC");
            assert_eq!(
                has_lc_label,
                single && w == 1024 && case.3,
                "{snap}: the LC label"
            );
            // with the controller off the target stays where it was
            if !case.3 {
                assert!(
                    r.report.loudness.iter().all(|(_, t)| !t.contains(" / ")),
                    "{snap}"
                );
            }
        }
    }
}

#[test]
fn without_the_gain_range_there_is_no_bar_and_the_target_shows() {
    let mut v = feed(None, Some((4.5, 2, true)));
    for s in v["stations"].as_array_mut().unwrap() {
        s.as_object_mut().unwrap().remove("loudness");
    }
    let r = draw(
        &v,
        &opts(1024, 600, ViewMode::Single, DisplayConfig::default()),
        "x",
    );
    assert!(r.report.lc.is_empty());
    assert!(
        r.report.loudness.iter().any(|(_, t)| t.ends_with("/ -14")),
        "{:?}",
        r.report.loudness
    );
}
