//! The processing view (architecture 11.2c) on the offscreen renderer, with synthetic frames: each band's bar against
//! its gain reduction, bars growing downward, bypassed and absent blocks reflowing without overlap for 2, 3, 5 and 6
//! bands, no text under the minimum, the rotation through the processing views, the ballistics of the bars.
//! The checks read the renderer's report and log and the drawn pixels.

use common::config::DisplayConfig;
use display::canvas::{theme, Color};
use display::feed::{demo, Feed};
use display::headless::{render, Options};
use display::layout::Rect;
use display::model::{Alert, StationModel};
use display::render::Renderer;
use display::scene::{ProcCol, Shown};
use display::view::{Presenter, ViewMode, ViewSettings};
use serde_json::{json, Value};

fn feed_from(v: &Value) -> Feed {
    Feed::from_json(v, std::path::Path::new(".")).expect("feed")
}

fn opts(w: i32, h: i32, diag: f64) -> Options {
    Options {
        w,
        h,
        cfg: DisplayConfig {
            diagonal_in: diag,
            ..DisplayConfig::default()
        },
        view: Some(ViewMode::Processing),
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

/// The crossovers of an `n`-band chain.
fn crossovers(n: usize) -> Vec<f64> {
    [120.0, 400.0, 1200.0, 3500.0, 9000.0][..n - 1].to_vec()
}

/// A station whose frames are all the same (so the bars settle on them): `bands` bands with reductions `gr`, the
/// stage mask `mask` (bit 0 AGC, 1 multiband, 2 wideband, 3 limiter, 4 clipper), the whole chain on.
fn proc_feed(bands: usize, gr: &[f64], mask: u8) -> Value {
    let mut v = demo("proc").unwrap();
    let mut f = v["stations"][0]["frames"][23].clone();
    let mut g: Vec<Value> = gr.iter().take(bands).map(|x| json!(x)).collect();
    g.resize(6, Value::Null);
    f["band_gr"] = json!(g);
    f["band_count"] = json!(bands);
    f["stage_mask"] = json!(mask);
    v["stations"][0]["frames"] = json!(vec![f; 24]);
    v["stations"][0]["chain"]["multiband"] =
        json!({"bands": bands, "crossovers_hz": crossovers(bands), "slope_db_per_octave": 24});
    v
}

/// The zone a drawn zone belongs to: the columns are drawn as `cols.bar.3` and so on, one zone each.
fn logical(zone: &str) -> &str {
    zone.split('.').next().unwrap_or(zone)
}

fn text_runs(r: &Renderer, zone: &str) -> Vec<(String, i32, Rect, Color)> {
    r.canvas
        .log
        .glyphs
        .iter()
        .filter(|g| logical(&g.zone) == zone)
        .map(|g| (g.text.clone(), g.size, g.bbox, g.color))
        .collect()
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

const CONFIGS: [(i32, i32, f64); 6] = [
    (1024, 600, 7.0),
    (800, 480, 9.0),
    (800, 480, 7.0),
    (1280, 720, 21.5),
    (1920, 1080, 21.5),
    (1920, 1080, 15.6),
];

fn proc_of(r: &Renderer) -> &display::scene::ProcReport {
    r.report.proc.as_ref().expect("a processing report")
}

fn col<'a>(r: &'a Renderer, name: &str) -> &'a ProcCol {
    proc_of(r)
        .cols
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

// ---- the layout ---------------------------------------------------------------------------------------------------

#[test]
fn the_zones_tile_the_screen_and_the_text_size_is_31_px_on_the_primary_panel() {
    for (w, h, d) in CONFIGS {
        let r = draw(&demo("proc").unwrap(), &opts(w, h, d));
        let zs = zones(&r);
        let mut cover = vec![0u8; (w * h) as usize];
        for z in zs.iter().map(|z| z.1) {
            assert!(Rect::new(0, 0, w, h).contains(&z), "{w}x{h}");
            for y in z.y..z.bottom() {
                for x in z.x..z.right() {
                    cover[(y * w + x) as usize] += 1;
                }
            }
        }
        assert!(
            cover.iter().all(|&c| c == 1),
            "{w}x{h}: zones overlap or leave a gap"
        );
        let names: Vec<_> = zs.iter().map(|z| z.0.as_str()).collect();
        assert_eq!(names, ["header", "now", "cols", "meters"], "{w}x{h}");
        // the zones that are really drawn (the columns are many) tile the screen too, each inside its layout zone
        let mut cover = vec![0u8; (w * h) as usize];
        for z in &r.zones {
            let layout = zs.iter().find(|l| l.0 == logical(&z.name)).unwrap().1;
            assert!(
                layout.contains(&z.rect),
                "{w}x{h}: {} {:?} outside {layout:?}",
                z.name,
                z.rect
            );
            for y in z.rect.y..z.rect.bottom() {
                for x in z.rect.x..z.rect.right() {
                    cover[(y * w + x) as usize] += 1;
                }
            }
        }
        assert!(
            cover.iter().all(|&c| c == 1),
            "{w}x{h}: the drawn zones overlap or leave a gap"
        );
    }
    let r = draw(&demo("proc").unwrap(), &opts(1024, 600, 7.0));
    assert_eq!(r.plan.min_px, 31);
    assert_eq!(proc_of(&r).t, 31);
    let want = [
        ("header", Rect::new(0, 0, 1024, 44)),
        ("now", Rect::new(0, 44, 1024, 40)),
        ("cols", Rect::new(0, 84, 1024, 390)),
        ("meters", Rect::new(0, 474, 1024, 126)),
    ];
    for (n, rect) in want {
        assert_eq!(
            zones(&r).into_iter().find(|z| z.0 == n).unwrap().1,
            rect,
            "{n}"
        );
    }
    // the fallback draws its own 19 px
    let r = draw(&demo("proc").unwrap(), &opts(800, 480, 9.0));
    assert_eq!(proc_of(&r).t, 19);
}

#[test]
fn no_text_is_drawn_below_the_minimum_and_every_run_stays_in_its_zone_and_clear_of_the_others() {
    for (w, h, d) in CONFIGS {
        for bands in [2usize, 3, 5, 6] {
            for hide in [false, true] {
                let mut v = proc_feed(bands, &[-3.0, -6.5, -9.0, -12.0, -4.0, -1.0], 0b00111);
                v["stations"][0]["frames"][23]["flags"] = json!(18);
                let mut o = opts(w, h, d);
                o.cfg.bypassed_stages = if hide { "hide".into() } else { "show".into() };
                let r = draw(&v, &o);
                let floor = r.plan.floor_px;
                let zs = zones(&r);
                assert!(!r.canvas.log.glyphs.is_empty());
                for g in &r.canvas.log.glyphs {
                    assert!(
                        g.size >= floor,
                        "{w}x{h} {bands} bands: `{}` at {} px < {floor}",
                        g.text,
                        g.size
                    );
                    let z = zs.iter().find(|z| z.0 == logical(&g.zone)).unwrap().1;
                    let own = r.zones.iter().find(|z| z.name == g.zone).unwrap().rect;
                    assert!(
                        (z.contains(&g.bbox) && own.contains(&g.bbox)) || g.bbox.w == 0,
                        "{w}x{h} {bands} bands: `{}` {:?} outside {} {:?}",
                        g.text,
                        g.bbox,
                        g.zone,
                        z
                    );
                }
                let runs = &r.canvas.log.glyphs;
                for (i, a) in runs.iter().enumerate() {
                    for b in &runs[i + 1..] {
                        assert!(
                            a.bbox.w == 0 || b.bbox.w == 0 || !a.bbox.intersects(&b.bbox),
                            "{w}x{h} {bands} bands hide={hide}: `{}` {:?} overlaps `{}` {:?}",
                            a.text,
                            a.bbox,
                            b.text,
                            b.bbox
                        );
                    }
                }
            }
        }
    }
    // on the primary panel the smallest text drawn is exactly the 31 px minimum
    let r = draw(&demo("proc").unwrap(), &opts(1024, 600, 7.0));
    assert_eq!(r.canvas.log.glyphs.iter().map(|g| g.size).min(), Some(31));
}

// ---- the bars -----------------------------------------------------------------------------------------------------

#[test]
fn each_bands_bar_matches_its_reduction_within_one_step_of_the_scale_and_grows_downward() {
    let gr = [-1.5, -4.0, -9.25, -6.0, -12.5, -2.0];
    for (w, h, d) in [
        (1024, 600, 7.0),
        (800, 480, 9.0),
        (1280, 720, 21.5),
        (1920, 1080, 21.5),
    ] {
        for bands in [2usize, 3, 5, 6] {
            let v = proc_feed(bands, &gr, 0b11111);
            let r = draw(&v, &opts(w, h, d));
            let p = proc_of(&r);
            let range = p.range_db;
            assert_eq!(range, 18.0);
            let mut seen = 0;
            for (b, &g) in gr.iter().enumerate().take(bands) {
                let c = col(&r, &format!("band{}", b + 1));
                assert!(!c.bypassed);
                // the bar is the frame's reduction on the scale: one pixel is range / track.h dB
                let want = (-g / range).clamp(0.0, 1.0) * f64::from(c.track.h);
                assert!(
                    (f64::from(c.bar.h) - want).abs() <= 1.0,
                    "{w}x{h} {bands} bands band {}: bar {} px, scale says {want:.2}",
                    b + 1,
                    c.bar.h
                );
                // the settled ballistics read the frame exactly
                assert!((c.bar_db + g).abs() < 1e-9, "{} {}", c.bar_db, g);
                // it grows downward from the top of its track
                assert_eq!(c.bar.y, c.track.y, "band {} hangs from the top", b + 1);
                assert_eq!((c.bar.x, c.bar.w), (c.track.x, c.track.w));
                // and it is what is on the picture: the lit rows of the column
                let x = c.track.x + c.track.w / 2;
                let lit = (c.track.y..c.track.bottom())
                    .filter(|&y| {
                        let [r_, g_, b_, _] = px(&r, x, y);
                        r_ > 200 && g_ > 100 && b_ < 130
                    })
                    .count();
                assert!(
                    (lit as f64 - want).abs() <= 1.5 + 0.0,
                    "{w}x{h} band {}: {lit} lit rows, expected {want:.2}",
                    b + 1
                );
                seen += 1;
            }
            assert_eq!(seen, bands);
            // wideband, limiter and clipper: their own reductions, on the same scale
            for (name, g) in [
                ("wideband", -1.2 * 1.0),
                ("limiter", -2.0),
                ("clipper", -0.5),
            ] {
                let c = col(&r, name);
                let want = (-g / range).clamp(0.0, 1.0) * f64::from(c.track.h);
                assert!(
                    (f64::from(c.bar.h) - want).abs() <= 1.0,
                    "{name}: {} vs {want}",
                    c.bar.h
                );
                assert_eq!(c.bar.y, c.track.y);
            }
            // all the tracks share one top and one height (one scale), and the scale's lines are every 6 dB from 0 at the top
            let top = col(&r, "band1").track;
            for c in p.cols.iter().filter(|c| !c.centered) {
                assert_eq!((c.track.y, c.track.h), (top.y, top.h), "{}", c.name);
            }
            assert_eq!(p.grid.first().map(|g| (g.0, g.1)), Some((0.0, top.y)));
            assert_eq!(p.grid.len(), 4, "0, 6, 12, 18");
            for (db, y) in &p.grid {
                let want = f64::from(top.y) + db / range * f64::from(top.h);
                assert!((f64::from(*y) - want).abs() <= 0.5 + 1e-9);
            }
        }
    }
}

#[test]
fn the_scale_range_is_a_setting_and_the_bars_follow_it() {
    let v = proc_feed(5, &[-3.0, -6.0, -9.0, -6.0, -3.0], 0b11111);
    for range in [6.0, 12.0, 18.0, 30.0] {
        let mut o = opts(1024, 600, 7.0);
        o.cfg.proc_range_db = range;
        let r = draw(&v, &o);
        let c = col(&r, "band3");
        assert_eq!(proc_of(&r).range_db, range);
        let want = (9.0f64 / range).clamp(0.0, 1.0) * f64::from(c.track.h);
        assert!((f64::from(c.bar.h) - want).abs() <= 1.0, "{range}");
        // the grid has a line every 6 dB down to the range
        assert_eq!(
            proc_of(&r).grid.len(),
            (range / 6.0) as usize + 1,
            "{range}"
        );
    }
}

#[test]
fn the_agc_bar_is_centered_boost_up_and_cut_down() {
    for (gain, up) in [(2.4f64, true), (-4.5, false), (9.0, true), (-12.0, false)] {
        let mut v = proc_feed(5, &[-3.0; 5], 0b11111);
        v["stations"][0]["frames"][23]["stages"]["agc"] =
            json!({"in": -22.0, "out": -19.0, "gr": gain});
        let r = draw(&v, &opts(1024, 600, 7.0));
        let c = col(&r, "agc");
        assert!(c.centered && c.range_db == 9.0);
        let mid = c.track.y + c.track.h / 2;
        let half = f64::from(c.track.h) / 2.0;
        let want = (gain.abs() / 9.0).min(1.0) * half;
        assert!(
            (f64::from(c.bar.h) - want).abs() <= 1.0,
            "{gain}: {} vs {want}",
            c.bar.h
        );
        if up {
            assert_eq!(c.bar.bottom(), mid, "{gain}: boost rises from the middle");
        } else {
            assert_eq!(c.bar.y, mid, "{gain}: cut hangs from the middle");
        }
    }
}

#[test]
fn the_leds_follow_the_flags_and_the_footer_says_gate_only_when_it_is() {
    for (flags, gate, clip) in [
        (0, false, false),
        (2, true, false),
        (16, false, true),
        (18, true, true),
    ] {
        let mut v = proc_feed(5, &[-3.0; 5], 0b11111);
        v["stations"][0]["frames"][23]["flags"] = json!(flags);
        let r = draw(&v, &opts(1024, 600, 7.0));
        assert_eq!(col(&r, "agc").led, Some(gate), "flags {flags}");
        assert_eq!(col(&r, "clipper").led, Some(clip), "flags {flags}");
        assert!(col(&r, "limiter").led.is_none());
        let said = text_runs(&r, "cols").iter().any(|t| t.0 == "AGC GATE");
        assert_eq!(said, gate, "flags {flags}");
    }
}

// ---- bypassed and absent blocks ---------------------------------------------------------------------------------------

fn no_overlap(r: &Renderer, what: &str) {
    let cols = &proc_of(r).cols;
    let zone = zones(r).into_iter().find(|z| z.0 == "cols").unwrap().1;
    for (i, a) in cols.iter().enumerate() {
        assert!(zone.contains(&a.rect), "{what}: {} outside", a.name);
        assert!(zone.contains(&a.track), "{what}: {} track outside", a.name);
        for b in &cols[i + 1..] {
            assert!(
                !a.rect.intersects(&b.rect),
                "{what}: {} overlaps {}",
                a.name,
                b.name
            );
            assert!(!a.track.intersects(&b.track), "{what}: tracks");
        }
    }
}

#[test]
fn a_bypassed_block_is_grey_with_byp_and_an_omitted_one_reflows_without_overlap() {
    for bands in [2usize, 3, 5, 6] {
        for (w, h, d) in [(1024, 600, 7.0), (800, 480, 9.0), (1920, 1080, 21.5)] {
            // the limiter (bit 3) and the clipper (bit 4) are bypassed
            let v = proc_feed(bands, &[-2.0; 6], 0b00111);
            let shown = draw(&v, &opts(w, h, d));
            let p = proc_of(&shown);
            assert_eq!(p.cols.len(), 1 + bands + 3, "{bands} bands at {w}");
            assert_eq!(p.cols.iter().filter(|c| c.bypassed).count(), 2);
            let byp: Vec<_> = text_runs(&shown, "cols")
                .into_iter()
                .filter(|t| t.0 == "BYP")
                .collect();
            assert_eq!(byp.len(), 2, "{bands} bands at {w}");
            assert!(byp.iter().all(|t| t.3 == theme::GREY));
            for c in p.cols.iter().filter(|c| c.bypassed) {
                assert_eq!(c.bar.h, 0, "{} has no bar", c.name);
                assert_eq!(
                    px(&shown, c.track.x + 1, c.track.y + 1),
                    rgb(Color::rgb(0x1a, 0x1f, 0x27)),
                    "{} is drawn dark",
                    c.name
                );
            }
            no_overlap(&shown, &format!("show {bands} bands {w}"));
            // `hide`: the two are gone and the rest are wider
            let mut o = opts(w, h, d);
            o.cfg.bypassed_stages = "hide".into();
            let hidden = draw(&v, &o);
            let ph = proc_of(&hidden);
            assert_eq!(ph.cols.len(), p.cols.len() - 2);
            assert!(text_runs(&hidden, "cols").iter().all(|t| t.0 != "BYP"));
            let gr_w = |p: &display::scene::ProcReport| {
                p.cols.iter().find(|c| !c.centered).unwrap().rect.w
            };
            assert!(
                gr_w(ph) > gr_w(p),
                "{bands} bands at {w}: columns reflow wider"
            );
            no_overlap(&hidden, &format!("hide {bands} bands {w}"));
            // a block the configuration does not list at all is absent, not grey, whatever `bypassed_stages` says
            let mut v2 = proc_feed(bands, &[-2.0; 6], 0b11111);
            v2["stations"][0]["chain"]["stages"] = json!([
                {"name": "agc", "enabled": true}, {"name": "multiband", "enabled": true},
                {"name": "limiter", "enabled": true, "ceiling_dbtp": -1.0}, {"name": "safety_limiter", "enabled": true}
            ]);
            let absent = draw(&v2, &opts(w, h, d));
            let pa = proc_of(&absent);
            let names: Vec<_> = pa.cols.iter().map(|c| c.name.as_str()).collect();
            assert!(
                !names.contains(&"wideband") && !names.contains(&"clipper"),
                "{names:?}"
            );
            assert_eq!(pa.cols.len(), 1 + bands + 1);
            assert!(pa.cols.iter().all(|c| !c.bypassed));
            assert!(gr_w(pa) >= gr_w(p));
            no_overlap(&absent, &format!("absent {bands} bands {w}"));
        }
    }
    // the whole chain bypassed (the M1 stream): every column grey, and a bypassed multiband is one grey column
    let r = draw(&demo("proc").unwrap(), &opts(1024, 600, 7.0));
    assert!(proc_of(&r).cols.iter().all(|c| !c.bypassed));
    let mut v = demo("proc").unwrap();
    for f in v["stations"][0]["frames"].as_array_mut().unwrap() {
        f["flags"] = json!(1);
    }
    let r = draw(&v, &opts(1024, 600, 7.0));
    assert!(proc_of(&r).cols.iter().all(|c| c.bypassed));
    assert_eq!(proc_of(&r).cols.len(), 5);
    let mut o = opts(1024, 600, 7.0);
    o.cfg.bypassed_stages = "hide".into();
    let r = draw(&v, &o);
    assert!(text_runs(&r, "cols")
        .iter()
        .any(|t| t.0 == "CHAIN BYPASSED"));
    // multiband off alone: one grey `MB` column
    let mut v = proc_feed(5, &[-2.0; 5], 0b11101);
    v["stations"][0]["frames"][23]["stage_mask"] = json!(0b11101);
    let r = draw(&v, &opts(1024, 600, 7.0));
    let mb = col(&r, "multiband");
    assert!(mb.bypassed);
    assert_eq!(proc_of(&r).cols.len(), 5);
}

#[test]
fn band_names_fit_their_columns_or_fall_back_to_numbers() {
    for bands in [2usize, 3, 4, 5, 6] {
        let v = proc_feed(bands, &[-2.0; 6], 0b11111);
        let r = draw(&v, &opts(1024, 600, 7.0));
        let p = proc_of(&r);
        let labels: Vec<_> = p
            .cols
            .iter()
            .filter(|c| c.name.starts_with("band"))
            .map(|c| c.label.clone())
            .collect();
        assert_eq!(labels.len(), bands);
        let named = display::scene::proc::band_names(bands);
        assert!(
            labels == named || labels == (1..=bands).map(|i| format!("B{i}")).collect::<Vec<_>>(),
            "{labels:?}"
        );
        // every label is written, and inside its column
        for c in p.cols.iter().filter(|c| c.name.starts_with("band")) {
            let t = text_runs(&r, "cols");
            let run = t.iter().find(|t| t.0 == c.label).unwrap();
            assert!(
                c.rect.contains(&run.2),
                "{} {:?} in {:?}",
                c.label,
                run.2,
                c.rect
            );
        }
        if bands <= 5 {
            assert_eq!(labels, named, "{bands} bands have room for names at 1024");
        }
    }
    assert_eq!(display::scene::proc::hz_text(120.0), "120");
    assert_eq!(display::scene::proc::hz_text(2000.0), "2k");
    assert_eq!(display::scene::proc::hz_text(3500.0), "3.5k");
}

// ---- the level meters and the loudness controller ---------------------------------------------------------------------

#[test]
fn the_level_bars_read_the_frame_and_the_ceiling_marker_sits_at_the_limiters_ceiling() {
    let r = draw(&demo("proc").unwrap(), &opts(1024, 600, 7.0));
    let bars: Vec<_> = r
        .report
        .bars
        .iter()
        .filter(|b| b.zone.starts_with("proc."))
        .collect();
    assert_eq!(
        bars.iter().map(|b| b.zone.as_str()).collect::<Vec<_>>(),
        ["proc.in", "proc.L", "proc.R"]
    );
    // the output bars are the PPM of the frame (the default preset passes it through), the input is the AGC's input
    assert_eq!(bars[1].db, Some(-9.5));
    assert_eq!(bars[2].db, Some(-10.4));
    assert_eq!(bars[0].db, Some(-22.0));
    for b in &bars {
        let want =
            b.track.x + (((b.db.unwrap() + 60.0) / 60.0) * f64::from(b.track.w)).round() as i32;
        assert_eq!(b.fill_x, want, "{}", b.zone);
    }
    // the ceiling marker: an amber tick above and below the L and R bars at -1 dBTP, none on the input bar
    let ceil_x =
        |b: &display::scene::MeterBar| display::scene::db_x(b.track, -1.0).min(b.track.right() - 3);
    for b in &bars[1..] {
        assert_eq!(
            px(&r, ceil_x(b) + 1, b.track.y - 2),
            rgb(theme::AMBER),
            "{}",
            b.zone
        );
    }
    assert_ne!(
        px(&r, ceil_x(bars[0]) + 1, bars[0].track.y - 2),
        rgb(theme::AMBER)
    );
    assert_eq!(proc_of(&r).ceiling, Some(-1.0));
    assert!(text_runs(&r, "cols")
        .iter()
        .any(|t| t.0 == "CEIL -1.0 dBTP"));
    // a limiter that is off has no marker and no ceiling line
    let v = proc_feed(5, &[-2.0; 5], 0b10111);
    let r = draw(&v, &opts(1024, 600, 7.0));
    assert_eq!(proc_of(&r).ceiling, None);
    assert!(text_runs(&r, "cols")
        .iter()
        .all(|t| !t.0.starts_with("CEIL")));
}

#[test]
fn the_loudness_controller_shows_its_target_gain_state_and_numerals() {
    let want = [
        (0, "OFF"),
        (1, "CONVERGING"),
        (2, "ON TARGET"),
        (3, "FROZEN"),
        (4, "AT LIMIT"),
    ];
    for (state, label) in want {
        let mut v = proc_feed(5, &[-3.0; 5], 0b11111);
        let f = &mut v["stations"][0]["frames"][23];
        f["loudness_state"] = json!(state);
        f["gain_db"] = json!(3.9);
        if state == 0 {
            f["target"] = Value::Null;
        }
        let r = draw(&v, &opts(1024, 600, 7.0));
        let t: Vec<String> = text_runs(&r, "meters").into_iter().map(|t| t.0).collect();
        assert!(t.iter().any(|x| x == label), "{label}: {t:?}");
        assert_eq!(
            t.iter().any(|x| x == "+3.9 dB"),
            state != 0,
            "{label}: {t:?}"
        );
        if state != 0 {
            assert!(t.iter().any(|x| x == "TARGET -14.0   W -14.0"), "{t:?}");
        } else {
            assert!(t.iter().any(|x| x == "TARGET --   W -14.0"), "{t:?}");
        }
        assert!(t.iter().any(|x| x == "S -14.1   I -14.0"), "{t:?}");
    }
}

// ---- the header and the strip --------------------------------------------------------------------------------------

#[test]
fn the_header_says_which_station_what_state_and_what_is_on_air() {
    let r = draw(&demo("proc").unwrap(), &opts(1024, 600, 7.0));
    let h: Vec<String> = text_runs(&r, "header").into_iter().map(|t| t.0).collect();
    for want in ["Office", "PLAYING", "Eve", "16:21:14", "restarted"] {
        assert!(h.iter().any(|x| x == want), "{want} in {h:?}");
    }
    let n: Vec<String> = text_runs(&r, "now").into_iter().map(|t| t.0).collect();
    assert!(
        n.iter().any(|x| x == "But I Do Want To Know You (RC1)"),
        "{n:?}"
    );
    assert!(n.iter().any(|x| x == "0:38"), "{n:?}");
    // a critical alert turns the header red and its code is in the pill
    let mut v = demo("proc").unwrap();
    v["stations"][0]["snapshot"]["alerts"] =
        json!([{"id": 9, "code": "silence", "severity": "critical", "message": "x"}]);
    let r = draw(&v, &opts(1024, 600, 7.0));
    assert_eq!(px(&r, 700, 3), rgb(theme::RED_BG));
    assert!(text_runs(&r, "header").iter().any(|t| t.0 == "silence"));
    // a station that cannot be reached
    let mut v = demo("proc").unwrap();
    v["stations"][0]["connected"] = json!(false);
    let r = draw(&v, &opts(1024, 600, 7.0));
    assert!(text_runs(&r, "now").iter().any(|t| t.0 == "NO CONNECTION"));
    assert!(proc_of(&r).cols.is_empty());
    // a long title is cut inside the strip, with the time at the right still whole
    let mut v = demo("proc").unwrap();
    v["stations"][0]["snapshot"]["now"]["title"] =
        json!("Symphony of the Unquiet Hours in Seven Movements, Op. 12 (Unabridged Demo)");
    let r = draw(&v, &opts(1024, 600, 7.0));
    let strip = zones(&r).into_iter().find(|z| z.0 == "now").unwrap().1;
    for t in text_runs(&r, "now") {
        assert!(strip.contains(&t.2), "{} {:?}", t.0, t.2);
    }
    let t = text_runs(&r, "now");
    let title = t.iter().find(|t| t.0.starts_with("Symphony")).unwrap();
    let time = t.iter().find(|t| t.0 == "0:38").unwrap();
    assert!(
        title.2.right() < time.2.x,
        "title {:?} time {:?}",
        title.2,
        time.2
    );
}

// ---- the spectrum, off by default ----------------------------------------------------------------------------------------

#[test]
fn the_spectrum_is_off_by_default_and_never_drawn_where_there_is_no_room() {
    let v = demo("proc").unwrap();
    for (w, h, d) in CONFIGS {
        let r = draw(&v, &opts(w, h, d));
        assert!(
            zones(&r).iter().all(|z| z.0 != "spectrum"),
            "{w}x{h} default"
        );
        assert!(!proc_of(&r).spectrum);
        // asked for
        let mut o = opts(w, h, d);
        o.cfg.proc_spectrum = true;
        let r = draw(&v, &o);
        let zs = zones(&r);
        let t = proc_of(&r).t;
        let cols = zs.iter().find(|z| z.0 == "cols").unwrap().1;
        match zs.iter().find(|z| z.0 == "spectrum") {
            Some((_, s)) => {
                assert!(proc_of(&r).spectrum);
                assert!(s.h >= 4 * t, "{w}x{h}: spectrum {} px for text of {t}", s.h);
                assert!(cols.h >= 10 * t - 1, "{w}x{h}: the bars keep {} px", cols.h);
            }
            None => {
                // no room: what the bars would have left over the 10 text sizes they need is under 4
                assert!(
                    cols.h - 10 * t < 4 * t,
                    "{w}x{h}: had room ({} px of bars)",
                    cols.h
                );
                assert!(
                    r.report.notes.iter().any(|n| n.starts_with("spectrum")),
                    "{w}x{h}"
                );
            }
        }
        // still tiling and still legible with it
        let mut cover = vec![0u8; (w * h) as usize];
        for z in zs.iter().map(|z| z.1) {
            for y in z.y..z.bottom() {
                for x in z.x..z.right() {
                    cover[(y * w + x) as usize] += 1;
                }
            }
        }
        assert!(cover.iter().all(|&c| c == 1));
        assert!(r
            .canvas
            .log
            .glyphs
            .iter()
            .all(|g| g.size >= r.plan.floor_px));
    }
    // the 7-inch panel: no room, the note says so
    let mut o = opts(1024, 600, 7.0);
    o.cfg.proc_spectrum = true;
    let r = draw(&v, &o);
    assert!(zones(&r).iter().all(|z| z.0 != "spectrum"));
    assert!(r.report.notes.iter().any(|n| n.contains("no room")));
    // 1080p on 21.5 inches has it, and the spectrum shows the tone's band
    let mut o = opts(1920, 1080, 21.5);
    o.cfg.proc_spectrum = true;
    let r = draw(&v, &o);
    assert!(zones(&r).iter().any(|z| z.0 == "spectrum"));
}

// ---- ballistics ------------------------------------------------------------------------------------------------------

struct Rig {
    feed_now: i64,
    now: i64,
    p: Presenter,
    plan: display::layout::Plan,
    cfg: DisplayConfig,
    released: display::frame::Frame,
}

/// Run the stations on with frames of no reduction until `sec` seconds after the feed's time, draw, and return each
/// band's `(bar dB, tick row)`.
fn at(
    rig: &mut Rig,
    sec: f64,
    stations: &mut [StationModel],
    r: &mut Renderer,
) -> Vec<(f64, Option<i32>)> {
    use display::scene::SceneInput;
    let target = rig.feed_now + (sec * 1000.0) as i64;
    while rig.now < target {
        rig.now += 33;
        stations[0].apply_frame(rig.released.clone(), rig.now);
        let live = stations[0].live_frame(rig.now).cloned();
        stations[0].dynamics.update(live.as_ref(), rig.now);
    }
    let shown = rig.p.shown(stations, rig.now);
    assert!(matches!(shown, Shown::Processing { .. }));
    let known = std::collections::HashSet::<String>::new();
    let has = |u: &str| known.contains(u);
    let bar = display::scene::BarInfo::new("P", &rig.cfg);
    let inp = SceneInput {
        stations,
        shown,
        now_ms: rig.now,
        clock: "16:21:14".into(),
        tz_min: -240,
        product: "P".into(),
        cfg: &rig.cfg,
        plan: rig.plan,
        has_image: &has,
        notices: &[],
        bar: &bar,
    };
    r.frame(&inp);
    proc_of(r)
        .cols
        .iter()
        .filter(|c| c.name.starts_with("band"))
        .map(|c| (c.bar_db, c.hold_y))
        .collect()
}

#[test]
fn the_bars_release_at_the_gain_reduction_ballistics_and_the_tick_waits() {
    let mut v = proc_feed(5, &[-12.0; 5], 0b11111);
    // a history of one deep bite
    v["stations"][0]["frames"] = json!(vec![v["stations"][0]["frames"][23].clone(); 3]);
    let feed = feed_from(&v);
    let plan = display::layout::Plan::new(1024, 600, 7.0, 70.0);
    let mut released = feed.stations[0].frame.clone().unwrap();
    released.band_gr = [Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(0.0), None];
    let mut rig = Rig {
        feed_now: feed.now_ms,
        now: feed.now_ms,
        p: Presenter::new(ViewSettings {
            view: ViewMode::Processing,
            ..Default::default()
        }),
        plan,
        cfg: DisplayConfig::default(),
        released,
    };
    let mut stations = feed.stations.clone();
    let mut r = Renderer::new(plan);
    // right after the bite the bar is at 12 dB (the attack was instant) and the tick is up
    let a = at(&mut rig, 0.0, &mut stations, &mut r);
    assert!(
        a.iter()
            .all(|(b, h)| (*b - 12.0).abs() < 1e-9 && h.is_some()),
        "{a:?}"
    );
    // 15 dB/s release: after 0.5 s the bar is 12 - 7.5, and the tick is still where it was (it waits 1 s)
    let b = at(&mut rig, 0.5, &mut stations, &mut r);
    let el = (rig.now - rig.feed_now) as f64 / 1000.0;
    for ((bar, hold), (_, hold0)) in b.iter().zip(&a) {
        assert!((bar - (12.0 - 15.0 * el)).abs() < 0.05, "{el} s: {bar}");
        assert_eq!(hold, hold0, "the tick waits 1 s");
    }
    let c = at(&mut rig, 1.3, &mut stations, &mut r);
    for (bar, hold) in &c {
        assert_eq!(*bar, 0.0, "released");
        assert!(hold.is_some(), "the tick is falling");
    }
    let d = at(&mut rig, 2.6, &mut stations, &mut r);
    assert!(
        d.iter().all(|(_, h)| h.is_none()),
        "{d:?}: the tick has fallen to the bar and gone"
    );
    // the setting: a slower release keeps the bar up longer
    let mut stations2 = feed.stations.clone();
    for s in &mut stations2 {
        s.dynamics.params.gr_release_db_s = 4.0;
    }
    rig.now = rig.feed_now;
    let mut r2 = Renderer::new(plan);
    let e = at(&mut rig, 0.5, &mut stations2, &mut r2);
    let el = (rig.now - rig.feed_now) as f64 / 1000.0;
    assert!(
        e.iter().all(|(b, _)| (*b - (12.0 - 4.0 * el)).abs() < 0.05),
        "{e:?}"
    );
}

// ---- the views ----------------------------------------------------------------------------------------------------------

fn two() -> Vec<StationModel> {
    vec![
        StationModel::new("office", "office"),
        StationModel::new("eve", "eve"),
    ]
}

#[test]
fn the_processing_view_is_a_view_of_its_own_and_works_on_a_one_station_box() {
    assert_eq!(ViewMode::parse("processing"), Some(ViewMode::Processing));
    assert_eq!(ViewMode::Processing.name(), "processing");
    let mut p = Presenter::new(ViewSettings {
        view: ViewMode::Processing,
        station: Some("eve".into()),
        ..Default::default()
    });
    assert_eq!(
        p.shown(&two(), 0),
        Shown::Processing {
            idx: 1,
            other: Some(0),
            rotate: None,
            pinned: false
        }
    );
    let one = vec![StationModel::new("a", "a")];
    assert_eq!(
        p.shown(&one, 0),
        Shown::Processing {
            idx: 0,
            other: None,
            rotate: None,
            pinned: false
        }
    );
    // the library's payload carries it
    let s = ViewSettings::from_json(
        &json!({"view": "processing", "station": "office", "rotate_s": 20, "rotate_processing": true}),
        &ViewSettings::default(),
    );
    assert_eq!(s.view, ViewMode::Processing);
    assert!(s.rotate_processing);
    assert_eq!(
        ViewSettings::from_json(&s.to_json(), &ViewSettings::default()),
        s
    );
}

#[test]
fn rotate_can_include_each_stations_processing_view_and_still_pins_on_a_critical_alert() {
    let kind = |s: &Shown| match s {
        Shown::Single { idx, .. } => ("single", *idx),
        Shown::Processing { idx, .. } => ("processing", *idx),
        Shown::Split { .. } => ("split", 9),
    };
    // off: the rotation alternates between the stations only
    let mut p = Presenter::new(ViewSettings {
        view: ViewMode::Rotate,
        rotate_s: 20,
        ..Default::default()
    });
    let st = two();
    let seq: Vec<_> = (0..5).map(|i| kind(&p.shown(&st, i * 20_000))).collect();
    assert_eq!(
        seq,
        [
            ("single", 0),
            ("single", 1),
            ("single", 0),
            ("single", 1),
            ("single", 0)
        ]
    );
    // on: each station's normal view, then its processing view
    let mut p = Presenter::new(ViewSettings {
        view: ViewMode::Rotate,
        rotate_s: 20,
        rotate_processing: true,
        ..Default::default()
    });
    let seq: Vec<_> = (0..9).map(|i| kind(&p.shown(&st, i * 20_000))).collect();
    assert_eq!(
        seq,
        [
            ("single", 0),
            ("processing", 0),
            ("single", 1),
            ("processing", 1),
            ("single", 0),
            ("processing", 0),
            ("single", 1),
            ("processing", 1),
            ("single", 0)
        ]
    );
    // inside an interval the view holds, and the bar carries the fraction of it
    match p.shown(&st, 30_000) {
        Shown::Processing {
            idx: 0,
            rotate: Some(f),
            pinned: false,
            ..
        } => assert!((f - 0.5).abs() < 1e-9),
        x => panic!("{x:?}"),
    }
    // a critical alert on the hidden station pins the view to its normal view, whichever one the rotation was on
    let mut st = two();
    assert_eq!(kind(&p.shown(&st, 30_000)), ("processing", 0));
    st[1].alerts.push(Alert {
        id: 1,
        code: "silence".into(),
        severity: "critical".into(),
        message: "x".into(),
        raised_ms: None,
    });
    assert!(matches!(
        p.shown(&st, 31_000),
        Shown::Single {
            idx: 1,
            pinned: true,
            ..
        }
    ));
    st[1].alerts.clear();
    assert!(matches!(
        p.shown(&st, 32_000),
        Shown::Single { pinned: false, .. }
    ));
    // changing the setting starts the rotation over
    p.set(ViewSettings {
        view: ViewMode::Rotate,
        rotate_s: 20,
        rotate_processing: false,
        ..Default::default()
    });
    assert_eq!(kind(&p.shown(&st, 100_000)), ("single", 0));
    assert_eq!(kind(&p.shown(&st, 120_000)), ("single", 1));
}

#[test]
fn a_rotated_processing_view_shows_the_rotation_bar_and_the_pinned_one_is_red() {
    let mut v = demo("proc").unwrap();
    v["view"] = json!("rotate");
    v["rotate_processing"] = json!(true);
    v["rotate_s"] = json!(20);
    v["rotate_elapsed_ms"] = json!(30_000);
    let (r, shown) = render(&feed_from(&v), &{
        let mut o = opts(1024, 600, 7.0);
        o.view = None;
        o
    });
    assert!(
        matches!(
            shown,
            Shown::Processing {
                idx: 0,
                rotate: Some(_),
                ..
            }
        ),
        "{shown:?}"
    );
    // the bar is the bottom rows of the header, amber, half way
    let hdr = zones(&r).into_iter().find(|z| z.0 == "header").unwrap().1;
    let y = hdr.bottom() - 1;
    assert_eq!(px(&r, 10, y), rgb(theme::AMBER));
    assert_eq!(px(&r, 1000, y), rgb(theme::LINE));
    assert!((500..=520).contains(
        &(0..1024)
            .take_while(|&x| px(&r, x, y) == rgb(theme::AMBER))
            .count()
    ));
}

// ---- the mini processing strip of the compact views -----------------------------------------------------------------

fn mini_opts(w: i32, h: i32, d: f64, view: ViewMode, mode: &str) -> Options {
    let mut o = opts(w, h, d);
    o.view = Some(view);
    o.cfg.mini_processing = mode.into();
    o
}

#[test]
fn the_mini_strip_has_bars_that_match_the_frame_and_the_only_small_text_is_its_labels() {
    use display::scene::proc::MINI_LABELS;
    for (w, h, d) in [(1024, 600, 7.0), (800, 480, 9.0)] {
        for view in [ViewMode::Single, ViewMode::Split] {
            let r = draw(&demo("proc").unwrap(), &mini_opts(w, h, d, view, "auto"));
            let floor = r.plan.floor_px;
            // the drawn zones: office has a chain on and a strip, eve is bypassed and (auto) has none
            if w == 800 && view == ViewMode::Split {
                // the 800 x 480 half has 100 px for alarms: no room for a strip beside a warning, so none
                assert!(r.report.mini.is_empty());
                continue;
            }
            assert_eq!(r.report.mini.len(), 1, "{w}x{h} {view:?}");
            let (id, p) = &r.report.mini[0];
            assert_eq!(id, "office");
            assert_eq!(p.cols.len(), 9, "AGC, 5 bands, WB, LIM, CLP");
            let range = p.range_db;
            let gr = [-3.0, -6.0, -9.0, -6.0, -3.0, -1.2, -2.0, -0.5];
            for (c, g) in p.cols.iter().filter(|c| !c.centered).zip(gr) {
                let want = (-g / range).clamp(0.0, 1.0) * f64::from(c.track.h);
                assert!(
                    (f64::from(c.bar.h) - want).abs() <= 1.0,
                    "{w}x{h} {view:?} {}: {} vs {want:.2}",
                    c.name,
                    c.bar.h
                );
                assert_eq!(c.bar.y, c.track.y, "{} hangs from the top", c.name);
            }
            let agc = p.cols.iter().find(|c| c.centered).unwrap();
            assert!((f64::from(agc.bar.h) - 2.4 / 9.0 * f64::from(agc.track.h) / 2.0).abs() <= 1.0);
            // the tracks do not overlap, and they end above the labels
            for (i, a) in p.cols.iter().enumerate() {
                for b in &p.cols[i + 1..] {
                    assert!(!a.track.intersects(&b.track), "{} {}", a.name, b.name);
                }
            }
            // text: nothing under the floor except the listed labels, and those are small and few
            let mut small = 0;
            for g in &r.canvas.log.glyphs {
                if g.size < floor {
                    assert!(
                        MINI_LABELS.contains(&g.text.as_str()),
                        "{w}x{h} {view:?}: `{}` at {} px",
                        g.text,
                        g.size
                    );
                    small += 1;
                    // inside the zone it was drawn in
                    let z = r.zones.iter().find(|z| z.name == g.zone).unwrap().rect;
                    assert!(
                        z.contains(&g.bbox),
                        "`{}` {:?} outside {} {:?}",
                        g.text,
                        g.bbox,
                        g.zone,
                        z
                    );
                }
            }
            assert!(
                small >= 9 + 2,
                "{w}x{h} {view:?}: {small} labels (names and scale)"
            );
            // no two glyph boxes of a zone overlap, strip labels included
            let runs = &r.canvas.log.glyphs;
            for (i, a) in runs.iter().enumerate() {
                for b in &runs[i + 1..] {
                    assert!(
                        a.zone != b.zone
                            || a.bbox.w == 0
                            || b.bbox.w == 0
                            || !a.bbox.intersects(&b.bbox),
                        "{w}x{h} {view:?}: `{}` overlaps `{}`",
                        a.text,
                        b.text
                    );
                }
            }
            // the layout zones still tile the screen
            let mut cover = vec![0u8; (w * h) as usize];
            let lay = r.layout.as_ref().unwrap();
            for z in lay.zones.iter().map(|z| z.rect).chain(lay.divider) {
                for y in z.y..z.bottom() {
                    for x in z.x..z.right() {
                        cover[(y * w + x) as usize] += 1;
                    }
                }
            }
            assert!(cover.iter().all(|&c| c == 1));
        }
    }
}

#[test]
fn the_mini_strip_follows_its_setting_and_shows_dark_tracks_when_forced_on() {
    for (mode, want) in [("off", 0usize), ("auto", 1), ("on", 2)] {
        let r = draw(
            &demo("proc").unwrap(),
            &mini_opts(1024, 600, 7.0, ViewMode::Split, mode),
        );
        assert_eq!(r.report.mini.len(), want, "{mode}");
    }
    // on, with the chain off: every column dark, no bar
    let r = draw(
        &demo("both").unwrap(),
        &mini_opts(1024, 600, 7.0, ViewMode::Single, "on"),
    );
    let (_, p) = &r.report.mini[0];
    assert!(p.cols.iter().all(|c| c.bypassed && c.bar.h == 0));
    // auto with the chain off: none; and the essentials are all still there
    let r = draw(
        &demo("both").unwrap(),
        &mini_opts(1024, 600, 7.0, ViewMode::Single, "auto"),
    );
    assert!(r.report.mini.is_empty());
    let r = draw(
        &demo("proc").unwrap(),
        &mini_opts(1024, 600, 7.0, ViewMode::Single, "auto"),
    );
    let now: Vec<String> = r
        .canvas
        .log
        .glyphs
        .iter()
        .filter(|g| g.zone == "now")
        .map(|g| g.text.clone())
        .collect();
    assert!(
        now.iter().any(|t| t.starts_with("But I Do Want")) && now.iter().any(|t| t == "0:38"),
        "{now:?}"
    );
    // the strip lies right of the time left (it does not cover the digits)
    let digits = r
        .canvas
        .log
        .glyphs
        .iter()
        .find(|g| g.zone == "now" && g.text == "0:38")
        .unwrap()
        .bbox;
    let (_, p) = &r.report.mini[0];
    assert!(
        p.cols.iter().all(|c| c.track.x > digits.right()),
        "strip over the digits"
    );
}

// ---- screenshots (ignored: they write docs/screenshots/proc-view/) ---------------------------------------------------------------------------

#[test]
#[ignore]
fn write_the_screenshots() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screenshots/proc-view");
    std::fs::create_dir_all(&dir).unwrap();
    let gr = [-2.0, -6.0, -10.5, -4.0, -1.5, -0.5];
    let save = |name: &str, v: &Value, o: &Options| {
        let r = draw(v, o);
        r.canvas.save_png(&dir.join(name)).unwrap();
    };
    for (w, h, d, tag) in [(1024, 600, 7.0, "1024x600"), (800, 480, 9.0, "800x480")] {
        save(
            &format!("native-{tag}.png"),
            &demo("proc").unwrap(),
            &opts(w, h, d),
        );
        for bands in [2usize, 3, 6] {
            save(
                &format!("native-{tag}-{bands}bands.png"),
                &proc_feed(bands, &gr, 0b11111),
                &opts(w, h, d),
            );
        }
        save(
            &format!("native-{tag}-limiter-clipper-bypassed.png"),
            &proc_feed(5, &gr, 0b00111),
            &opts(w, h, d),
        );
        let mut o = opts(w, h, d);
        o.cfg.bypassed_stages = "hide".into();
        save(
            &format!("native-{tag}-hidden.png"),
            &proc_feed(5, &gr, 0b00111),
            &o,
        );
    }
    for (w, h, d, tag) in [(1024, 600, 7.0, "1024x600"), (800, 480, 9.0, "800x480")] {
        for (view, vn) in [(ViewMode::Single, "single"), (ViewMode::Split, "split")] {
            save(
                &format!("mini-{tag}-{vn}.png"),
                &demo("proc").unwrap(),
                &mini_opts(w, h, d, view, "auto"),
            );
        }
        let mut v = proc_feed(6, &gr, 0b00111);
        v["stations"][0]["frames"][23]["flags"] = json!(18);
        save(
            &format!("mini-{tag}-single-6bands-bypassed.png"),
            &v,
            &mini_opts(w, h, d, ViewMode::Single, "auto"),
        );
    }
    let mut o = opts(1920, 1080, 21.5);
    o.cfg.proc_spectrum = true;
    save("native-1920x1080-spectrum.png", &demo("proc").unwrap(), &o);
    save(
        "native-1280x720.png",
        &demo("proc").unwrap(),
        &opts(1280, 720, 21.5),
    );
}
