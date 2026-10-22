//! M2 acceptance 2 and 3 over real HTTP against a real library service: the transition preview is sample-identical
//! to the scenario renderer for the same pair, a marker saved through the API changes the effective marker and the
//! preview, every preset can be auditioned, and an item's own transition settings reach the preview.

mod support_api;
use library::{import_file, ImportOptions, ImportOutcome};
use serde_json::{json, Value};
use support_api::*;

fn item(e: &Env, path: &std::path::Path) -> i64 {
    match import_file(
        &e.lib,
        path,
        &ImportOptions {
            category: Some("music".into()),
            source: "test".into(),
        },
    ) {
        ImportOutcome::Done { item_id, .. } => item_id,
        o => panic!("{o:?}"),
    }
}

fn wav_f32(body: &[u8]) -> Vec<f32> {
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(
        u16::from_le_bytes([body[20], body[21]]),
        3,
        "IEEE float WAV"
    );
    body[44..]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn preview(lb: &str, q: &str) -> (Vec<u8>, Value) {
    let r = get(lb, &format!("/api/v1/preview/transition?{q}"));
    assert_eq!(r.status, 200, "{}", r.text());
    let meta: Value = serde_json::from_str(r.header("x-preview").expect("x-preview")).unwrap();
    (r.body.clone(), meta)
}

/// The scenario renderer on the same two stored files, with the same overrides, as a plain scenario.
fn scenario_pair(
    e: &Env,
    a: i64,
    b: i64,
    preset: &str,
    a_ovr: Value,
) -> station::scenario::Rendered {
    let path = |id: i64| -> String {
        let sp: String = e
            .lib
            .conn()
            .query_row("SELECT store_path FROM items WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        e.data.join("media").join(sp).to_string_lossy().to_string()
    };
    let sc = station::scenario::Scenario::from_yaml(
        &json!({
            "name": "x", "station": {"preset": preset, "start_s": 0.1},
            "items": {"a": {"file": path(a), "category": "music", "overrides": a_ovr}, "b": {"file": path(b), "category": "music"}},
            "queue": [{"item": "a"}, {"item": "b"}],
        })
        .to_string(),
    )
    .unwrap();
    station::scenario::preview_pair(&sc, None, 10.0, 10.0).unwrap()
}

#[test]
fn preview_is_sample_identical_to_the_scenario_renderer_and_follows_the_markers() {
    let (fx, report) = fixtures::shared_dir(&["tone_fade.flac", "pickup.flac"]);
    if !report.skipped.is_empty() {
        eprintln!("SKIPPED (no ffmpeg?)");
        return;
    }
    let mut e = Env::new(
        "m2prev",
        &Opts {
            tones: 0,
            ..Opts::default()
        },
    );
    let a = item(&e, &fx.join("tone_fade.flac"));
    let b = item(&e, &fx.join("pickup.flac"));
    e.spawn_library();
    let lb = e.lb();

    // Acceptance 2: one pair, every preset, sample for sample (32-bit float, no quantization).
    for preset in ["basic", "plexamp", "sam", "fm", "none"] {
        let (wav, meta) = preview(&lb, &format!("from={a}&to={b}&preset={preset}&depth=32"));
        let got = wav_f32(&wav);
        let want = scenario_pair(&e, a, b, preset, json!({}));
        let same = got.len() == want.samples.len()
            && got
                .iter()
                .zip(&want.samples)
                .all(|(x, y)| x.to_bits() == y.to_bits());
        let maxdiff = got
            .iter()
            .zip(&want.samples)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        println!(
            "{preset:8} preview {} samples, scenario {} samples, identical {same}, max |diff| {maxdiff:e}, overlap {} frames, kind {}",
            got.len(), want.samples.len(), meta["boundary"]["overlap_frames"], meta["boundary"]["kind"]
        );
        if !same {
            println!("preview boundary {}", meta["boundary"]);
            println!("preview a {} b {}", meta["a"], meta["b"]);
            println!("scenario boundary {:?}", want.sidecar.boundaries);
            println!(
                "scenario voices {:?}",
                want.sidecar
                    .voices
                    .iter()
                    .map(|v| (
                        v.file_start_ms,
                        v.start_frame,
                        v.attack_frame,
                        v.segue_frame,
                        v.cue_out_frame,
                        v.gain_db
                    ))
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            same,
            "{preset}: the preview differs from the scenario renderer"
        );
    }

    // Acceptance 3: a saved marker is the effective marker, and the preview moves with it.
    let base = {
        let it = get(&lb, &format!("/api/v1/items/{a}")).json();
        it["effective"]["segue"].as_i64().unwrap()
    };
    let (w0, m0) = preview(&lb, &format!("from={a}&to={b}&preset=sam&depth=32"));
    let new_segue = base - 1500;
    let r = send(
        &lb,
        "PUT",
        &format!("/api/v1/items/{a}/overrides"),
        &json!({"segue": new_segue}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["effective"]["segue"], new_segue);
    let it = get(&lb, &format!("/api/v1/items/{a}")).json();
    assert_eq!(it["effective"]["segue"], new_segue);
    assert_eq!(it["overrides"]["segue"], new_segue);
    let (w1, m1) = preview(&lb, &format!("from={a}&to={b}&preset=sam&depth=32"));
    let (s0, s1) = (
        m0["boundary"]["s_b"].as_i64().unwrap(),
        m1["boundary"]["s_b"].as_i64().unwrap(),
    );
    println!("segue {base} -> {new_segue} ms: B starts at frame {s0} -> {s1} ({} frames, expected about {})", s1 - s0, -1500 * 48);
    assert_eq!(m1["a"]["markers"]["segue"], new_segue);
    assert!(
        ((s1 - s0) - (-1500 * 48)).abs() <= 48,
        "B must start 1.5 s earlier (within 1 ms)"
    );
    assert_ne!(wav_f32(&w0), wav_f32(&w1));
    // And still identical to the scenario renderer given the same override.
    let want = scenario_pair(&e, a, b, "sam", json!({"segue": new_segue}));
    let got = wav_f32(&w1);
    assert!(
        got.len() == want.samples.len()
            && got
                .iter()
                .zip(&want.samples)
                .all(|(x, y)| x.to_bits() == y.to_bits()),
        "override preview differs from the scenario renderer"
    );
    // The cache is keyed on the library revision: the same query before the save gave the old result.
    assert_ne!(s0, s1);

    // The picker: presets differ, `station` is the plexamp default plus the item and category layers.
    let (_, ms) = preview(&lb, &format!("from={a}&to={b}&preset=station"));
    let (_, mp) = preview(&lb, &format!("from={a}&to={b}&preset=plexamp"));
    assert_eq!(
        ms["boundary"]["overlap_frames"],
        mp["boundary"]["overlap_frames"]
    );
    // An item's own transition setting (2.6 step 3) is used by `station` but loses to a chosen preset.
    let r = send(
        &lb,
        "PUT",
        &format!("/api/v1/items/{b}/overrides"),
        &json!({"params": {"transition": {"preset": "none"}}}),
    );
    assert_eq!(r.status, 200, "{}", r.text());
    let (_, mi) = preview(&lb, &format!("from={a}&to={b}&preset=station"));
    let (_, mf) = preview(&lb, &format!("from={a}&to={b}&preset=fm"));
    println!(
        "item preset none on B: station overlap {} -> {} frames; under fm still {} frames",
        ms["boundary"]["overlap_frames"],
        mi["boundary"]["overlap_frames"],
        mf["boundary"]["overlap_frames"]
    );
    assert_ne!(
        ms["boundary"]["overlap_frames"],
        mi["boundary"]["overlap_frames"]
    );
    // A patch on the one boundary.
    let (_, mq) = preview(
        &lb,
        &format!(
            "from={a}&to={b}&preset=basic&params=%7B%22align%22%3A%7B%22overlap_s%22%3A2%7D%7D"
        ),
    );
    assert_eq!(
        mq["boundary"]["overlap_frames"],
        2 * 48000,
        "{}",
        mq["boundary"]
    );
    // Errors.
    assert_eq!(
        get(
            &lb,
            &format!("/api/v1/preview/transition?from={a}&to={b}&preset=nope")
        )
        .status,
        400
    );
    assert_eq!(
        get(
            &lb,
            &format!("/api/v1/preview/transition?from={a}&to={b}&params=%7Bnope")
        )
        .status,
        400
    );
}
