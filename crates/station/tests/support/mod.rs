//! Shared helpers for the M1.4 and M1.4a tests: fixtures, scenario rendering from YAML text, and
//! audio measurements that separate two overlapping voices by frequency (a demodulator over whole
//! cycles of both tones) or by differencing against a solo render at the same start frame.
#![allow(dead_code)]

use station::scenario::{render, Rendered, Scenario};
use std::path::{Path, PathBuf};

pub const RATE: u32 = 48_000;

/// The fixture directory with the named files, or `None` (with a message) without ffmpeg.
pub fn fx(names: &[&str]) -> Option<PathBuf> {
    let (dir, rep) = fixtures::shared_dir(names);
    if !rep.skipped.is_empty() {
        eprintln!("SKIPPED: fixtures not available: {:?}", rep.skipped);
        return None;
    }
    Some(dir)
}

pub fn run(yaml: &str, dir: &Path) -> Rendered {
    let sc = Scenario::from_yaml(yaml).unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    render(&sc, Some(dir)).unwrap_or_else(|e| panic!("render failed: {e}\n{yaml}"))
}

pub fn left(x: &[f32]) -> Vec<f32> {
    x.chunks_exact(2).map(|f| f[0]).collect()
}

pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

pub fn rms_db(x: &[f32]) -> f64 {
    if x.is_empty() {
        return -200.0;
    }
    let p: f64 = x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * p.max(1e-20).log10()
}

/// The amplitude of the component at `hz` in `x[start..start + win]`, by correlation with a
/// complex exponential. `win` must hold a whole number of cycles of every other tone present for
/// the others to cancel exactly.
pub fn tone_amp(x: &[f32], hz: f64, start: usize, win: usize) -> f64 {
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, &v) in x[start..start + win].iter().enumerate() {
        let ph = std::f64::consts::TAU * hz * (start + k) as f64 / f64::from(RATE);
        re += f64::from(v) * ph.cos();
        im += f64::from(v) * ph.sin();
    }
    2.0 * (re * re + im * im).sqrt() / win as f64
}

/// The audio of a file's left channel at 48 kHz (files here are 48 kHz), from frame 0.
pub fn decode_left(path: &Path) -> Vec<f32> {
    let mut v = Vec::new();
    let info = analysis::decode::decode_file(path, |s, ch, _| {
        v.extend(s.chunks_exact(ch).map(|f| f[0]));
    })
    .unwrap();
    assert_eq!(info.sample_rate, RATE, "{}", path.display());
    v
}

pub fn ms(frames: u64) -> f64 {
    frames as f64 * 1000.0 / f64::from(RATE)
}

pub fn frames(s: f64) -> u64 {
    (s * f64::from(RATE)).round() as u64
}

/// The sidecar voice named `name` (first one).
pub fn voice<'a>(r: &'a Rendered, name: &str) -> &'a station::sidecar::VoiceRecord {
    r.sidecar
        .voices
        .iter()
        .find(|v| v.item == name)
        .unwrap_or_else(|| panic!("no voice `{name}`"))
}

/// The scenario with only `item` queued, starting at `start_frame`: what that item alone sounds
/// like (same gain, same declick), for differencing against a two-item render.
pub fn solo_yaml(items: &str, item: &str, start_frame: u64, station: &str) -> String {
    format!(
        "station: {{ {station}, start_s: {} }}\nitems:\n{items}\nqueue:\n  - {{ item: {item} }}\n",
        start_frame as f64 / f64::from(RATE)
    )
}

pub fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| (a.get(i).copied().unwrap_or(0.0) - b.get(i).copied().unwrap_or(0.0)).abs())
        .fold(0.0, f32::max)
}

/// Maximum sample-to-sample step of `x`.
pub fn max_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// The runs of consecutive frames (both channels) below `thr_db` dBFS that are longer than
/// `min_frames`, as `(start, len)`. The level is the peak of a 1 ms sliding window, so a zero
/// crossing of a quiet tone does not open a run.
pub fn silent_runs(stereo: &[f32], thr_db: f64, min_frames: usize) -> Vec<(usize, usize)> {
    let thr = 10f64.powf(thr_db / 20.0) as f32;
    let l = left(stereo);
    let win = 48;
    let mut runs = vec![];
    let mut start: Option<usize> = None;
    let mut peak_ring = std::collections::VecDeque::<(usize, f32)>::new();
    for (i, &v) in l.iter().enumerate() {
        let a = v.abs();
        while peak_ring.back().is_some_and(|&(_, p)| p <= a) {
            peak_ring.pop_back();
        }
        peak_ring.push_back((i, a));
        while peak_ring.front().is_some_and(|&(j, _)| j + win <= i) {
            peak_ring.pop_front();
        }
        let peak = peak_ring.front().map_or(0.0, |p| p.1);
        if peak < thr {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            if i - s > min_frames {
                runs.push((s, i - s));
            }
        }
    }
    if let Some(s) = start {
        if l.len() - s > min_frames {
            runs.push((s, l.len() - s));
        }
    }
    runs
}

/// A 20-item scenario alternating music fixtures and the two liners. A silent run (below -60 dBFS,
/// longer than 50 ms) is dead air unless it lies inside a voice's own silence: some voice is
/// active for every frame of it, and every active voice's own signal there is below -60 dBFS too.
pub fn office_yaml(preset: &str) -> (String, Vec<&'static str>) {
    let music = [
        "tone_fade",
        "pickup",
        "cold_start",
        "album_t2",
        "quiet_track",
    ];
    let liners = ["liner_8s", "voice_short"];
    let mut items = String::new();
    for m in music {
        items.push_str(&format!("  {m}: {{ file: {m}.flac }}\n"));
    }
    for l in liners {
        items.push_str(&format!("  {l}: {{ file: {l}.flac, category: liner }}\n"));
    }
    let mut order = vec![];
    for i in 0..10 {
        order.push(music[i % music.len()]);
        order.push(liners[i % 2]);
    }
    let mut queue = String::new();
    for o in &order {
        queue.push_str(&format!("  - {{ item: {o} }}\n"));
    }
    (
        format!("station: {{ preset: {preset}, start_s: 0 }}\nitems:\n{items}queue:\n{queue}"),
        order,
    )
}

pub const OFFICE_FILES: [&str; 7] = [
    "tone_fade.flac",
    "pickup.flac",
    "cold_start.flac",
    "album_t2.flac",
    "quiet_track.flac",
    "liner_8s.flac",
    "voice_short.flac",
];

/// Frames of dead air in a render: silent runs no voice's own silence explains. Also used by
/// the M1.4a office test.
pub fn dead_air(r: &station::scenario::Rendered, dir: &std::path::Path) -> (usize, usize) {
    use std::collections::HashMap;
    let l = left(&r.samples);
    let mut src: HashMap<String, Vec<f32>> = HashMap::new();
    for v in &r.sidecar.voices {
        src.entry(v.path.clone())
            .or_insert_with(|| decode_left(std::path::Path::new(&v.path)));
    }
    let _ = dir;
    let runs = silent_runs(&r.samples, -60.0, 2_400);
    let first_start = r
        .sidecar
        .voices
        .iter()
        .map(|v| v.start_frame)
        .min()
        .unwrap() as usize;
    let mut bad = 0usize;
    let mut checked = 0usize;
    for (s, len) in &runs {
        if *s + *len <= first_start || *s + len >= l.len() {
            continue;
        }
        checked += 1;
        let mut f = *s;
        let mut ok = true;
        while f < s + len {
            let active: Vec<_> = r
                .sidecar
                .voices
                .iter()
                .filter(|v| {
                    v.started_frame.is_some_and(|st| st as usize <= f)
                        && v.ended_frame.is_some_and(|e| e as usize > f)
                })
                .collect();
            if active.is_empty() {
                ok = false;
                println!("    frame {f}: no voice is active");
                break;
            }
            for v in &active {
                // Inside the 3 ms declick at either end of a voice its own signal is faded on
                // purpose (a fade to silence starting from the cue-out's -47 dBFS is quiet).
                let (st, en) = (
                    v.started_frame.unwrap() as usize,
                    v.ended_frame.unwrap() as usize,
                );
                if f < st + 144 || f + 144 >= en {
                    continue;
                }
                // Inside a fade the executor was asked for (a stop fade, the fade in of `basic`)
                // the voice is quieter than its own signal on purpose.
                let stop_fade = v
                    .stop
                    .as_ref()
                    .is_some_and(|s| f >= s.at as usize && s.frames > 144);
                let fading_in = v
                    .envelope
                    .iter()
                    .any(|e| f >= e.start as usize && f < e.end as usize);
                if stop_fade || fading_in {
                    continue;
                }
                let g = 10f32.powf(v.gain_db as f32 / 20.0);
                let idx = (v.file_start_ms * 48) as usize + (f - v.start_frame as usize);
                let s_ = &src[&v.path];
                // The 1 ms window ending at this frame, as `silent_runs` measures the render.
                let w = &s_[idx.saturating_sub(47).min(s_.len())..(idx + 1).min(s_.len())];
                let peak = w.iter().fold(0.0f32, |m, x| m.max(x.abs())) * g;
                if db(f64::from(peak)) > -59.0 {
                    ok = false;
                    println!(
                        "    frame {f}: voice `{}` (started {}, ended {:?}) has its own signal at {:.1} dBFS",
                        v.item,
                        v.start_frame,
                        v.ended_frame,
                        db(f64::from(peak))
                    );
                }
            }
            if !ok {
                break;
            }
            f += 480;
        }
        if !ok {
            bad += 1;
            println!("  dead air: run of {len} frames at {s}");
        }
    }
    (bad, checked)
}
