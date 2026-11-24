//! The status display's switch, `display.enabled` (Settings, Status display): the boot generator wants the unit only
//! while it is on, the unit's `ExecCondition` (`hotclock display --check-enabled`) refuses to start it while it is
//! off, and the library service starts or stops the unit when the Settings page changes it, through
//! `dev/fake-systemctl.sh` standing in for systemd, and reports what systemd and the display say beside the switch.

mod support_api;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use support_api::*;

fn packaging(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging")
        .join(rel)
}

/// Run the generator against `global` and a stations directory with one station; the names it made wanted.
fn generate(dir: &Path, global: Option<&str>) -> Vec<String> {
    let out = dir.join("gen");
    let _ = std::fs::remove_dir_all(&out);
    let stations = dir.join("stations");
    std::fs::create_dir_all(&stations).unwrap();
    std::fs::write(stations.join("office.toml"), "name = \"office\"\n").unwrap();
    let g = dir.join("hotclock.toml");
    let _ = std::fs::remove_file(&g);
    if let Some(t) = global {
        std::fs::write(&g, t).unwrap();
    }
    let st = Command::new("sh")
        .arg(packaging("systemd/hotclock-stations-generator"))
        .arg(&out)
        .arg(&out)
        .arg(&out)
        .env("HOTCLOCK_STATIONS_DIR", &stations)
        .env("HOTCLOCK_GLOBAL_CONFIG", &g)
        .status()
        .unwrap();
    assert!(st.success());
    let wants = out.join("multi-user.target.wants");
    let mut v: Vec<String> = std::fs::read_dir(&wants)
        .unwrap()
        .flatten()
        .map(|e| {
            let target = std::fs::read_link(e.path()).unwrap();
            format!(
                "{} -> {}",
                e.file_name().to_string_lossy(),
                target.display()
            )
        })
        .collect();
    v.sort();
    v
}

const ON: &str = "hotclock-display.service -> /lib/systemd/system/hotclock-display.service";
const OFFICE: &str =
    "hotclock-station@office.service -> /lib/systemd/system/hotclock-station@.service";

#[test]
fn the_generator_wants_the_display_unless_the_switch_is_off() {
    let d = tmpdir("display_generator");
    // on: the default (no file, no key, no block), and an explicit true
    for global in [
        None,
        Some("data_dir = \"/var/lib/hotclock\"\n"),
        Some("[display]\nmode = \"1024x600@60\"\n"),
        Some("[display]\nenabled = true\n"),
        // the shipped file's commented example says nothing
        Some("# [display]\n# enabled = false\n"),
        // another table's `enabled` is not the display's
        Some("[display]\nmode = \"800x480\"\n[stream]\nenabled = false\n"),
    ] {
        assert_eq!(generate(&d, global), [ON, OFFICE], "{global:?}");
    }
    let shipped = std::fs::read_to_string(packaging("etc/hotclock.toml")).unwrap();
    assert_eq!(
        generate(&d, Some(&shipped)),
        [ON, OFFICE],
        "the shipped file"
    );
    // off: the key in the [display] table, however it is spaced and commented, or dotted before any table
    for global in [
        "[display]\nenabled = false\n",
        "time_zone = \"UTC\"\n\n  [ display ]   # the box's monitor\nmode = \"1024x600@60\"\n  enabled=false   # off since 13 Oct\n[library]\nlisten = \"0.0.0.0:8300\"\n",
        "display.enabled = false\n[library]\nlisten = \"0.0.0.0:8300\"\n",
    ] {
        assert_eq!(generate(&d, Some(global)), [OFFICE], "{global}");
    }
    // the stations are not touched by the switch
    assert_eq!(
        generate(&d, Some("[display]\nenabled = false\n")).len(),
        1,
        "the station stays wanted"
    );
}

fn check_enabled(global: &Path) -> i32 {
    Command::new(env!("CARGO_BIN_EXE_hotclock"))
        .arg("--global-config")
        .arg(global)
        .args(["display", "--check-enabled"])
        .output()
        .unwrap()
        .status
        .code()
        .unwrap()
}

#[test]
fn the_unit_condition_follows_the_parser() {
    let d = tmpdir("display_condition");
    let g = d.join("hotclock.toml");
    assert_eq!(check_enabled(&g), 0, "no file: the default, on");
    for (text, code) in [
        ("[display]\nenabled = true\n", 0),
        ("[display]\nenabled = false\n", 1),
        ("display.enabled = false\n", 1),
        // a file the parser refuses: the display itself runs and names the key, as before the switch
        ("[display]\nenabled = \"no\"\n", 0),
    ] {
        std::fs::write(&g, text).unwrap();
        assert_eq!(check_enabled(&g), code, "{text}");
    }
    let unit = std::fs::read_to_string(packaging("systemd/hotclock-display.service")).unwrap();
    assert!(
        unit.lines()
            .any(|l| l == "ExecCondition=/usr/bin/hotclock display --check-enabled"),
        "{unit}"
    );
}

fn status(lb: &str) -> Value {
    let r = get(lb, "/api/v1/display/status");
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

fn put_global(lb: &str, values: Value) -> Value {
    let r = send(
        lb,
        "PUT",
        "/api/v1/settings/global",
        &json!({ "values": values }),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()
}

#[test]
fn the_switch_starts_and_stops_the_unit_and_reports_the_monitor() {
    let mut e = Env::new("display_switch", &Opts::default());
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dev/fake-systemctl.sh");
    let sc = e.dir.join("sc");
    let state = e.dir.join("display-state");
    std::fs::create_dir_all(&sc).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let global = e.global.clone();
    e.spawn_library_with(&[
        ("HOTCLOCK_SYSTEMCTL", fake.as_os_str()),
        ("FAKE_SC_DIR", sc.as_os_str()),
        ("FAKE_SC_BIN", env!("CARGO_BIN_EXE_hotclock").as_ref()),
        ("FAKE_SC_GLOBAL", global.as_os_str()),
        ("HOTCLOCK_DISPLAY_STATE_DIR", state.as_os_str()),
    ]);
    let lb = e.lb();
    let log = || std::fs::read_to_string(sc.join("systemctl.log")).unwrap_or_default();

    // The default: on, in the catalog with its label, and stopped until something starts it.
    let s = get(&lb, "/api/v1/settings").json();
    let def = s["catalog"]["global"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"] == "display.enabled")
        .cloned()
        .expect("display.enabled in the catalog");
    assert_eq!(
        (&def["kind"], &def["default"], &def["apply"]),
        (&json!("bool"), &json!(true), &json!("display_switch"))
    );
    assert!(def["help"]
        .as_str()
        .unwrap()
        .starts_with("Status display on this box's monitor."));
    assert_eq!(s["global"]["effective"]["display.enabled"], true);
    assert_eq!(s["display_status"]["text"], "stopped");
    let st = status(&lb);
    assert_eq!(
        (&st["enabled"], &st["state"], &st["running"]),
        (&json!(true), &json!("inactive"), &json!(false))
    );

    // Off: saved first, then stopped.
    let r = put_global(&lb, json!({"display.enabled": false}));
    assert_eq!(r["saved"], true);
    assert_eq!(r["display_switch"]["action"], "stop");
    assert_eq!(r["display_switch"]["ok"], true);
    assert_eq!(r["changed"][0]["applied"], true);
    assert!(
        log().lines().any(|l| l == "stop hotclock-display.service"),
        "{}",
        log()
    );
    let text = std::fs::read_to_string(&global).unwrap();
    assert!(
        text.contains("[display]") && text.contains("enabled = false"),
        "{text}"
    );
    assert_eq!(check_enabled(&global), 1);
    assert_eq!(status(&lb)["enabled"], false);
    // Off holds: a start of the unit (a restart from the page, a boot) is skipped by its condition.
    let r = post(&lb, "/api/v1/system/restart", &json!({"unit": "display"}));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(status(&lb)["state"], "inactive");

    // On: saved, then started; running, and it has not reported a monitor yet.
    let r = put_global(&lb, json!({"display.enabled": true}));
    assert_eq!(r["display_switch"]["action"], "start");
    assert_eq!(r["display_switch"]["ok"], true);
    assert_eq!(r["display_switch"]["status"]["running"], true);
    assert!(
        log().lines().any(|l| l == "start hotclock-display.service"),
        "{}",
        log()
    );
    let st = status(&lb);
    assert_eq!(
        (&st["state"], &st["running"]),
        (&json!("active"), &json!(true))
    );
    assert_eq!(st["text"], "running; it has not reported a monitor yet");

    // What the display writes beside its view: waiting for a station, the monitor in use, or none.
    let me = std::process::id();
    std::fs::write(
        state.join("output.json"),
        json!({"v": 1, "pid": me, "connected": null, "waiting": "no station file in /etc/hotclock/stations"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        status(&lb)["text"],
        "waiting for a station (no station file in /etc/hotclock/stations)"
    );
    std::fs::write(
        state.join("output.json"),
        json!({"v": 1, "pid": me, "connected": true, "connector": "HDMI-A-2", "mode": "1024x600@60"}).to_string(),
    )
    .unwrap();
    let st = status(&lb);
    assert_eq!(st["text"], "HDMI-A-2, 1024x600@60");
    assert_eq!(st["output"]["connector"], "HDMI-A-2");
    std::fs::write(
        state.join("output.json"),
        json!({"v": 1, "pid": me, "connected": false, "error": "/dev/dri/card0: no connected output"}).to_string(),
    )
    .unwrap();
    assert_eq!(status(&lb)["text"], "no monitor connected");
    // a file left by a display that is gone is not believed
    std::fs::write(
        state.join("output.json"),
        json!({"v": 1, "pid": 999_999_999u64, "connected": true, "connector": "HDMI-A-1", "mode": "800x480@60"}).to_string(),
    )
    .unwrap();
    assert_eq!(status(&lb)["output"], Value::Null);

    // When systemd refuses, the file is still saved (the next boot follows it) and the answer says the unit did not
    // start.
    put_global(&lb, json!({"display.enabled": false}));
    std::fs::write(sc.join("fail"), "start:display").unwrap();
    let r = put_global(&lb, json!({"display.enabled": true}));
    assert_eq!(r["saved"], true);
    assert_eq!(r["display_switch"]["ok"], false);
    assert!(r["display_switch"]["message"]
        .as_str()
        .unwrap()
        .contains("systemctl start hotclock-display.service failed"));
    assert_eq!(r["changed"][0]["applied"], false);
    assert_eq!(check_enabled(&global), 0);
    assert_eq!(status(&lb)["text"], "stopped");

    // Other global keys do not touch the unit.
    let n = log().lines().count();
    put_global(&lb, json!({"display.diagonal_in": 10.1}));
    assert_eq!(log().lines().count(), n, "{}", log());
}

// ---- what the display writes for the Settings page: `output.json` beside its view ----------------------------------

use display::app::{App, Config, OutputFactory, StationSpec};
use display::output::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct Panel(Arc<AtomicBool>);

impl Output for Panel {
    fn size(&self) -> (i32, i32) {
        (1024, 600)
    }
    fn present(
        &mut self,
        _c: &display::canvas::Canvas,
        _d: &[display::layout::Rect],
        _l: &common::night::Lut,
    ) -> Result<(), String> {
        Ok(())
    }
    fn connected(&mut self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn describe(&self) -> String {
        "HDMI-A-2 test panel".into()
    }
    fn connector(&self) -> String {
        "HDMI-A-2".into()
    }
    fn mode(&self) -> String {
        "1024x600@60".into()
    }
}

struct Plug(Arc<AtomicBool>);

impl OutputFactory for Plug {
    fn open(&mut self, _c: &common::config::DisplayConfig) -> Result<Box<dyn Output>, String> {
        if self.0.load(Ordering::SeqCst) {
            Ok(Box::new(Panel(self.0.clone())))
        } else {
            Err("/dev/dri/card0: no connected output".into())
        }
    }
}

#[test]
fn the_display_reports_its_monitor_and_its_loss_in_output_json() {
    let d = tmpdir("display_output_json");
    let plugged = Arc::new(AtomicBool::new(false));
    let cfg = Config {
        display: common::config::DisplayConfig::default(),
        // nobody listens there: the display draws its "not connected" screen, which is all this needs
        stations: vec![StationSpec {
            id: "office".into(),
            display_name: "Office".into(),
            addr: format!("127.0.0.1:{}", free_port()),
        }],
        library_addr: format!("127.0.0.1:{}", free_port()),
        state_file: Some(d.join("view.json")),
        force_view: None,
        tz_min: Some(0),
        product: "Hotclock".into(),
        force_night: None,
        snapshot: None,
    };
    let mut app = App::start(cfg, Box::new(Plug(plugged.clone())));
    let read = || -> Value {
        serde_json::from_str(&std::fs::read_to_string(d.join("output.json")).unwrap()).unwrap()
    };
    app.tick();
    let v = read();
    assert_eq!(v["connected"], false, "{v}");
    assert_eq!(v["error"], "/dev/dri/card0: no connected output");
    assert_eq!(v["pid"], std::process::id());
    // a failure that repeats is not written again
    let at = v["at_ms"].clone();
    std::thread::sleep(std::time::Duration::from_millis(2100));
    app.tick();
    assert_eq!(read()["at_ms"], at);
    // the monitor arrives (the next try is 2 s after the last)
    plugged.store(true, Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    app.tick();
    let v = read();
    assert_eq!(
        (&v["connected"], &v["connector"], &v["mode"]),
        (&json!(true), &json!("HDMI-A-2"), &json!("1024x600@60")),
        "{v}"
    );
    // and goes
    plugged.store(false, Ordering::SeqCst);
    app.tick();
    let v = read();
    assert_eq!(v["connected"], false, "{v}");
    assert_eq!(v["error"], "the monitor was disconnected");
}

// ---- the postinst's one-time migration (packaging/debian/postinst, `display_switch_migrate`) -----------------------

/// Run the function from the postinst against `text` as the global file: `was` is what `systemctl is-enabled` said,
/// `old` the version upgraded from (empty on a fresh install). Returns the file afterwards and the output.
fn migrate(
    d: &Path,
    text: Option<&str>,
    was: &str,
    old: &str,
    marker: bool,
) -> (Option<String>, String) {
    let g = d.join("hotclock.toml");
    let m = d.join("lib/.display-switch");
    let _ = std::fs::remove_file(&g);
    let _ = std::fs::remove_file(g.with_extension("toml.pre-display-switch"));
    let _ = std::fs::remove_dir_all(d.join("lib"));
    if let Some(t) = text {
        std::fs::write(&g, t).unwrap();
    }
    if marker {
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(&m, "x").unwrap();
    }
    let script = format!(
        "set -e\neval \"$(sed -n '/^display_switch_migrate() {{/,/^}}/p' '{}')\"\nglobal='{}'\nmarker='{}'\nhotclock='{}'\ndisplay_switch_migrate '{was}' '{old}'\ntest -e \"$marker\"",
        packaging("debian/postinst").display(),
        g.display(),
        m.display(),
        env!("CARGO_BIN_EXE_hotclock"),
    );
    let o = Command::new("sh").arg("-c").arg(&script).output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        o.status.success(),
        "the migration failed or left no marker: {out}"
    );
    (std::fs::read_to_string(&g).ok(), out)
}

#[test]
fn the_first_upgrade_keeps_what_the_box_had_and_later_ones_touch_nothing() {
    let d = tmpdir("display_migrate");
    let shipped = std::fs::read_to_string(packaging("etc/hotclock.toml")).unwrap();
    let g = d.join("hotclock.toml");
    // a fresh install: the default applies, nothing written
    let (t, _) = migrate(&d, Some(&shipped), "disabled", "", false);
    assert_eq!(t.as_deref(), Some(shipped.as_str()));
    assert_eq!(check_enabled(&g), 0);
    // an upgrade of a box where the display was enabled by hand: it stays on, nothing written
    let (t, _) = migrate(&d, Some(&shipped), "enabled", "0.1.0-30", false);
    assert_eq!(t.as_deref(), Some(shipped.as_str()));
    // an upgrade of a box where it was not: the shipped file (its [display] only commented) gets a block at the end
    let (t, out) = migrate(&d, Some(&shipped), "disabled", "0.1.0-30", false);
    let t = t.unwrap();
    assert!(t.starts_with(&shipped) && t.ends_with("\n[display]\nenabled = false   # the status display was not enabled before the Settings switch existed\n"), "{t}");
    assert!(out.contains("wrote enabled = false"), "{out}");
    assert_eq!(check_enabled(&g), 1);
    assert_eq!(
        std::fs::read_to_string(g.with_extension("toml.pre-display-switch")).unwrap(),
        shipped
    );
    assert_eq!(
        generate(&d, Some(&t)),
        [OFFICE],
        "the generator reads it as off"
    );
    // ... a file with its own [display] block gets the key inside it, comments and the rest kept
    let own = "data_dir = \"/var/lib/hotclock\"\n\n[display]   # the 7-inch panel\nmode = \"1024x600@60\"\n\n[library]\nlisten = \"0.0.0.0:8300\"\n";
    let (t, _) = migrate(&d, Some(own), "static", "0.1.0-30", false);
    assert_eq!(
        t.unwrap(),
        own.replace(
            "[display]   # the 7-inch panel\n",
            "[display]   # the 7-inch panel\nenabled = false   # the status display was not enabled before the Settings switch existed\n"
        )
    );
    assert_eq!(check_enabled(&g), 1);
    // ... a file that already says it, either way, is left alone
    for said in [
        "[display]\nenabled = true\n",
        "[display]\nenabled = false\n",
    ] {
        let (t, _) = migrate(&d, Some(said), "disabled", "0.1.0-30", false);
        assert_eq!(t.as_deref(), Some(said));
    }
    // ... display keys outside a [display] block: not edited, and the output says what to do
    let dotted = "display.mode = \"800x480\"\n";
    let (t, out) = migrate(&d, Some(dotted), "disabled", "0.1.0-30", false);
    assert_eq!(t.as_deref(), Some(dotted));
    assert!(out.contains("add enabled = false there"), "{out}");
    // ... no global file: nothing to write
    let (t, _) = migrate(&d, None, "disabled", "0.1.0-30", false);
    assert_eq!(t, None);
    // every later upgrade (the marker): nothing, whatever the file and the unit say
    let (t, _) = migrate(&d, Some(&shipped), "disabled", "0.1.0-31", true);
    assert_eq!(t.as_deref(), Some(shipped.as_str()));
}
