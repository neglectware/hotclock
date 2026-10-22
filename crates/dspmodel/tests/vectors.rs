//! The shared vectors: `tests/vectors.json` is what the JavaScript port is held to. Regenerate with
//! `UPDATE_VECTORS=1 cargo test -p dspmodel`.
use std::path::PathBuf;

#[test]
fn vectors_file_is_current() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors.json");
    let now = dspmodel::vectors();
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(&p, serde_json::to_string_pretty(&now).unwrap() + "\n").unwrap();
    }
    let have: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).expect("vectors.json")).unwrap();
    let (a, b) = (have.as_array().unwrap(), now.as_array().unwrap());
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x["db"].as_array().unwrap(), y["db"].as_array().unwrap());
        for (p, q) in dx.iter().zip(dy) {
            assert!(
                (p.as_f64().unwrap() - q.as_f64().unwrap()).abs() < 1e-9,
                "{}",
                x["name"]
            );
        }
    }
}
