//! Rewrite save format version 6 files in canonical form, or check them.
//!
//! ```sh
//! cargo run --example demo_fmt                  # every demo in assets/demos
//! cargo run --example demo_fmt -- --check       # fail if one is not canonical
//! cargo run --example demo_fmt -- a.json b.json # these files
//! ```
//!
//! Canonical form is what `gate_quest::sim::source::to_text` prints (see
//! `docs/save-format.md`). The manifest is left alone.

use std::path::PathBuf;
use std::process::ExitCode;

use gate_quest::sim::source;

fn main() -> ExitCode {
    let mut check = false;
    let mut files: Vec<PathBuf> = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            _ => files.push(arg.into()),
        }
    }
    if files.is_empty() {
        let dir = PathBuf::from(gate_quest::sim::demos::DEMO_DIR);
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("assets/demos (run from the repository root)")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|e| e == "json")
                    && p.file_name().is_some_and(|n| n != "manifest.json")
            })
            .collect();
        found.sort();
        files = found;
    }
    let mut failed = false;
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                failed = true;
                continue;
            }
        };
        let file = match source::parse(&text) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                failed = true;
                continue;
            }
        };
        let canonical = source::to_text(&file);
        if canonical == text {
            continue;
        }
        if check {
            eprintln!("{}: not in canonical form", path.display());
            failed = true;
        } else if let Err(e) = std::fs::write(&path, canonical) {
            eprintln!("{}: {e}", path.display());
            failed = true;
        } else {
            println!("formatted {}", path.display());
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
