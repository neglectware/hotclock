//! `hotclock census <dir or m3u>`.

use analysis::census;
use clap::Args;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Args)]
pub struct CensusArgs {
    /// A directory (searched recursively), an `.m3u`/`.m3u8` list, or one file.
    path: PathBuf,
    /// Machine-readable output.
    #[arg(long)]
    json: bool,
}

pub fn run(a: CensusArgs) -> ExitCode {
    let files = match census::collect_files(&a.path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("census: {e}");
            return ExitCode::FAILURE;
        }
    };
    if files.is_empty() {
        eprintln!("census: no audio files under {}", a.path.display());
        return ExitCode::FAILURE;
    }
    let reports: Vec<_> = files.iter().map(|f| census::inspect(f)).collect();
    if a.json {
        let out = serde_json::json!({
            "summary": census::summarize(&reports),
            "files": reports,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        let base = if a.path.is_dir() {
            a.path.clone()
        } else {
            a.path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
        };
        print!("{}", census::format_text(&reports, &base));
    }
    ExitCode::SUCCESS
}
