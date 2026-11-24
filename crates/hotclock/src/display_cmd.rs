//! `hotclock display`: the native status display (M7, architecture 11). Live it draws on the monitor through
//! KMS; with `--render-png` it draws one frame of a feed into a PNG and writes the log of what it drew, which
//! is how the layouts are checked without a panel.

use clap::Args;
use common::config::GlobalConfig;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Args)]
pub struct DisplayArgs {
    /// Draw one frame of `--feed` into this PNG and exit (no screen needed).
    #[arg(long, value_name = "FILE.png")]
    render_png: Option<PathBuf>,
    /// The size to draw at, `WIDTHxHEIGHT` (default: the mode from `display.mode`).
    #[arg(long, value_name = "WxH")]
    size: Option<String>,
    /// `single`, `split`, `rotate` or `processing`. With `--render-png`: the view drawn (default the feed's). Live: forces this
    /// view and ignores the library's, for measurements.
    #[arg(long)]
    view: Option<String>,
    /// The station `single` shows.
    #[arg(long)]
    station: Option<String>,
    /// `demo:NAME` (both, chain5, chain3, proc, alert, untagged, longtitle, gapless, disconnected) or a JSON feed file.
    #[arg(long, default_value = "demo:both")]
    feed: String,
    /// Write the draw log (zones, glyph runs with sizes, bars, the curve) as JSON here.
    #[arg(long, value_name = "FILE.json")]
    log: Option<PathBuf>,
    /// Override `display.diagonal_in`.
    #[arg(long)]
    diagonal_in: Option<f64>,
    /// Override `display.view_distance_cm`.
    #[arg(long)]
    view_distance_cm: Option<f64>,
    /// Override `display.processing_view` (`spectrum`, `curve`, `both`).
    #[arg(long)]
    processing_view: Option<String>,
    /// Override `display.spectrum_source` (`pre`, `post`, `both`).
    #[arg(long)]
    spectrum_source: Option<String>,
    /// Override `display.bypassed_stages` (`show`, `hide`).
    #[arg(long)]
    bypassed_stages: Option<String>,
    /// Override `display.proc_range_db`, the processing view's gain-reduction scale (6 to 30).
    #[arg(long)]
    proc_range_db: Option<f64>,
    /// Override `display.mini_processing` (`off`, `auto`, `on`).
    #[arg(long)]
    mini_processing: Option<String>,
    /// Override `display.proc_spectrum`: the spectrum in the processing view, where there is room.
    #[arg(long)]
    proc_spectrum: Option<bool>,
    /// Draw nothing: exit 0 when `display.enabled` is on (or the file cannot be read, so the display itself reports
    /// the error), 1 when it is off. The unit's `ExecCondition=`: an off switch keeps the display stopped whatever
    /// pulls the unit in (the boot generator, an `enable` made by hand, a restart).
    #[arg(long)]
    check_enabled: bool,
    /// The night color at this temperature (1900 to 6500 K), whatever the hour: with `--render-png`, the PNG is the
    /// frame as the panel would show it; live, the panel keeps this look (to judge it), ignoring `display.night_*`.
    #[arg(long, value_name = "K")]
    night_kelvin: Option<u32>,
    /// With `--night-kelvin`: the night dim, 0 to 50 percent.
    #[arg(long, value_name = "PERCENT", default_value_t = 0)]
    night_dim: u32,
}

pub fn run(a: DisplayArgs, global: &std::path::Path) -> ExitCode {
    if a.check_enabled {
        return match GlobalConfig::load_or_default(global) {
            Ok(g) if !g.display.enabled => {
                println!(
                    "hotclock display: display.enabled is off in {}: not starting",
                    global.display()
                );
                ExitCode::FAILURE
            }
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::SUCCESS
            }
        };
    }
    let mut g = match GlobalConfig::load_or_default(global) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if let Some(d) = a.diagonal_in {
        g.display.diagonal_in = d;
    }
    if let Some(d) = a.view_distance_cm {
        g.display.view_distance_cm = d;
    }
    if let Some(v) = &a.processing_view {
        g.display.processing_view.clone_from(v);
    }
    if let Some(v) = &a.spectrum_source {
        g.display.spectrum_source.clone_from(v);
    }
    if let Some(v) = &a.bypassed_stages {
        g.display.bypassed_stages.clone_from(v);
    }
    if let Some(v) = a.proc_range_db {
        g.display.proc_range_db = v;
    }
    if let Some(v) = &a.mini_processing {
        g.display.mini_processing.clone_from(v);
    }
    if let Some(v) = a.proc_spectrum {
        g.display.proc_spectrum = v;
    }
    let force_night = match a.night_kelvin {
        Some(k) if (1900..=6500).contains(&k) && a.night_dim <= 50 => {
            Some((f64::from(k), f64::from(a.night_dim) / 100.0))
        }
        Some(_) => {
            eprintln!("--night-kelvin is 1900 to 6500 and --night-dim 0 to 50");
            return ExitCode::from(2);
        }
        None => None,
    };
    let errs = g.display.validate(global);
    if !errs.is_empty() {
        eprintln!("{}", common::config::ConfigErrors(errs));
        return ExitCode::from(2);
    }
    match &a.render_png {
        Some(png) => match render_png(&a, &g, png, force_night) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("hotclock display: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            let view = match a.view.as_deref().map(display::view::ViewMode::parse) {
                Some(Some(v)) => Some(v),
                Some(None) => {
                    eprintln!("--view is single, split, rotate or processing");
                    return ExitCode::from(2);
                }
                None => None,
            };
            display::app::run(&g, global, view, force_night, crate::brand::DISPLAY_NAME)
        }
    }
}

fn render_png(
    a: &DisplayArgs,
    g: &GlobalConfig,
    png: &std::path::Path,
    night: Option<(f64, f64)>,
) -> Result<(), String> {
    let (w, h) = match &a.size {
        Some(s) => {
            let (w, h, _) =
                common::config::parse_mode(s).ok_or("--size must look like 1024x600")?;
            (w as i32, h as i32)
        }
        None => {
            let (w, h, _) =
                common::config::parse_mode(&g.display.mode).ok_or("bad display.mode")?;
            (w as i32, h as i32)
        }
    };
    let (feed_json, base) = match a.feed.strip_prefix("demo:") {
        Some(n) => (
            display::feed::demo(n).ok_or_else(|| format!("no demo feed `{n}`"))?,
            PathBuf::from("."),
        ),
        None => {
            let p = PathBuf::from(&a.feed);
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            (
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?,
                p.parent().map(PathBuf::from).unwrap_or_default(),
            )
        }
    };
    let feed = display::feed::Feed::from_json(&feed_json, &base)?;
    let view = a
        .view
        .as_deref()
        .map(|v| {
            display::view::ViewMode::parse(v).ok_or("--view is single, split, rotate or processing")
        })
        .transpose()?;
    let o = display::headless::Options {
        w,
        h,
        cfg: g.display.clone(),
        view,
        station: a.station.clone(),
        product: crate::brand::DISPLAY_NAME.into(),
    };
    let (r, _) = display::headless::render(&feed, &o);
    if r.plan.degraded {
        eprintln!(
            "hotclock display: warning: {w} x {h} on {} inches asks for a {} px minimum; the fallback layout draws 19 px, so this mode is degraded",
            g.display.diagonal_in, r.plan.min_px
        );
    }
    // the frame as the panel shows it: through the night table, the same pass as the live display's
    let lut = night.map_or_else(common::night::Lut::identity, |(k, d)| {
        common::night::Lut::for_look(k, d)
    });
    r.canvas.save_png_with(png, &lut)?;
    if let Some(l) = &a.log {
        std::fs::write(
            l,
            serde_json::to_string_pretty(&display::headless::log_json(&r))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("{}: {e}", l.display()))?;
    }
    println!(
        "wrote {} ({w} x {h}, {:?}, minimum {} px)",
        png.display(),
        r.plan.variant,
        r.plan.min_px
    );
    Ok(())
}
