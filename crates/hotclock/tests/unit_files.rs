//! The packaged systemd units: a station that is sandboxed with `ProtectSystem=strict` can write only what the unit
//! lists. The web UI writes the station's configuration file (and, around it, a temporary file and a `.bak`), so the
//! unit must cover that directory: it did not, and every Processing change failed with "Read-only file system".

use std::path::{Path, PathBuf};

fn unit(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/systemd")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn setting(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix(&format!("{key}=")))
        .flat_map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

fn covered(paths: &[String], p: &Path) -> bool {
    paths
        .iter()
        .any(|w| p.starts_with(w.trim_start_matches('-')))
}

#[test]
fn the_station_unit_can_write_every_path_the_station_writes() {
    let u = unit("hotclock-station@.service");
    assert!(setting(&u, "ProtectSystem").contains(&"strict".to_string()));
    let rw = setting(&u, "ReadWritePaths");
    // The configuration file named on the command line: written atomically, so its directory must be writable.
    let exec = setting(&u, "ExecStart");
    let cfg = exec
        .iter()
        .position(|a| a == "--config")
        .map(|i| exec[i + 1].clone())
        .expect("--config in ExecStart");
    let cfg = PathBuf::from(cfg.replace("%i", "office"));
    for (what, p) in [
        ("the configuration file", cfg.clone()),
        (
            "its temporary file",
            cfg.with_file_name(".office.toml.tmp-1"),
        ),
        ("its .bak copy", cfg.with_file_name("office.toml.bak")),
        (
            "the station's state",
            PathBuf::from("/var/lib/hotclock/stations/office/state.db"),
        ),
        ("the backups", PathBuf::from("/var/lib/hotclock/backup/x")),
    ] {
        let rw: Vec<String> = rw.iter().map(|w| w.replace("%i", "office")).collect();
        let state = setting(&u, "StateDirectory")
            .iter()
            .map(|s| format!("/var/lib/{}", s.replace("%i", "office")))
            .collect::<Vec<_>>();
        assert!(
            covered(&rw, &p) || covered(&state, &p),
            "{what} ({}) is not under ReadWritePaths {rw:?}",
            p.display()
        );
    }
}

/// The directory and file modes come from the tmpfiles rules (`d`/`z` lines: path, mode, user, group). The station
/// process is user `hotclock` with its primary and supplementary groups from the unit; it can replace a file when it
/// owns the directory or one of its groups owns it with the group write bit.
#[test]
fn the_stations_groups_can_write_the_config_directory_and_file() {
    let u = unit("hotclock-station@.service");
    let user = setting(&u, "User").pop().unwrap();
    let mut groups = setting(&u, "Group");
    groups.extend(setting(&u, "SupplementaryGroups"));
    let t = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tmpfiles/hotclock.conf"),
    )
    .or_else(|_| {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/tmpfiles");
        std::fs::read_to_string(
            std::fs::read_dir(d)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
    })
    .unwrap();
    for (what, path) in [
        ("the directory", "/etc/hotclock/stations"),
        ("the station file", "/etc/hotclock/stations/*.toml"),
    ] {
        let line = t
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>())
            .find(|f| f.len() >= 5 && matches!(f[0], "d" | "z") && f[1] == path)
            .unwrap_or_else(|| panic!("no tmpfiles rule for {what} {path}"));
        let (mode, owner, group) = (u32::from_str_radix(line[2], 8).unwrap(), line[3], line[4]);
        let ok = (owner == user && mode & 0o200 != 0)
            || (groups.iter().any(|g| g == group) && mode & 0o020 != 0);
        assert!(ok, "{what} {path} is {owner}:{group} {:o}; the station runs as {user} with groups {groups:?}", mode);
    }
}
