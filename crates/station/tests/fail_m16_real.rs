//! M1.6 on real audio: the real office set (`HOTCLOCK_TESTDATA/office`, never mixed with the other sets)
//! played by a whole station for two virtual hours with every detector on, plus a RAM loop decoded from
//! real 44.1 kHz FLAC. A healthy library must raise no alert at all: no false silence, no false early end,
//! no false media-store failure, and nothing set by hand (no `silence_ok`: the deliberately quiet "From Silence
//! (RC1)" is judged on its own analysed quiet stretch). A genuinely dead item among the same tracks is still
//! skipped at 15 s. Skipped without the test data.

use engine::render::{render_tone, ToneRender};
use engine::wav::WavFormat;
use library::playlists::{Mode, NewBlock, NewPlaylist};
use library::{import_file, ImportOptions, ImportOutcome, Library, Paths};
use station::offline::Offline;
use station::runtime::{Op, Station, StationOptions};
use station::sched::At;
use std::sync::{Arc, Mutex};

const T0: i64 = 1_800_000_000_000;

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

/// Finished silent runs `(start frame, frames)` and the start of the one in progress.
type Runs = (Vec<(u64, u64)>, Option<u64>);

#[derive(Default)]
struct Quiet {
    run: u64,
    longest: u64,
    over_50ms: u64,
}

/// The office set imported into a fresh library, with three liners. `None` without the test data.
fn office_library(tag: &str) -> Option<(std::path::PathBuf, Library, Vec<i64>)> {
    let Some(root) = std::env::var_os("HOTCLOCK_TESTDATA") else {
        eprintln!("SKIPPED: HOTCLOCK_TESTDATA not set");
        return None;
    };
    let office = std::path::Path::new(&root).join("office");
    if !office.exists() {
        eprintln!("SKIPPED: no office set");
        return None;
    }
    let dir =
        fixtures::testdir::process_dir().join(format!("m16real-{tag}-{}", std::process::id()));
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
        let p = dir.join(format!("liner{i}.wav"));
        render_tone(&ToneRender {
            out: p.clone(),
            rate: 48_000,
            seconds: 8.0,
            hz: 1000.0 + 100.0 * f64::from(i),
            level_dbfs: -20.0,
            format: WavFormat::F32,
        })
        .unwrap();
        let _ = import_file(
            &lib,
            &p,
            &ImportOptions {
                category: Some("liner".into()),
                source: "office".into(),
            },
        );
    }
    Some((dir, lib, ids))
}

/// The office items with the longest quiet stretch the analysis found (1.5), under -50 dBFS at unity gain:
/// `(id, title, cue window ms, whole file ms)`, longest (whole file) first.
fn quiet_items(lib: &Library, ids: &[i64]) -> Vec<(i64, String, i64, i64)> {
    let mut v: Vec<(i64, String, i64, i64)> = ids
        .iter()
        .map(|id| {
            let e = lib.item_effective(*id).unwrap();
            let title: String = lib
                .conn()
                .query_row("SELECT title FROM items WHERE id = ?1", [id], |r| r.get(0))
                .unwrap();
            let p = analysis::quiet::QuietProfile::from_json(&e.quiet_profile).expect("a profile");
            (*id, title, e.longest_quiet_ms, p.longest_below(-50.0, true))
        })
        .collect();
    v.sort_by_key(|x| -x.3);
    v
}

#[test]
fn a_healthy_real_library_raises_no_alert_in_two_hours() {
    let Some((dir, lib, ids)) = office_library("healthy") else {
        return;
    };
    // Nothing is flagged by hand. "From Silence (RC1)" opens with a long stretch under -50 dBFS; the
    // analysis measures it and the detector gives the item that much (plus 5 s) before calling it dead.
    let quiet = quiet_items(&lib, &ids);
    for (id, title, cue, file) in quiet.iter().take(4) {
        println!(
            "real: longest stretch under -50 dBFS of {id} `{title}`: {:.1} s in the cue window, {:.1} s in the whole file",
            *cue as f64 / 1000.0,
            *file as f64 / 1000.0
        );
    }
    let flagged: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM items WHERE silence_ok = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(flagged, 0, "nothing is flagged silence_ok");
    let cid = lib.category_id("liner").unwrap().unwrap();
    let mut np = NewPlaylist::shuffle("Office");
    np.no_repeat_minutes = 60;
    let pl = lib.create_playlist(&np).unwrap();
    for i in &ids {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    lib.add_block_to_playlist(
        pl,
        &NewBlock::insert_every("liners", 4, serde_json::json!({"category_id": cid})),
    )
    .unwrap();
    // The emergency playlist: two of the same tracks, in order.
    let mut ep = NewPlaylist::shuffle("Emergency");
    ep.mode = Mode::Sequential;
    let epl = lib.create_playlist(&ep).unwrap();
    for i in ids.iter().take(2) {
        lib.add_item_to_playlist(epl, *i, 1.0).unwrap();
    }

    let mut o = StationOptions::new("office", lib.paths.media.clone());
    o.default_playlist = Some("Office".into());
    o.emergency_playlist = Some("Emergency".into());
    o.seed = 2026;
    o.ram_loop_seconds = 300;
    let (st, mixer) = Station::open(
        o,
        Some(dir.join("stations").join("office").join("state.db")),
        Some(lib.paths.db.clone()),
        T0,
    )
    .unwrap();
    let mut off = Offline::new(st, mixer, T0);
    off.keep_samples = false;
    let q = Arc::new(Mutex::new(Quiet::default()));
    let q2 = q.clone();
    off.sink = Some(Box::new(move |_t, b| {
        let mut q = q2.lock().unwrap();
        for f in b.chunks_exact(2) {
            if f[0].abs().max(f[1].abs()) < 1e-3 {
                q.run += 1;
            } else {
                if q.run > 2_400 {
                    q.over_50ms += 1;
                }
                q.longest = q.longest.max(q.run);
                q.run = 0;
            }
        }
    }));
    off.start().unwrap();
    let t = std::time::Instant::now();
    off.run_until(7200.0, &[]).unwrap();
    let ram = off
        .station
        .fail
        .ram
        .clone()
        .expect("the RAM loop loaded from real FLAC");
    let q = q.lock().unwrap();
    let al = off.station.alerts();
    println!(
        "real: 2 h of the office set ({} tracks) rendered in {:.0} s; {} plays; RAM loop {} items, {:.0} s, {:.1} MB; alerts raised {:?}; quiet runs over 50 ms {} (longest {:.0} ms)",
        ids.len(),
        t.elapsed().as_secs_f64(),
        off.station.as_run.len(),
        ram.items.len(),
        ram.seconds(48_000),
        ram.items.iter().map(|i| i.clip.samples.len() * 2).sum::<usize>() as f64 / 1e6,
        al.raised_total,
        q.over_50ms,
        q.longest as f64 / 48.0
    );
    for e in &al.history {
        println!(
            "  alert {:?} {} {}: {} (at {:.0} s)",
            e.change,
            e.alert.code,
            e.alert.key,
            e.alert.message,
            (e.at_ms - T0) as f64 / 1000.0
        );
    }
    for r in off
        .station
        .as_run
        .iter()
        .filter(|r| r.end_reason.as_deref() == Some("cut"))
    {
        println!(
            "  cut: {:?} {} at {:.0} s",
            r.item_id,
            r.title,
            (r.started_ms - T0) as f64 / 1000.0
        );
    }
    assert!(
        al.raised_total.is_empty(),
        "false alarm: {:?}",
        al.raised_total
    );
    assert!(al.history.is_empty());
    assert_eq!(off.station.health().mode, "normal");
    assert!(ram.seconds(48_000) <= 300.0);
    assert!(off.station.as_run.len() > 10);
}

/// The other half of deriving the window from the analysis: a genuinely dead item is still caught at the
/// profile's 15 s. Among the real office tracks, a 3-minute music item (a tone at import, so its analysis has
/// no quiet stretch) has its file replaced by digital silence of the same size (pre-flight passes). It is
/// played right after "From Silence (RC1)", the quietest real track, and once right before it, so both the
/// quiet tail going into it and the quiet opening coming out of it are exercised.
#[test]
fn a_dead_item_among_the_real_tracks_is_still_skipped_at_15_s() {
    let Some((dir, lib, ids)) = office_library("dead") else {
        return;
    };
    let quiet = quiet_items(&lib, &ids);
    let (fs, fs_title, _, fs_quiet) = quiet
        .iter()
        .find(|q| q.1.starts_with("From Silence"))
        .cloned()
        .unwrap_or_else(|| quiet[0].clone());
    println!(
        "real dead: `{fs_title}` ({fs}): {:.1} s under -50 dBFS in the whole file",
        fs_quiet as f64 / 1000.0
    );
    let src = dir.join("dead.wav");
    let secs = 180.0;
    render_tone(&ToneRender {
        out: src.clone(),
        rate: 48_000,
        seconds: secs,
        hz: 523.0,
        level_dbfs: -18.0,
        format: WavFormat::F32,
    })
    .unwrap();
    let dead = match import_file(
        &lib,
        &src,
        &ImportOptions {
            category: Some("music".into()),
            source: "office".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    };
    let sp: String = lib
        .conn()
        .query_row("SELECT store_path FROM items WHERE id = ?1", [dead], |r| {
            r.get(0)
        })
        .unwrap();
    let media = lib.paths.media.join(sp);
    let size = std::fs::metadata(&media).unwrap().len();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o644)).unwrap();
        let frames = (secs * 48_000.0) as usize;
        let mut wr = engine::wav::WavWriter::create(&media, 48_000, 2, WavFormat::F32).unwrap();
        wr.write_bytes(&vec![0u8; frames * 8]).unwrap();
        wr.finish().unwrap();
    }
    assert_eq!(std::fs::metadata(&media).unwrap().len(), size);
    let others: Vec<i64> = ids.iter().copied().filter(|i| *i != fs).collect();
    // From Silence, the dead item, then the rest; later the dead item and From Silence are inserted again,
    // in the other order.
    let order: Vec<i64> = [fs, dead]
        .into_iter()
        .chain(others.iter().copied())
        .collect();
    let mut np = NewPlaylist::shuffle("Office");
    np.mode = Mode::Sequential;
    np.no_repeat_minutes = 0;
    let pl = lib.create_playlist(&np).unwrap();
    for i in &order {
        lib.add_item_to_playlist(pl, *i, 1.0).unwrap();
    }
    let mut o = StationOptions::new("office", lib.paths.media.clone());
    o.default_playlist = Some("Office".into());
    o.seed = 7;
    let (st, mixer) = Station::open(
        o,
        Some(dir.join("stations").join("office").join("state.db")),
        Some(lib.paths.db.clone()),
        T0,
    )
    .unwrap();
    let mut off = Offline::new(st, mixer, T0);
    off.keep_samples = false;
    // The output's silent runs as the detector measures them (block mean square of both channels under
    // -50 dBFS): `(start frame, frames)`.
    let runs: Arc<Mutex<Runs>> = Arc::new(Mutex::new((vec![], None)));
    let r2 = runs.clone();
    off.sink = Some(Box::new(move |t, b| {
        let ms = b.iter().map(|v| v * v).sum::<f32>() / b.len().max(1) as f32;
        let mut g = r2.lock().unwrap();
        if ms < 1e-5 {
            if g.1.is_none() {
                g.1 = Some(t);
            }
        } else if let Some(s0) = g.1.take() {
            g.0.push((s0, t - s0));
        }
    }));
    off.start().unwrap();
    off.run_until(1200.0, &[]).unwrap();
    off.run_until(
        2400.0,
        &[
            (
                1200.0,
                Op::Insert {
                    item_id: dead,
                    at: At::Next,
                },
            ),
            (
                1201.0,
                Op::Insert {
                    item_id: fs,
                    at: At::Index(1),
                },
            ),
        ],
    )
    .unwrap();
    let rows = off.station.as_run.clone();
    for r in rows.iter().take(14) {
        println!(
            "real dead: {:>7.1} s  {:<40} played {:>6.1} s  {:?}",
            (r.started_ms - T0) as f64 / 1000.0,
            r.title,
            r.played_ms.unwrap_or(0) as f64 / 1000.0,
            r.end_reason
        );
    }
    let al = off.station.alerts();
    let dead_rows: Vec<_> = rows.iter().filter(|r| r.item_id == Some(dead)).collect();
    assert_eq!(dead_rows.len(), 2, "played twice, then quarantined");
    let runs = runs.lock().unwrap().0.clone();
    for r in &dead_rows {
        // The silence the listener heard: the run under -50 dBFS that the cut ended (it starts when the item
        // before went quiet under the crossfade, or when the dead item began).
        let cut_at = ((r.ended_ms.unwrap() - T0) as f64 * 48.0) as u64;
        let (s0, n) = *runs
            .iter()
            .find(|(s0, n)| *s0 <= cut_at && cut_at <= s0 + n + 48_000)
            .expect("a silent run up to the cut");
        let before_cut = cut_at.saturating_sub(s0) as f64 / 48_000.0;
        println!(
            "real dead: the dead item began at {:.1} s; the silence began at {:.1} s and the next item started {before_cut:.2} s later (limit 15 +- 0.5 s; the whole quiet run was {:.2} s, the next item's own quiet opening included)",
            (r.started_ms - T0) as f64 / 1000.0,
            s0 as f64 / 48_000.0,
            n as f64 / 48_000.0
        );
        assert!((14.5..=15.5).contains(&before_cut), "{before_cut}");
        assert_eq!(r.end_reason.as_deref(), Some("cut"));
    }
    // Every silence alert is the dead item's; nothing else was cut; the escalation never started.
    for e in al.history.iter().filter(|e| e.alert.code == "silence") {
        assert_eq!(e.alert.item_id.or(Some(dead)), Some(dead), "{e:?}");
        assert_ne!(
            e.alert.severity,
            station::alerts::Severity::Critical,
            "{e:?}"
        );
    }
    let fs_rows: Vec<_> = rows.iter().filter(|r| r.item_id == Some(fs)).collect();
    assert!(fs_rows.len() >= 2);
    for r in rows.iter().filter(|r| r.item_id != Some(dead)) {
        assert_ne!(r.end_reason.as_deref(), Some("cut"), "{} was cut", r.title);
    }
    println!(
        "real dead: alerts raised {:?}; From Silence played {} times, never cut",
        al.raised_total,
        fs_rows.len()
    );
    assert_eq!(al.count_raised("silence"), 2);
    assert_eq!(al.count_raised("item_quarantined"), 1);
    assert_eq!(off.station.health().mode, "normal");
}
