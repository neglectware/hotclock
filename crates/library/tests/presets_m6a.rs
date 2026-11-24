//! M6a: processing presets (saved settings of an effect): save, load, replace, rename, delete, the shipped
//! ones read-only, export and import with validation.

use library::presets::{self, Preset};
use library::{Library, Paths};
use serde_json::{json, Value};

fn lib(name: &str) -> Library {
    let d = fixtures::testdir::process_dir().join(format!("{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Library::open(Paths::from_data_dir(&d.join("data"))).unwrap()
}

fn warm() -> Value {
    json!({"target_lufs": -16.0, "within_item": "hold", "item_memory_s": 400.0, "peak_allowance_db": 1.0})
}

#[test]
fn shipped_presets_are_listed_first_and_cannot_be_changed() {
    let l = lib("shipped");
    let c = l.conn();
    let list = presets::list(c, "loudness").unwrap();
    let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["ambient", "balanced", "aggressive"]);
    assert!(list.iter().all(|p| p.builtin));
    for n in ["ambient", "aggressive"] {
        assert!(
            presets::put(c, "loudness", n, &warm(), "").is_err(),
            "{n} is read-only"
        );
        assert!(presets::delete(c, "loudness", n).is_err());
        assert!(presets::rename(c, "loudness", n, "mine").is_err());
    }
    // "Save as" under another name copies it.
    let amb: Preset = presets::get(c, "loudness", "ambient").unwrap().unwrap();
    presets::put(c, "loudness", "Ambient copy", &amb.params, "mine").unwrap();
    assert_eq!(
        presets::get(c, "loudness", "Ambient copy")
            .unwrap()
            .unwrap()
            .params,
        amb.params
    );
}

#[test]
fn a_user_preset_is_saved_loaded_replaced_renamed_and_deleted() {
    let l = lib("crud");
    let c = l.conn();
    let rev0 = l.revision().unwrap();
    presets::put(c, "loudness", "Warm", &warm(), "a little lower").unwrap();
    assert!(l.revision().unwrap() > rev0, "the library revision moves");
    let p = presets::get(c, "loudness", "Warm").unwrap().unwrap();
    assert!(!p.builtin);
    assert_eq!(p.params["target_lufs"], -16.0);
    assert_eq!(p.description, "a little lower");
    // Replace.
    presets::put(c, "loudness", "Warm", &json!({"target_lufs": -15.0}), "").unwrap();
    assert_eq!(
        presets::get(c, "loudness", "Warm").unwrap().unwrap().params["target_lufs"],
        -15.0
    );
    assert_eq!(presets::list(c, "loudness").unwrap().len(), 4);
    // Rename; the old name is gone, a taken name is refused.
    presets::put(c, "loudness", "Other", &warm(), "").unwrap();
    assert!(presets::rename(c, "loudness", "Warm", "Other").is_err());
    assert!(presets::rename(c, "loudness", "Warm", "balanced").is_err());
    assert!(presets::rename(c, "loudness", "Nope", "X").is_err());
    presets::rename(c, "loudness", "Warm", "Cosy").unwrap();
    assert!(presets::get(c, "loudness", "Warm").unwrap().is_none());
    assert!(presets::get(c, "loudness", "Cosy").unwrap().is_some());
    // Delete.
    assert!(presets::delete(c, "loudness", "Cosy").unwrap());
    assert!(!presets::delete(c, "loudness", "Cosy").unwrap());
    assert_eq!(presets::list(c, "loudness").unwrap().len(), 4);
}

#[test]
fn settings_are_validated_before_they_are_stored() {
    let l = lib("validate");
    let c = l.conn();
    for bad in [
        json!({"target_lufs": -3.0}),
        json!({"bogus": 1}),
        json!({"within_item": "sideways"}),
        json!({"enabled": true}),
        json!({"target_lufs": "loud"}),
        json!([1, 2]),
    ] {
        assert!(presets::put(c, "loudness", "X", &bad, "").is_err(), "{bad}");
    }
    for name in ["", " lead", "trail ", &"x".repeat(61)] {
        assert!(
            presets::put(c, "loudness", name, &warm(), "").is_err(),
            "{name:?}"
        );
    }
    assert!(
        presets::put(c, "agc", "X", &warm(), "").is_err(),
        "no such effect"
    );
    assert!(
        presets::list(c, "loudness")
            .unwrap()
            .iter()
            .all(|p| p.builtin),
        "nothing was stored"
    );
}

#[test]
fn export_and_import_round_trip_and_the_file_is_checked() {
    let l = lib("io");
    let c = l.conn();
    presets::put(c, "loudness", "Warm", &warm(), "lower").unwrap();
    let file = presets::export(
        "loudness",
        &presets::get(c, "loudness", "Warm").unwrap().unwrap(),
    );
    presets::delete(c, "loudness", "Warm").unwrap();
    assert_eq!(
        presets::import(c, "loudness", &file, false, None).unwrap(),
        "Warm"
    );
    assert_eq!(
        presets::get(c, "loudness", "Warm").unwrap().unwrap().params["item_memory_s"],
        400.0
    );
    // A second import neither overwrites by default nor loses the first; `overwrite` and a new name both work.
    assert!(presets::import(c, "loudness", &file, false, None).is_err());
    assert!(presets::import(c, "loudness", &file, true, None).is_ok());
    assert_eq!(
        presets::import(c, "loudness", &file, false, Some("Warm 2")).unwrap(),
        "Warm 2"
    );
    // Bad files.
    let mut wrong_effect = file.clone();
    wrong_effect["effect"] = json!("agc");
    let mut wrong_marker = file.clone();
    wrong_marker["hotclock_preset"] = json!(2);
    let mut out_of_range = file.clone();
    out_of_range["params"]["target_lufs"] = json!(-2.0);
    let mut shipped_name = file.clone();
    shipped_name["name"] = json!("ambient");
    let mut no_name = file.clone();
    no_name.as_object_mut().unwrap().remove("name");
    for (what, f) in [
        ("another effect", wrong_effect),
        ("no marker", wrong_marker),
        ("out of range", out_of_range),
        ("a shipped name", shipped_name),
        ("no name", no_name),
        ("not an object", json!("hello")),
    ] {
        assert!(
            presets::import(c, "loudness", &f, true, None).is_err(),
            "{what}"
        );
    }
}

#[test]
fn an_agc_preset_saved_with_a_fast_attack_of_0_reads_as_the_fast_clamp_off() {
    let l = lib("agc-attack0");
    let c = l.conn();
    // rows saved by an older release: the clamp was off at 0 ms, and 1 ms was allowed
    for (effect, name, params) in [
        (
            "agc",
            "Old no clamp",
            json!({"target_dbfs": -20.0, "fast_attack_ms": 0.0}),
        ),
        (
            "agc",
            "Old 1 ms",
            json!({"target_dbfs": -20.0, "fast_attack_ms": 1.0}),
        ),
        (
            "chain",
            "Old chain",
            json!({"agc": {"enabled": true, "fast_attack_ms": 0.0}}),
        ),
    ] {
        c.execute(
            "INSERT INTO processing_presets(effect, name, params, description, builtin, updated_at)
             VALUES(?1, ?2, ?3, '', 0, '2026-01-01T00:00:00Z')",
            rusqlite::params![effect, name, params.to_string()],
        )
        .unwrap();
    }
    let p = presets::get(c, "agc", "Old no clamp").unwrap().unwrap();
    assert_eq!(p.params["fast_clamp"], json!(false));
    assert_eq!(p.params["fast_attack_ms"], json!(10.0));
    let p = presets::get(c, "agc", "Old 1 ms").unwrap().unwrap();
    assert_eq!(p.params["fast_attack_ms"], json!(2.0));
    assert!(p.params.get("fast_clamp").is_none(), "on, as before");
    let p = presets::get(c, "chain", "Old chain").unwrap().unwrap();
    assert_eq!(p.params["agc"]["fast_clamp"], json!(false));
    // saved again, they pass the checks
    presets::put(
        c,
        "agc",
        "Old no clamp",
        &presets::get(c, "agc", "Old no clamp")
            .unwrap()
            .unwrap()
            .params,
        "",
    )
    .unwrap();
}
