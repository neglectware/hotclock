//! `cargo run -p fixtures [-- --out DIR] [NAME ...]`: generate the synthetic test fixtures.

use fixtures::{default_dir, Generator, ALL};
use std::path::PathBuf;

fn main() {
    let mut out: Option<PathBuf> = None;
    let mut names: Vec<String> = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--list" => {
                for n in ALL {
                    println!("{n}");
                }
                return;
            }
            "-h" | "--help" => {
                println!("usage: fixtures [--out DIR] [--list] [NAME ...]\nGenerates all fixtures when no NAME is given. Default DIR: tests/fixtures/generated");
                return;
            }
            n => names.push(n.to_string()),
        }
    }
    let dir = out.unwrap_or_else(default_dir);
    std::fs::create_dir_all(&dir).expect("create output directory");
    let g = Generator::new(&dir);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let report = g.generate(&refs);
    for p in &report.written {
        println!("wrote {}", p.display());
    }
    for (n, why) in &report.skipped {
        println!("SKIPPED {n}: {why}");
    }
    if !report.skipped.is_empty() {
        std::process::exit(1);
    }
}
