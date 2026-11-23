//! The native suite runner (`docs/suite-runner.md`): big CPU test suites
//! against a design, on every core.
//!
//! ```sh
//! cargo run --example nes_suite -- <workspace> <suite config> [options]
//! ```
//!
//! `<workspace>` is a workspace folder (holding `manifest.json`) or its
//! manifest; `<suite config>` a JSON file saying the suite kind (`harte`,
//! `nestest`, `klaus`, `blargg`), its files, and how its registers,
//! memory, bus and timing map onto the design. Options:
//!
//! - `--jobs N`: simulations run side by side (default: every core).
//! - `--only P[,P..]`: only the case files whose name matches a pattern
//!   (`*`, `?`; `a9` or `a9.json`).
//! - `--limit N`: at most N cases of each file.
//! - `--stop-after N`: stop once N cases failed.
//! - `--out DIR`: where the report goes (default: `<config>.report/`
//!   beside the config).
//! - `--unoptimized`: compile with function matching, word lifting and
//!   word cleanup off, to tell a design bug from a compiler bug.
//!
//! It prints a short summary and writes `report.md`, `report.json` and,
//! when a Harte case failed, `repro.json` (a `run` script for the MCP
//! server). The exit code is 0 when everything passed, 1 when something
//! failed, 2 when the suite could not run.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gate_quest::mcp::suite::{self, Config, RunOptions};

const USAGE: &str = "usage: nes_suite <workspace> <suite config> [--jobs N] [--only P[,P..]] \
                     [--limit N] [--stop-after N] [--out DIR] [--unoptimized]";

struct Args {
    workspace: PathBuf,
    config: PathBuf,
    out: Option<PathBuf>,
    options: RunOptions,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut options = RunOptions::default();
    let mut out = None;
    let mut i = 0;
    let count = |v: Option<&String>, flag: &str| -> Result<usize, String> {
        v.and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{flag} takes a number"))
    };
    while i < args.len() {
        let a = &args[i];
        let value = args.get(i + 1);
        match a.as_str() {
            "--jobs" | "-j" => {
                options.jobs = count(value, a)?.max(1);
                i += 1;
            }
            "--only" => {
                let v = value.ok_or("--only takes patterns")?;
                options
                    .only
                    .extend(v.split(',').filter(|p| !p.is_empty()).map(String::from));
                i += 1;
            }
            "--limit" => {
                options.limit = Some(count(value, a)?);
                i += 1;
            }
            "--stop-after" => {
                options.stop_after = Some(count(value, a)?.max(1));
                i += 1;
            }
            "--out" => {
                out = Some(PathBuf::from(value.ok_or("--out takes a folder")?));
                i += 1;
            }
            "--unoptimized" => options.unoptimized = true,
            "--help" | "-h" => return Err(USAGE.into()),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}\n{USAGE}")),
            _ => positional.push(PathBuf::from(a)),
        }
        i += 1;
    }
    let [workspace, config] =
        <[PathBuf; 2]>::try_from(positional).map_err(|_| USAGE.to_string())?;
    Ok(Args {
        workspace,
        config,
        out,
        options,
    })
}

/// `<config dir>/<config stem>.report`.
fn default_out(config: &Path) -> PathBuf {
    let stem = config
        .file_stem()
        .map_or_else(|| "suite".into(), |s| s.to_string_lossy().into_owned());
    config
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{stem}.report"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let run = || -> Result<(suite::Report, Vec<PathBuf>), String> {
        let ws = suite::read_workspace(&args.workspace)?;
        let config = Config::read(&args.config)?;
        let report = suite::run(
            &ws,
            &config,
            &args.options,
            (
                &args.workspace.display().to_string(),
                &args.config.display().to_string(),
            ),
        )?;
        let out = args
            .out
            .clone()
            .unwrap_or_else(|| default_out(&args.config));
        let written = report.write(&out)?;
        Ok((report, written))
    };
    match run() {
        Ok((report, written)) => {
            print!("{}", report.summary());
            if let Some(md) = written.first() {
                println!("report: {}", md.display());
            }
            if report.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("nes_suite: {e}");
            ExitCode::from(2)
        }
    }
}
