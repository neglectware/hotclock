//! A rough cost measurement of the renderer (run with `cargo test --release -p display --test bench -- --ignored
//! --nocapture`): the time per 30 Hz frame in each view, with the meters moving and the clock running. The CPU
//! budget itself is measured on the target box (M7 acceptance 9); this finds what costs.

use common::config::DisplayConfig;
use display::feed::{demo, Feed};
use display::layout::Plan;
use display::render::Renderer;
use display::scene::SceneInput;
use display::view::{Presenter, ViewMode, ViewSettings};
use std::time::Instant;

fn bench(w: i32, h: i32, diag: f64, view: ViewMode, feed_name: &str) {
    let feed = Feed::from_json(&demo(feed_name).unwrap(), std::path::Path::new(".")).unwrap();
    let cfg = DisplayConfig {
        diagonal_in: diag,
        ..DisplayConfig::default()
    };
    let plan = Plan::new(w, h, diag, 70.0);
    let mut r = Renderer::new(plan);
    let known: std::collections::HashSet<String> =
        feed.art.iter().map(|(u, _)| u.clone()).collect();
    for (u, img) in &feed.art {
        r.canvas.images.insert(u, img.clone());
    }
    let has = |u: &str| known.contains(u);
    let mut p = Presenter::new(ViewSettings {
        view,
        ..Default::default()
    });
    let mut stations = feed.stations.clone();
    let n = 300;
    let (mut t_total, mut px_total) = (0.0, 0u64);
    for i in 0..n {
        let now = feed.now_ms + i as i64 * 33;
        for s in &mut stations {
            let mut f = s.frame.clone().unwrap();
            f.ppm = [
                Some(-20.0 + (i % 17) as f64 * 0.5),
                Some(-21.0 + (i % 13) as f64 * 0.5),
            ];
            f.post[i % 64] = -30.0;
            // the processing view's bars move with the program: every band and block bites by a different, changing amount
            if f.band_count > 0 {
                for b in 0..usize::from(f.band_count).min(6) {
                    f.band_gr[b] = Some(-(1.0 + ((i + 5 * b) % 13) as f64));
                }
                f.stages[2].gr = Some(-(((i + 3) % 7) as f64) * 0.5);
                f.stages[3].gr = Some(-(((i + 1) % 11) as f64) * 0.4);
                f.stages[0].gr = Some(((i % 19) as f64 - 9.0) * 0.5);
            }
            s.apply_frame(f, now);
            let live = s.live_frame(now).cloned();
            s.dynamics.update(live.as_ref(), now);
        }
        let shown = p.shown(&stations, now);
        let bar = display::scene::BarInfo::new("Product", &cfg);
        let inp = SceneInput {
            stations: &stations,
            shown,
            now_ms: now,
            clock: "16:21:14".into(),
            tz_min: -240,
            product: "Product".into(),
            cfg: &cfg,
            plan,
            has_image: &has,
            notices: &[],
            bar: &bar,
        };
        let t = Instant::now();
        let d = r.frame(&inp);
        t_total += t.elapsed().as_secs_f64();
        px_total += d.iter().map(|r| (r.w * r.h) as u64).sum::<u64>();
    }
    println!(
        "{w}x{h} {view:?} {feed_name}: {:.2} ms per frame, {:.0} kpx damaged per frame (30 Hz: {:.1} % of a core on this machine)",
        t_total / n as f64 * 1000.0,
        px_total as f64 / n as f64 / 1000.0,
        t_total / n as f64 * 30.0 * 100.0
    );
}

#[test]
#[ignore]
fn frame_costs() {
    bench(1024, 600, 7.0, ViewMode::Single, "both");
    bench(1024, 600, 7.0, ViewMode::Split, "both");
    bench(800, 480, 9.0, ViewMode::Single, "both");
    bench(1920, 1080, 21.5, ViewMode::Single, "chain5");
    bench(1280, 720, 21.5, ViewMode::Single, "chain5");
    // the mini strip in single and split (the demo's office has the chain on)
    bench(1024, 600, 7.0, ViewMode::Single, "proc");
    bench(1024, 600, 7.0, ViewMode::Split, "proc");
    // the processing view (11.2c): the bars move every frame
    bench(1024, 600, 7.0, ViewMode::Processing, "proc");
    bench(800, 480, 9.0, ViewMode::Processing, "proc");
    bench(1920, 1080, 21.5, ViewMode::Processing, "proc");
}
