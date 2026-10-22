//! The product name has exactly three spellings anywhere in the tree: `hotclock`, `HOTCLOCK`
//! (environment variables) and `Hotclock` (display name). Anything else (`Hot Clock`, `HotClock`,
//! `hot_clock`, `hot-clock`) makes a later rename more than three replaces (architecture 0).

use std::path::Path;

const ALLOWED: [&str; 3] = ["hotclock", "HOTCLOCK", "Hotclock"];

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if matches!(
                name.as_str(),
                "target" | "target-linux" | ".git" | "generated"
            ) {
                continue;
            }
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Every occurrence of the name in any spelling, as it is written.
fn spellings(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut found = vec![];
    let mut i = 0;
    while let Some(off) = lower[i..].find("hot") {
        let s = i + off;
        let mut j = s + 3;
        if j < b.len() && matches!(b[j], b' ' | b'_' | b'-') {
            j += 1;
        }
        if lower[j..].starts_with("clock") {
            found.push(text[s..j + 5].to_string());
        }
        i = s + 3;
    }
    found
}

#[test]
fn the_name_has_only_its_three_spellings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![];
    walk(&root, &mut files);
    let mut bad = vec![];
    let mut checked = 0;
    for f in files {
        if f.file_name().is_some_and(|n| n == "naming.rs") {
            continue; // this file names the wrong spellings on purpose
        }
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue; // binary
        };
        checked += 1;
        for s in spellings(&text) {
            if !ALLOWED.contains(&s.as_str()) {
                bad.push(format!("{}: `{s}`", f.display()));
            }
        }
    }
    println!("checked {checked} text files");
    assert!(checked > 20);
    assert!(
        bad.is_empty(),
        "wrong spellings of the name:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_checker_itself_catches_the_wrong_ones() {
    for wrong in [
        "Hot Clock",
        "HotClock",
        "hot_clock",
        "hot-clock",
        "Hot clock",
    ] {
        let s = spellings(&format!("x {wrong} y"));
        assert_eq!(s.len(), 1);
        assert!(!ALLOWED.contains(&s[0].as_str()), "{wrong}");
    }
    for right in ALLOWED {
        assert!(ALLOWED.contains(&spellings(right)[0].as_str()));
    }
}
