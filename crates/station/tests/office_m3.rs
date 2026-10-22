//! M3 on the real office set (`HOTCLOCK_TESTDATA/office`): 30 virtual days of the office playlist with a
//! liner every 4 and the top-of-hour ID as a station block (window -60/+30 s), with the transition
//! planner's real overlaps and markers as the timing model, and the 10-minutes-ahead prediction of the next
//! ID. Skipped without the test data.

use common::timespec::{TimeSpec, Zone};
use common::transition::PresetBook;
use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{NewBlock, NewPlaylist};
use library::tracks::read_all_tracks;
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::catalog::Catalog;
use station::programming::Programming;
use station::runtime::RuntimeTiming;
use station::sched::{Origin, SchedConfig, Scheduler};
use station::sim::run_timed;

const MIN: i64 = 60_000;
const SEC: i64 = 1000;

fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|e| e == "flac") {
            out.push(p);
        }
    }
}

fn tone_item(lib: &Library, dir: &std::path::Path, name: &str, hz: f64, secs: f64, cat: &str) {
    let p = dir.join(name);
    render_tone(&ToneRender {
        out: p.clone(),
        rate: 48_000,
        seconds: secs,
        hz,
        level_dbfs: -20.0,
        format: WavFormat::F32,
    })
    .unwrap();
    match import_file(
        lib,
        &p,
        &ImportOptions {
            category: Some(cat.into()),
            source: "office".into(),
        },
    ) {
        ImportOutcome::Done { .. } => {}
        o => panic!("{o:?}"),
    }
}

#[test]
fn thirty_virtual_days_of_ids_on_the_real_office_set() {
    let Some(root) = std::env::var_os("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA not set");
        return;
    };
    let office = std::path::Path::new(&root).join("office");
    if !office.exists() {
        eprintln!("SKIPPED: no office set");
        return;
    }
    let dir = fixtures::testdir::process_dir().join(format!("office-m3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = Library::open(Paths::from_data_dir(&dir.join("data"))).unwrap();
    let mut fl = vec![];
    files(&office, &mut fl);
    let mut ids = vec![];
    for f in &fl {
        match import_file(
            &lib,
            f,
            &ImportOptions {
                category: Some("music".into()),
                source: "office".into(),
            },
        ) {
            ImportOutcome::Done { item_id, .. } | ImportOutcome::Duplicate { item_id } => {
                ids.push(item_id)
            }
            o => panic!("{}: {o:?}", f.display()),
        }
    }
    for i in 0..3 {
        tone_item(
            &lib,
            &dir,
            &format!("liner{i}.wav"),
            1000.0 + 100.0 * f64::from(i),
            8.0,
            "liner",
        );
        tone_item(
            &lib,
            &dir,
            &format!("id{i}.wav"),
            3000.0 + 100.0 * f64::from(i),
            6.0,
            "id",
        );
    }
    let liner = lib.category_id("liner").unwrap().unwrap();
    let id_cat = lib.category_id("id").unwrap().unwrap();
    let mut np = NewPlaylist::shuffle("Office");
    np.no_repeat_minutes = 60;
    let pl = lib.create_playlist(&np).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    lib.add_block_to_playlist(
        pl,
        &NewBlock::insert_every("liners", 4, serde_json::json!({"category_id": liner})),
    )
    .unwrap();
    let id_block = lib
        .add_station_block(
            "office",
            &NewBlock {
                name: "top of the hour".into(),
                kind: "insert".into(),
                trigger: "timed".into(),
                priority: 100,
                params: serde_json::json!({
                    "source": {"category_id": id_cat},
                    "when": {"kind": "recurrence", "cron": "0 * * * *"},
                    "mode": "soft_then_hard", "window_early_s": 60, "window_late_s": 30
                }),
            },
        )
        .unwrap();
    let mut cat = Catalog::new(&lib.paths.media);
    cat.apply(read_all_tracks(lib.conn()).unwrap());
    let prog = std::sync::Arc::new(Programming::from_rows(
        &lib.playlists().unwrap(),
        &library::playlists::read_station_blocks(lib.conn(), "office").unwrap(),
    ));
    let presets = PresetBook::builtin();
    let timing = RuntimeTiming {
        cat: &cat,
        presets: &presets,
        preset: "plexamp",
        rate: 48_000,
    };
    let zone = Zone::named("America/New_York").unwrap();
    let t0 = zone
        .compatible("2026-10-20T12:00:00".parse().unwrap())
        .unwrap();
    let until = t0 + 30 * 24 * 60 * MIN;
    let horizon_min: i64 = std::env::var("M3_HORIZON_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let cfg = SchedConfig {
        zone: zone.clone(),
        horizon_ms: horizon_min * MIN,
        ..SchedConfig::default()
    };
    let mut s = Scheduler::new(cfg, prog, 2026);
    s.start_program(pl, t0);
    let _ = TimeSpec::relative(0);
    let (mut plays, mut rows) = (Vec::new(), Vec::new());
    let (mut errs, mut samples) = (Vec::new(), 0usize);
    let mut now = t0;
    let mut next_sample_hour = 1i64;
    while now < until {
        let target = t0 + next_sample_hour * 60 * MIN;
        // Run to ten minutes before the next ID, ask the upcoming API, then to five minutes after it.
        let sample = target - 10 * MIN;
        let (p, _, n) = run_timed(
            &mut s,
            &cat,
            &timing,
            now,
            sample,
            usize::MAX,
            |_, _, _, _| {},
        );
        plays.extend(p);
        now = n.max(sample);
        let up = s.upcoming(sample, 1);
        let predicted = up
            .iter()
            .find(|u| u.block_id == id_block && u.target_ms == target)
            .map(|u| u.predicted_ms);
        let (p, _, n) = run_timed(
            &mut s,
            &cat,
            &timing,
            now,
            target + 5 * MIN,
            usize::MAX,
            |_, _, _, _| {},
        );
        let actual = p
            .iter()
            .find(|x| {
                x.entry
                    .timed
                    .as_ref()
                    .is_some_and(|t| t.target_ms == target)
            })
            .map(|x| x.start_ms);
        plays.extend(p);
        now = n.max(target + 5 * MIN);
        if let (Some(a), Some(b)) = (predicted, actual) {
            errs.push((a - b).abs());
        }
        samples += 1;
        next_sample_hour += 1;
        for r in &s.st.log.rows {
            if r.block_id == id_block
                && !rows
                    .iter()
                    .rev()
                    .take(200)
                    .any(|x: &station::timed::LogRow| x.target_ms == r.target_ms)
            {
                rows.push(r.clone());
            }
        }
    }
    let fired: Vec<_> = rows.iter().filter(|r| r.fired_ms.is_some()).collect();
    let soft = fired.iter().filter(|r| r.how == "soft").count();
    let devs: Vec<i64> = fired.iter().map(|r| r.deviation_ms.unwrap()).collect();
    let mut sorted = devs.clone();
    sorted.sort_unstable();
    let q = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize] as f64 / 1000.0;
    let within = errs.iter().filter(|e| **e <= 5 * SEC).count();
    let kinds: Vec<&str> = plays
        .iter()
        .map(|p| cat.meta(p.item_id()).map_or("", |m| m.kind.as_str()))
        .collect();
    let liner_after_id = kinds
        .windows(2)
        .filter(|w| w[0] == "id" && w[1] == "liner")
        .count();
    let rotation_relaxed = plays
        .iter()
        .filter(|p| p.entry.origin == Origin::Rotation && p.relaxed())
        .count();
    println!(
        "real office set, 30 days (plexamp preset): {} IDs fired of {} hours; soft {soft} ({:.1} percent); deviation s min/p5/p25/median/p75/p95/max = {:.0}/{:.0}/{:.0}/{:.0}/{:.0}/{:.0}/{:.0}; \
         every one inside [-60, +30] s: {}; liners right after an ID: {liner_after_id}; relaxed rotation plays {rotation_relaxed}; \
         prediction 10 min ahead within 5 s in {within} of {samples} ({:.1} percent), worst {} ms",
        fired.len(),
        samples,
        100.0 * soft as f64 / fired.len() as f64,
        q(0.0), q(0.05), q(0.25), q(0.5), q(0.75), q(0.95), q(1.0),
        devs.iter().all(|d| (-60 * SEC..=30 * SEC).contains(d)),
        100.0 * within as f64 / samples as f64,
        errs.iter().max().unwrap_or(&0)
    );
    for r in fired.iter().filter(|r| r.how == "hard").take(10) {
        let near: Vec<String> = plays
            .iter()
            .filter(|p| p.start_ms > r.target_ms - 12 * MIN && p.start_ms <= r.target_ms + 10 * MIN)
            .map(|p| {
                format!(
                    "{}[{}s]@{:+}s{}{}",
                    cat.meta(p.item_id())
                        .unwrap()
                        .title
                        .chars()
                        .take(10)
                        .collect::<String>(),
                    cat.meta(p.item_id()).unwrap().effective_ms / 1000,
                    (p.start_ms - r.target_ms) / SEC,
                    if p.entry.flags.iter().any(|f| f == "hard_time") {
                        "!"
                    } else {
                        ""
                    },
                    if p.entry.flags.iter().any(|f| f == "fitted") {
                        "*"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        println!(
            "HARD {} {} dev {} s: {}",
            zone.local_string(r.target_ms),
            r.how,
            r.deviation_ms.unwrap() / SEC,
            near.join(" | ")
        );
    }
    assert!(
        devs.iter().all(|d| (-60 * SEC..=30 * SEC).contains(d)),
        "outside the window"
    );
    assert!(100.0 * soft as f64 / fired.len() as f64 >= 95.0);
    assert_eq!(liner_after_id, 0);
    assert!(100.0 * within as f64 / samples as f64 >= 95.0);
}
