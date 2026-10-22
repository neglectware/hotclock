//! `hotclock backup` and `hotclock restore` (M1.8, architecture 5.5).
//!
//! `backup` writes the nightly snapshots now, for `library.db` and every station's `state.db`, from a read
//! transaction: safe beside running services (it is what the services do themselves at 02:30). `restore`
//! puts the newest snapshot (or the one from `--date`) back in place of each live database. The services
//! must be stopped: it refuses while a station holds its lock or the library answers on its port, and it
//! does nothing without `--yes`.

use clap::Args;
use common::config::GlobalConfig;
use library::backup;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct BackupArgs {
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct RestoreArgs {
    /// Override `data_dir` of the global config.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// The snapshot date, `YYYYMMDD` (default: the newest of each database).
    #[arg(long)]
    date: Option<String>,
    /// Do it. Without this the command only lists what it would restore.
    #[arg(long)]
    yes: bool,
    /// Skip the check that the library service is not answering on its port (tests, and boxes that
    /// run the library on another port than the config says).
    #[arg(long)]
    no_service_check: bool,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `(snapshot name, live path)` for the library and every station directory that has a `state.db`.
fn databases(data_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v = vec![("library".to_string(), data_dir.join("library.db"))];
    let mut st: Vec<(String, PathBuf)> = std::fs::read_dir(data_dir.join("stations"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            (
                format!("station-{}", e.file_name().to_string_lossy()),
                e.path().join("state.db"),
            )
        })
        .collect();
    st.sort();
    v.extend(st);
    v
}

fn load(global: &Path, data_dir: &Option<PathBuf>) -> Result<(GlobalConfig, PathBuf), String> {
    let g = GlobalConfig::load_or_default(global).map_err(|e| e.to_string())?;
    let d = data_dir.clone().unwrap_or_else(|| g.data_dir.clone());
    Ok((g, d))
}

pub fn backup_now(a: BackupArgs, global: &Path) -> ExitCode {
    let (_, data) = match load(global, &a.data_dir) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("backup: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = data.join("backup");
    let (day, _) = common::localtime::local_day_minute(now_ms());
    let date = common::localtime::ymd(day);
    let mut failed = false;
    for (name, live) in databases(&data) {
        if !live.exists() {
            continue;
        }
        match backup::snapshot(&live, &dir, &name, &date, now_ms()) {
            Ok(i) if i.ok() => println!("{}: {} bytes, integrity ok", i.file, i.bytes),
            Ok(i) => {
                failed = true;
                eprintln!("{name}: failed its check: {}", i.check);
            }
            Err(e) => {
                failed = true;
                eprintln!("{name}: {e}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn restore(a: RestoreArgs, global: &Path) -> ExitCode {
    let (g, data) = match load(global, &a.data_dir) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("restore: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = data.join("backup");
    // What would be restored: the library and each station that has a snapshot.
    let mut plan: Vec<(String, PathBuf, PathBuf, String)> = vec![];
    let mut names: Vec<(String, PathBuf)> = databases(&data);
    // A station whose directory is gone (a wiped disk) is found from its snapshots.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let f = e.file_name().to_string_lossy().into_owned();
            if let Some(rest) = f.strip_prefix("station-") {
                if let Some((st, d)) = rest.rsplit_once('-') {
                    if d.ends_with(".db") && d.len() == 11 {
                        let name = format!("station-{st}");
                        if !names.iter().any(|(n, _)| *n == name) {
                            names.push((name, data.join("stations").join(st).join("state.db")));
                        }
                    }
                }
            }
        }
    }
    for (name, live) in names {
        let found = backup::list(&dir, &name);
        let pick = match &a.date {
            Some(d) => found.into_iter().find(|(x, _)| x == d),
            None => found.into_iter().next(),
        };
        match pick {
            Some((d, p)) => plan.push((name, live, p, d)),
            None => eprintln!("restore: no snapshot of {name} to restore; left alone"),
        }
    }
    if plan.is_empty() {
        eprintln!("restore: no snapshots in {}", dir.display());
        return ExitCode::FAILURE;
    }
    for (name, live, snap, date) in &plan {
        println!(
            "{name}: {} -> {} (snapshot of {date})",
            snap.display(),
            live.display()
        );
    }
    if !a.yes {
        println!("Nothing changed. Stop the services and run again with --yes.");
        return ExitCode::SUCCESS;
    }
    // The services must be stopped.
    let mut locks = vec![];
    for (name, live, _, _) in &plan {
        if name.starts_with("station-") && live.parent().is_some_and(Path::exists) {
            match station::state::lock_station(live) {
                Ok(f) => locks.push(f),
                Err(e) => {
                    eprintln!("restore: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    if !a.no_service_check {
        if let Ok(addr) = g.library.listen.parse::<std::net::SocketAddr>() {
            let probe = std::net::SocketAddr::from(([127, 0, 0, 1], addr.port()));
            if std::net::TcpStream::connect_timeout(&probe, std::time::Duration::from_millis(500))
                .is_ok()
            {
                eprintln!(
                    "restore: something answers on port {}: stop hotclock-library first",
                    addr.port()
                );
                return ExitCode::FAILURE;
            }
        }
    }
    let stamp = common::localtime::ymd(common::localtime::local_day_minute(now_ms()).0)
        + &format!("-{}", now_ms() / 1000 % 100_000);
    let mut failed = false;
    for (name, live, snap, _) in &plan {
        if let Err(e) = backup::restore(snap, live, &stamp) {
            eprintln!("restore: {name}: {e}");
            failed = true;
            continue;
        }
        own_like_parent(live);
        println!("restored {name}");
    }
    // A station's rows for the library (`library_outbox`) carry ids the library remembers having applied.
    // A restored file that starts numbering below that place would have its new rows skipped, so the
    // sequence is raised to the library's place.
    for (name, live, _, _) in &plan {
        if let Some(st) = name.strip_prefix("station-") {
            if let Err(e) = raise_outbox_sequence(&data.join("library.db"), live, st) {
                eprintln!("restore: {name}: outbox sequence: {e}");
            }
        }
    }
    drop(locks);
    if failed {
        ExitCode::FAILURE
    } else {
        println!("Start the services (systemctl start hotclock-library hotclock-station@...).");
        ExitCode::SUCCESS
    }
}

/// A file made by root during a restore belongs to the service user: the owner of its directory.
fn own_like_parent(p: &Path) {
    use std::os::unix::fs::MetadataExt;
    if let Some(m) = p.parent().and_then(|d| std::fs::metadata(d).ok()) {
        let _ = std::os::unix::fs::chown(p, Some(m.uid()), Some(m.gid()));
    }
}

fn raise_outbox_sequence(library_db: &Path, state_db: &Path, station: &str) -> Result<(), String> {
    let lib = match Connection::open_with_flags(
        library_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Ok(()), // no library yet: nothing remembered
    };
    let place: Option<String> = lib
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![format!("outbox:{station}")],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((inst, n)) = place.as_deref().and_then(|v| v.rsplit_once(':')) else {
        return Ok(());
    };
    let Ok(n) = n.parse::<i64>() else {
        return Ok(());
    };
    let st = Connection::open(state_db).map_err(|e| e.to_string())?;
    let mine: Option<String> = st
        .query_row("SELECT value FROM meta WHERE key='instance'", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    if mine.as_deref() != Some(inst) {
        return Ok(()); // a different file: the library starts from zero for it anyway
    }
    let cur: i64 = st
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM sqlite_sequence WHERE name = 'library_outbox'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if cur >= n {
        return Ok(());
    }
    let updated = st
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'library_outbox'",
            params![n],
        )
        .map_err(|e| e.to_string())?;
    if updated == 0 {
        st.execute(
            "INSERT INTO sqlite_sequence(name, seq) VALUES('library_outbox', ?1)",
            params![n],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
