//! `hotclock import`, `hotclock library`, `hotclock analyze`, `hotclock reanalyze` and `hotclock ramp-fit` (M1.3).

use analysis::census;
use analysis::markers::ProfileParams;
use clap::Args;
use library::{import::import_many, ImportOptions, ImportOutcome, Library, Paths};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn paths_for(global: &Path, data_dir: &Option<PathBuf>) -> Result<Paths, String> {
    if let Some(d) = data_dir {
        return Ok(Paths::from_data_dir(d));
    }
    common::config::GlobalConfig::load_or_default(global)
        .map(|c| Paths::from_config(&c))
        .map_err(|e| e.to_string())
}

#[derive(Args)]
pub struct ImportArgs {
    /// Files, directories (searched recursively) or `.m3u` lists.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Category for every file (a category name; default `music`).
    #[arg(long)]
    category: Option<String>,
    /// Worker threads.
    #[arg(long, default_value_t = 2)]
    workers: usize,
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

pub fn import(a: ImportArgs, global: &Path) -> ExitCode {
    let paths = match paths_for(global, &a.data_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("import: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Create the database and directories first (without touching upload-tmp).
    if let Err(e) = Library::open_worker(paths.clone()) {
        eprintln!("import: cannot open the library: {e}");
        return ExitCode::FAILURE;
    }
    let mut files = vec![];
    for p in &a.paths {
        match census::collect_files(p) {
            Ok(mut f) => files.append(&mut f),
            Err(e) => {
                eprintln!("import: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let jobs: Vec<_> = files
        .into_iter()
        .map(|f| {
            (
                f,
                ImportOptions {
                    category: a.category.clone(),
                    source: "cli".into(),
                },
            )
        })
        .collect();
    let n = jobs.len();
    let t0 = Instant::now();
    let results = import_many(&paths, jobs, a.workers);
    let secs = t0.elapsed().as_secs_f64();
    let (mut done, mut dup, mut failed) = (0, 0, 0);
    for (p, o) in &results {
        match o {
            ImportOutcome::Done {
                item_id,
                warnings,
                possible_duplicate,
            } => {
                done += 1;
                println!("imported  {}  item {item_id}", p.display());
                for w in warnings {
                    println!("  warning: {w}");
                }
                if let Some(d) = possible_duplicate {
                    println!("  possible duplicate of item {d}");
                }
            }
            ImportOutcome::Duplicate { item_id } => {
                dup += 1;
                println!("duplicate {}  is item {item_id}", p.display());
            }
            ImportOutcome::Failed { error } => {
                failed += 1;
                println!("FAILED    {error}");
            }
        }
    }
    println!(
        "{n} files: {done} imported, {dup} duplicate, {failed} failed in {secs:.1} s ({:.2} files/s, {} workers)",
        n as f64 / secs.max(1e-9),
        a.workers
    );
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[derive(Args)]
pub struct LibraryArgs {
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Import workers.
    #[arg(long, default_value_t = 2)]
    workers: usize,
}

/// The library service (M1.7): the API on `library.listen`, the notifier, the inbox watcher and the
/// station outboxes.
pub fn library(a: LibraryArgs, global_path: &Path) -> ExitCode {
    let global = match common::config::GlobalConfig::load_or_default(global_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let paths = match &a.data_dir {
        Some(d) => Paths::from_data_dir(d),
        None => Paths::from_config(&global),
    };
    let lib = match Library::open(paths.clone()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("library: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        "library open: {} (revision {})",
        paths.db.display(),
        lib.revision().unwrap_or(-1)
    );
    drop(lib);
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = Arc::clone(&stop);
    let _ = ctrlc::set_handler(move || s2.store(true, Ordering::Relaxed));

    let product = crate::brand::display_name(&global).to_string();
    let api = serve::library_http::LibraryApi::new(serve::library_http::LibraryOptions {
        paths: paths.clone(),
        product: product.clone(),
        stations_dir: global.stations_dir.clone(),
        secrets: global.secrets_file.clone(),
        global_config: global_path.to_path_buf(),
        version: crate::brand::version(),
        time_zone: global.time_zone.clone(),
    });
    // This box's own ports, for the `Origin` check (13).
    let mut ports: Vec<u16> = vec![];
    if let Ok(x) = global.library.listen.parse::<std::net::SocketAddr>() {
        ports.push(x.port());
    }
    for (_, _, listen) in api.stations() {
        if let Ok(x) = listen.parse::<std::net::SocketAddr>() {
            ports.push(x.port());
        }
    }
    let sec = Arc::new(serve::security::Security::new(&global.allowed_hosts, ports));
    let mut server = match serve::http::Server::start(
        &global.library.listen,
        Arc::new(serve::library_http::LibraryHandler { api: api.clone() }),
        sec,
    ) {
        Ok(s) => {
            tracing::info!("library API on http://{}", s.addr);
            Some(s)
        }
        Err(e) => {
            eprintln!("library: cannot listen on {}: {e}", global.library.listen);
            return ExitCode::FAILURE;
        }
    };
    // The notifier (12.1) and the revision watcher that feeds the `library` topic.
    let notifier = serve::notify::Notifier::new(
        paths.db.clone(),
        paths
            .db
            .parent()
            .map(|d| d.join("stations"))
            .unwrap_or_default(),
        global.secrets_file.clone(),
        product,
    );
    let sent = notifier.sent.clone();
    let st = stop.clone();
    let nt = std::thread::spawn(move || notifier.run(st));
    let (api2, st2, paths2) = (api.clone(), stop.clone(), paths.clone());
    let rt = std::thread::spawn(move || {
        let mut last = -1i64;
        while !st2.load(Ordering::Relaxed) {
            if let Ok(l) = Library::open_worker(paths2.clone()) {
                let rev = l.revision().unwrap_or(-1);
                if rev != last {
                    if last >= 0 {
                        api2.hub.send_with("library", &|seq| {
                            serde_json::json!({"v": 1, "type": "library", "seq": seq, "data": {"revision": rev}}).to_string()
                        });
                    }
                    last = rev;
                }
                api2.notifications_sent
                    .store(sent.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
    // The nightly snapshot of library.db (5.5).
    let (paths3, st3) = (paths.clone(), stop.clone());
    let bt = std::thread::spawn(move || nightly_snapshot(&paths3, &st3));
    let result = library::inbox::run(&paths, a.workers, stop.clone());
    stop.store(true, Ordering::Relaxed);
    if let Some(mut s) = server.take() {
        s.stop();
    }
    let _ = nt.join();
    let _ = rt.join();
    let _ = bt.join();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("library: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Once per local day, from 02:30 (and at the first check after a start past 02:30 if tonight's is not
/// there): `VACUUM INTO` a checked snapshot of `library.db` (5.5). Wakes every 20 s to look at the clock.
fn nightly_snapshot(paths: &Paths, stop: &AtomicBool) {
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    };
    let mut done_day: Option<i64> = None;
    while !stop.load(Ordering::Relaxed) {
        let wall = now();
        let (day, minute) = common::localtime::local_day_minute(wall);
        if done_day.is_none() {
            let today = library::backup::read_status(&paths.backup, "library")
                .is_some_and(|i| i.date == common::localtime::ymd(day) && i.ok());
            if today {
                done_day = Some(day);
            }
        }
        if done_day != Some(day) && minute >= library::backup::AT_MINUTE {
            done_day = Some(day);
            match library::backup::snapshot(
                &paths.db,
                &paths.backup,
                "library",
                &common::localtime::ymd(day),
                wall,
            ) {
                Ok(i) if i.ok() => {
                    tracing::info!("snapshot {}: {} bytes, integrity ok", i.file, i.bytes)
                }
                Ok(i) => tracing::error!("the library snapshot failed its check: {}", i.check),
                Err(e) => tracing::error!("the library snapshot failed: {e}"),
            }
            // The snapshot read the whole database through memory glibc would keep (soak 2: +6.1 MB, once).
            crate::heap::trim();
        }
        for _ in 0..20 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

#[derive(Args)]
pub struct AnalyzeArgs {
    file: PathBuf,
    /// Segue profile: music, liner, ambient, longform.
    #[arg(long, default_value = "music")]
    profile: String,
}

pub fn analyze(a: AnalyzeArgs) -> ExitCode {
    let Some(profile) = ProfileParams::builtin(&a.profile) else {
        eprintln!("analyze: unknown profile `{}`", a.profile);
        return ExitCode::from(2);
    };
    match analysis::analyze::analyze_file(&a.file, &profile) {
        Ok(r) => {
            let e = &r.auto;
            let quiet =
                analysis::quiet::QuietProfile::measure(&r.env.e50, e.cue_in, e.cue_out, r.file_ms);
            let out = serde_json::json!({
                "file": a.file,
                "file_ms": r.file_ms,
                "sample_rate": r.info.sample_rate,
                "channels": r.info.channels,
                "l_int": r.l_int, "lra": r.lra, "true_peak": r.true_peak, "sample_peak": r.sample_peak,
                "cue_in": e.cue_in, "attack": e.attack, "segue": e.segue, "cue_out": e.cue_out,
                "flags": e.flags.iter().map(|f| f.name()).collect::<Vec<_>>(),
                "ramp_start": r.ramp_start, "ramp_end": r.ramp_end,
                // The `truncated_fade` detector: the last 500 ms with sound, and the fall over the 10 s before.
                "tail": {"end_db": r.tail.end_db, "plunge_db": r.tail.plunge_db, "drop_db": r.tail.drop_db, "slope_db_s": r.tail.slope_db_s,
                         "truncated_fade": r.tail.truncated},
                // The longest stretch under -50 dBFS (1.5: the silence detector's window is at least this plus 5 s).
                "longest_quiet_ms": quiet.longest_below(-50.0, false),
                "longest_quiet_ms_whole_file": quiet.longest_below(-50.0, true),
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("analyze: {e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Args)]
pub struct ReanalyzeArgs {
    /// Item ids to analyze again.
    items: Vec<i64>,
    /// Every item whose analysis an older version of the analysis made (for example before the
    /// `truncated_fade` flag existed).
    #[arg(long)]
    stale: bool,
    /// Every item of the library.
    #[arg(long)]
    all: bool,
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

/// `hotclock reanalyze`: decode the stored files again and replace their analysis and effective markers. The
/// overrides stay. A failure keeps the old analysis (`Library::reanalyze_item`).
pub fn reanalyze(a: ReanalyzeArgs, global: &Path) -> ExitCode {
    let lib = match paths_for(global, &a.data_dir)
        .and_then(|p| Library::open_worker(p).map_err(|e| e.to_string()))
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!("reanalyze: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ids: Vec<i64> = if a.all {
        match lib
            .conn()
            .prepare("SELECT id FROM items WHERE deleted_at IS NULL ORDER BY id")
            .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect())
        {
            Ok(ids) => ids,
            Err(e) => {
                eprintln!("reanalyze: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else if a.stale {
        match lib.stale_analysis_ids() {
            Ok(ids) => ids,
            Err(e) => {
                eprintln!("reanalyze: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        a.items.clone()
    };
    if ids.is_empty() && !a.stale && !a.all {
        eprintln!("reanalyze: give item ids, --stale or --all");
        return ExitCode::from(2);
    }
    let t0 = Instant::now();
    let (mut done, mut failed) = (0, 0);
    for id in &ids {
        match lib.reanalyze_item(*id, None) {
            Ok(library::health::Reanalysis::Updated) => {
                done += 1;
                let flags = lib
                    .item_effective(*id)
                    .map(|e| {
                        e.flags
                            .iter()
                            .map(|f| f.name())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                println!("analyzed  item {id}  {flags}");
            }
            Ok(library::health::Reanalysis::KeptOld { error }) => {
                failed += 1;
                println!("FAILED    item {id}: {error} (the old analysis is kept)");
            }
            Err(e) => {
                failed += 1;
                println!("FAILED    item {id}: {e}");
            }
        }
    }
    println!(
        "{} items: {done} analyzed, {failed} failed in {:.1} s",
        ids.len(),
        t0.elapsed().as_secs_f64()
    );
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[derive(Args)]
pub struct RampFitArgs {
    /// The test-data root (holds office/, preshow/, elevator/ and plexamp-calibration/).
    /// Default: `$HOTCLOCK_TESTDATA`.
    #[arg(long)]
    testdata: Option<PathBuf>,
}

pub fn ramp_fit(a: RampFitArgs) -> ExitCode {
    let root = a
        .testdata
        .or_else(|| std::env::var_os("HOTCLOCK_TESTDATA").map(PathBuf::from));
    let Some(root) = root else {
        eprintln!("ramp-fit: give --testdata or set HOTCLOCK_TESTDATA");
        return ExitCode::from(2);
    };
    match analysis::ramp_fit::run(&root) {
        Some(r) => {
            print!("{}", analysis::ramp_fit::format_report(&r));
            ExitCode::SUCCESS
        }
        None => {
            eprintln!(
                "ramp-fit: no calibration data or no matching local files under {}",
                root.display()
            );
            ExitCode::FAILURE
        }
    }
}
