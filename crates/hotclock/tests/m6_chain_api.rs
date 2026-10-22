//! M6 with real processes and real HTTP (`support_api`): `GET /api/v1/chain`, `PUT /api/v1/chain/{stage}`, whole-chain
//! presets, the config file the changes are written to, the restart that reads it back, the per-effect and
//! whole-chain presets of the library, and acceptance 12 (the API and the WebSocket frame carry the same values).

mod support_api;

use serde_json::{json, Value};
use serve::ws::{client::Conn, Message};
use std::time::Duration;
use support_api::*;

fn env(name: &str, extra: &str) -> Env {
    Env::new(
        name,
        &Opts {
            tones: 2,
            secs: 300.0,
            level: -18.0,
            extra_station: extra.into(),
            ..Opts::default()
        },
    )
}

fn wait_playing(e: &Env) {
    wait_for("playing", 30.0, || {
        get(&e.st(), "/api/v1/state").json()["now"]["title"]
            .as_str()
            .map(|_| ())
    });
}

fn chain(e: &Env) -> Value {
    get(&e.st(), "/api/v1/chain").json()
}

fn cfg_text(e: &Env) -> String {
    std::fs::read_to_string(e.dir.join("stations").join("office.toml")).unwrap()
}

#[test]
fn the_chain_api_reports_the_running_chain() {
    let mut e = env("m6-get", "processing_preset = \"radio\"\n");
    e.spawn_station(&[]);
    wait_playing(&e);
    let c = chain(&e);
    println!(
        "GET /chain under radio: preset {}, bands {}, crossovers {}, stages {:?}",
        c["preset"],
        c["multiband"]["bands"],
        c["multiband"]["crossovers_hz"],
        c["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| format!("{}={}", s["name"], s["enabled"]))
            .collect::<Vec<_>>()
    );
    assert_eq!(c["preset"], "radio");
    assert_eq!(c["custom"], false);
    assert_eq!(c["bypassed"], false);
    assert_eq!(c["multiband"]["bands"], 5);
    assert_eq!(
        c["multiband"]["crossovers_hz"],
        json!([120.0, 500.0, 2000.0, 6000.0])
    );
    assert_eq!(c["makeup_db"].as_array().unwrap().len(), 5);
    assert_eq!(c["blocks"]["agc"]["enabled"], true);
    assert_eq!(c["blocks"]["wideband"]["enabled"], false);
    assert_eq!(c["blocks"]["clipper"]["enabled"], false);
    for s in ["agc", "multiband", "limiter", "safety_limiter"] {
        let st = c["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == s)
            .unwrap();
        assert_eq!(st["enabled"], true, "{s}");
    }
    // The schema comes only on request.
    assert!(c.get("schema").is_none());
    let s = get(&e.st(), "/api/v1/chain?schema=1").json();
    let blocks: Vec<&str> = s["schema"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        blocks,
        [
            "trim",
            "highpass",
            "rotator",
            "stereo",
            "agc",
            "eq",
            "bass",
            "multiband",
            "wideband",
            "makeup",
            "clipper",
            "limiter"
        ]
    );
    // And the live frame carries it.
    let mut ws = Conn::connect(&e.st(), "/api/v1/ws?topics=meters", &[]).unwrap();
    ws.set_timeout(Some(Duration::from_secs(5)));
    let mut mask = 0;
    let mut bands = 0;
    for _ in 0..40 {
        if let Ok(Message::Binary(b)) = ws.recv() {
            mask = b[28];
            bands = b[29];
        }
    }
    println!("meter frame under radio: stage mask {mask:#010b}, band count {bands}");
    assert_eq!(
        mask & 0b111111,
        0b101011,
        "agc, multiband, limiter and safety"
    );
    assert_eq!(bands, 5);
}

#[test]
fn a_block_change_is_checked_applied_written_and_survives_a_restart() {
    let mut e = env("m6-put", "processing_preset = \"radio\"\n");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    let rev0 = chain(&e)["chain_cfg_rev"].as_u64().unwrap();
    let r = send(
        &st,
        "PUT",
        "/api/v1/chain/agc",
        &json!({"target_dbfs": -22.0, "up_db_per_s": 2.0, "user_preset": "Mine"}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["blocks"]["agc"]["target_dbfs"], -22.0);
    assert_eq!(j["custom"], true);
    assert_eq!(j["block_presets"]["agc"], "Mine");
    assert!(j["chain_cfg_rev"].as_u64().unwrap() > rev0);
    let text = cfg_text(&e);
    println!("config after PUT /chain/agc:\n{text}");
    assert!(text.contains("processing_preset = \"radio\""));
    assert!(
        text.contains("[chain.agc]")
            && text.contains("target_dbfs = -22")
            && text.contains("user_preset = \"Mine\""),
        "{text}"
    );
    assert!(
        !text.contains("[chain.multiband]"),
        "only what differs from radio is written"
    );
    // A change of the multiband's band count carries its arrays; a mismatch is refused and changes nothing.
    let r = send(
        &st,
        "PUT",
        "/api/v1/chain/multiband",
        &json!({"crossovers_hz": [200.0, 4000.0]}),
    );
    assert_eq!(r.status, 400);
    println!(
        "mismatched band arrays: {} {}",
        r.status,
        r.json()["error"]["message"]
    );
    assert_eq!(chain(&e)["multiband"]["bands"], 5);
    let three = json!({"crossovers_hz": [200.0, 4000.0], "threshold_dbfs": [-24.0, -22.0, -24.0], "ratio": [2.0, 2.0, 2.0], "knee_db": [6.0, 6.0, 6.0],
        "attack_ms": [30.0, 12.0, 5.0], "release_ms": [400.0, 200.0, 120.0], "makeup_db": [2.0, 2.0, 2.0]});
    let r = send(&st, "PUT", "/api/v1/chain/multiband", &three);
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["multiband"]["bands"], 3);
    // Refusals.
    for (stage, body, code) in [
        ("agc", json!({"target_dbfs": 3.0}), 400),
        ("agc", json!({"nope": 1}), 400),
        ("agc", json!({"enabled": "yes"}), 400),
        ("trim", json!({"enabled": true}), 400),
        ("eq", json!({"position": "middle"}), 400),
        ("nonesuch", json!({}), 404),
    ] {
        let r = send(&st, "PUT", &format!("/api/v1/chain/{stage}"), &body);
        assert_eq!(r.status, code, "{stage} {body}: {}", r.text());
    }
    assert_eq!(send(&st, "POST", "/api/v1/chain", &json!({})).status, 405);
    // The file reads back through the parser (the station's own) as the same chain after a restart.
    std::thread::sleep(Duration::from_millis(300));
    let before = chain(&e);
    e.children[0].kill().unwrap();
    let _ = e.children[0].wait();
    e.children.clear();
    e.spawn_station(&[]);
    wait_playing(&e);
    let after = chain(&e);
    println!(
        "after a restart: preset {}, custom {}, bands {}, agc target {} (before the restart: bands {}, target {})",
        after["preset"], after["custom"], after["multiband"]["bands"], after["blocks"]["agc"]["target_dbfs"],
        before["multiband"]["bands"], before["blocks"]["agc"]["target_dbfs"]
    );
    assert_eq!(after["blocks"], before["blocks"]);
    assert_eq!(after["block_presets"], before["block_presets"]);
}

#[test]
fn chain_presets_and_the_whole_chain_bypass() {
    let mut e = env("m6-presets", "");
    e.spawn_station(&[]);
    wait_playing(&e);
    let st = e.st();
    assert_eq!(chain(&e)["preset"], "bypass");
    assert_eq!(chain(&e)["bypassed"], true);
    let r = send(&st, "PUT", "/api/v1/chain", &json!({"preset": "radio"}));
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["multiband"]["bands"], 5);
    assert!(cfg_text(&e).contains("processing_preset = \"radio\""));
    let r = post(&st, "/api/v1/processing/preset", &json!({"name": "gentle"}));
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["multiband"]["bands"], 3);
    let text = cfg_text(&e);
    assert!(
        text.contains("processing_preset = \"gentle\"") && !text.contains("[chain"),
        "{text}"
    );
    assert_eq!(
        post(&st, "/api/v1/processing/preset", &json!({"name": "loud"})).status,
        400
    );
    // The whole-chain bypass: out and back, the saved settings untouched.
    let r = post(&st, "/api/v1/processing/bypass", &json!({"on": true}));
    assert_eq!(r.status, 200);
    let j = r.json();
    assert_eq!(j["bypassed"], true);
    assert_eq!(j["bypass_override"], true);
    assert!(
        cfg_text(&e).contains("processing_preset = \"gentle\""),
        "the file keeps the configured chain"
    );
    let r = post(&st, "/api/v1/processing/bypass", &json!({"on": false}));
    assert_eq!(r.json()["bypassed"], false);
    assert_eq!(r.json()["preset"], "gentle");
    // A saved whole chain is applied from its settings; the file names the closest shipped preset and the changes.
    let mut c = common::chain::preset_config("radio").unwrap();
    c.apply_block("agc", json!({"target_dbfs": -23.0}).as_object().unwrap())
        .unwrap();
    c.apply_block("clipper", json!({"enabled": true}).as_object().unwrap())
        .unwrap();
    let r = send(
        &st,
        "PUT",
        "/api/v1/chain",
        &json!({"chain": c.to_json(), "user_preset": "My sound"}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["user_preset"], "My sound");
    assert_eq!(j["preset"], "radio");
    assert_eq!(j["blocks"]["clipper"]["enabled"], true);
    let text = cfg_text(&e);
    println!("config for a saved whole chain:\n{text}");
    assert!(
        text.contains("processing_preset = \"radio\"")
            && text.contains("user_preset = \"My sound\"")
            && text.contains("[chain.clipper]"),
        "{text}"
    );
    // Bad bodies.
    for bad in [
        json!({"preset": 3}),
        json!({"chain": {"agc": {"target_dbfs": 9}}}),
        json!({"surprise": 1}),
    ] {
        assert_eq!(send(&st, "PUT", "/api/v1/chain", &bad).status, 400, "{bad}");
    }
}

#[test]
fn the_library_keeps_whole_chain_and_per_effect_presets() {
    let mut e = env("m6-lib", "");
    e.spawn_library();
    let lb = e.lb();
    // Shipped ones first, per effect.
    let l = get(&lb, "/api/v1/processing-presets/chain").json();
    let names: Vec<&str> = l["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bypass", "gentle", "radio"]);
    let mb = get(&lb, "/api/v1/processing-presets/multiband").json();
    let mbn: Vec<&str> = mb["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    println!("shipped multiband presets: {mbn:?}");
    assert!(mbn.contains(&"radio") && mbn.contains(&"gentle") && mbn.contains(&"four band"));
    assert!(
        get(&lb, "/api/v1/processing-presets").json()["effects"]
            .as_array()
            .unwrap()
            .len()
            >= 13
    );
    // Save, replace, rename, export, import, delete; shipped ones refuse.
    let params = common::chain::ChainConfig::default().params_json("eq");
    let mut p2 = params.clone();
    p2["low_db"] = json!(4.0);
    let r = send(
        &lb,
        "PUT",
        "/api/v1/processing-presets/eq/Warm%20room",
        &json!({"params": p2, "description": "mine"}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/eq/warm",
            &json!({"params": p2})
        )
        .status,
        422,
        "a shipped name is refused"
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/eq/Bad",
            &json!({"params": {"low_db": 99}})
        )
        .status,
        422
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/eq/Bad",
            &json!({"params": {"enabled": true}})
        )
        .status,
        422
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/multiband/Bad",
            &json!({"params": {"crossovers_hz": [100.0, 900.0]}})
        )
        .status,
        422
    );
    let exp = get(&lb, "/api/v1/processing-presets/eq/Warm%20room/export").json();
    assert_eq!(exp["effect"], "eq");
    assert_eq!(
        send(
            &lb,
            "POST",
            "/api/v1/processing-presets/eq/Warm%20room/rename",
            &json!({"to": "Warm room 2"})
        )
        .status,
        200
    );
    assert_eq!(
        send(
            &lb,
            "POST",
            "/api/v1/processing-presets/eq/import",
            &json!({"file": exp})
        )
        .status,
        200
    );
    let l = get(&lb, "/api/v1/processing-presets/eq").json();
    assert_eq!(
        l["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| !p["builtin"].as_bool().unwrap())
            .count(),
        2
    );
    // A whole chain.
    let all = common::chain::preset_config("radio").unwrap().to_json();
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/chain/Station%20sound",
            &json!({"params": all})
        )
        .status,
        200
    );
    assert_eq!(
        send(
            &lb,
            "PUT",
            "/api/v1/processing-presets/chain/Broken",
            &json!({"params": {"agc": {"target_dbfs": 7}}})
        )
        .status,
        422
    );
    assert_eq!(
        send(
            &lb,
            "DELETE",
            "/api/v1/processing-presets/chain/radio",
            &json!({})
        )
        .status,
        422,
        "a shipped chain preset cannot be deleted"
    );
    assert_eq!(
        send(
            &lb,
            "DELETE",
            "/api/v1/processing-presets/chain/Station%20sound",
            &json!({})
        )
        .status,
        204
    );
}

/// The two JSON values are the same, numbers to 1e-9 (one went through text, the other did not).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            (x.as_f64().unwrap() - y.as_f64().unwrap()).abs() < 1e-9
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn differing(a: &Value, b: &Value) -> String {
    a.as_object()
        .map(|o| {
            o.iter()
                .filter(|(k, v)| !b.get(k.as_str()).is_some_and(|w| same(v, w)))
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Acceptance 12: `GET /api/v1/chain/meters` and the WebSocket frame of the same `seq` carry identical values, and
/// `GET /api/v1/chain` reports the stage list, band count and crossovers in use.
#[test]
fn acceptance_12_the_api_and_the_frame_agree() {
    let mut e = env("m6-parity", "processing_preset = \"radio\"\n");
    e.spawn_station(&[]);
    wait_playing(&e);
    let frames: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u32, Vec<u8>>>> =
        Default::default();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (f2, s2, addr) = (frames.clone(), stop.clone(), e.st());
    let reader = std::thread::spawn(move || {
        let mut ws = Conn::connect(&addr, "/api/v1/ws?topics=meters", &[]).unwrap();
        ws.set_timeout(Some(Duration::from_secs(5)));
        while !s2.load(std::sync::atomic::Ordering::Relaxed) {
            if let Ok(Message::Binary(b)) = ws.recv() {
                let seq = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                let mut m = f2.lock().unwrap();
                if m.len() > 600 {
                    m.clear();
                }
                m.insert(seq, b);
            }
        }
    });
    std::thread::sleep(Duration::from_secs(6));
    let (mut checked, mut fields) = (0, 0);
    for _ in 0..8 {
        let j = get(&e.st(), "/api/v1/chain/meters").json();
        let seq = j["seq"].as_u64().unwrap() as u32;
        std::thread::sleep(Duration::from_millis(150));
        let Some(frame) = frames.lock().unwrap().get(&seq).cloned() else {
            continue;
        };
        let arr: [u8; serve::meters::FRAME_LEN] = frame.try_into().unwrap();
        let fj = serve::meters::frame_json(&arr);
        assert!(
            same(&j, &fj),
            "seq {seq}: {} fields differ",
            differing(&j, &fj)
        );
        fields += fj.as_object().unwrap().len();
        checked += 1;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = reader.join();
    let c = chain(&e);
    let j = get(&e.st(), "/api/v1/chain/meters").json();
    println!(
        "GET /chain/meters against the WebSocket frame of the same seq: {checked} of 8 requests compared, every field identical ({fields} fields); \
         the frame's band count {} and stage mask {:#010b} against GET /chain: {} bands, crossovers {}",
        j["band_count"], j["stage_mask"].as_u64().unwrap(), c["multiband"]["bands"], c["multiband"]["crossovers_hz"]
    );
    assert!(checked >= 3, "too few GETs had a WebSocket twin: {checked}");
    assert_eq!(j["band_count"], c["multiband"]["bands"]);
    assert_eq!(c["multiband"]["crossovers_hz"].as_array().unwrap().len(), 4);
    // The stage list agrees with the frame's mask.
    let mask = j["stage_mask"].as_u64().unwrap();
    for (i, name) in [
        "agc",
        "multiband",
        "wideband",
        "limiter",
        "clipper",
        "safety_limiter",
    ]
    .iter()
    .enumerate()
    {
        let on = c["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == *name)
            .unwrap()["enabled"]
            .as_bool()
            .unwrap();
        assert_eq!(on, mask >> i & 1 == 1, "{name}");
    }
}

/// the owner's first hands-on test: the config file could not be written (a read-only file system under the unit), and
/// every change failed. A change must go live anyway and the answer must carry a warning that it is not saved.
#[test]
fn a_change_goes_live_when_the_config_file_cannot_be_written_and_says_so() {
    let mut e = env("m6-soft", "processing_preset = \"radio\"\n");
    e.spawn_station(&[]);
    wait_playing(&e);
    // A directory where the file was: reading and replacing it both fail, as a read-only file system would.
    let f = e.dir.join("stations").join("office.toml");
    std::fs::rename(&f, e.dir.join("stations").join("office.toml.away")).unwrap();
    std::fs::create_dir(&f).unwrap();
    let r = send(
        &e.st(),
        "PUT",
        "/api/v1/chain/wideband",
        &json!({"enabled": true}),
    );
    println!(
        "PUT with an unwritable file: {} {}",
        r.status,
        r.json()["persist_warning"]
    );
    assert_eq!(r.status, 200, "{}", r.json());
    let c = chain(&e);
    assert_eq!(c["blocks"]["wideband"]["enabled"], true, "live");
    assert!(
        c["persist_warning"]
            .as_str()
            .unwrap()
            .contains("not have it"),
        "{c}"
    );
    // Once the file is writable again the next change clears the warning.
    std::fs::remove_dir(&f).unwrap();
    std::fs::rename(e.dir.join("stations").join("office.toml.away"), &f).unwrap();
    let r = send(
        &e.st(),
        "PUT",
        "/api/v1/chain/wideband",
        &json!({"enabled": false}),
    );
    assert_eq!(r.status, 200);
    assert_eq!(chain(&e)["persist_warning"], Value::Null);
    assert!(cfg_text(&e).contains("processing_preset"));
}
