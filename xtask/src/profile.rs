//! `perf flamegraph` and `perf stat`: Linux `perf` on one target, built
//! with the `profiling` profile (optimized, no LTO, line tables; the same one
//! `perf bench` uses, so the two share one build).
//!
//! Targets:
//! - `example:<name>`: an example of the game (e.g. `example:cpu_bench`,
//!   with `-- 500000 cpu16` to profile one CPU's tick loop);
//! - `bench:<suite>[/<filter>]`: a criterion suite run with
//!   `--profile-time 10` (no analysis, just the routine), optionally only
//!   the benchmarks matching `<filter>`;
//! - `bin:perf-metrics`.
//!
//! The flamegraph is drawn by `inferno` from `perf script` output:
//! `<target>/perf/profiles/<name>.svg`, with the folded stacks beside it.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use inferno::collapse::Collapse;

use crate::bench::SUITES;
use crate::util::{self, Args};

struct Target {
    /// For file names.
    slug: String,
    exe: PathBuf,
    args: Vec<String>,
}

/// The cargo profile targets are built with (see the workspace
/// `Cargo.toml`).
const PROFILE: &str = "profiling";

/// Build `spec` with [`PROFILE`] and find its executable.
fn build(spec: &str, passthrough: &[String]) -> Result<Target, String> {
    let root = util::repo_root();
    let (kind, name) = spec
        .split_once(':')
        .ok_or_else(|| format!("target {spec:?}: use example:<name>, bench:<suite>, bin:<name>"))?;
    let mut cargo = util::cargo(&root);
    cargo.args([
        "build",
        "--profile",
        PROFILE,
        "--message-format=json-render-diagnostics",
    ]);
    let (artifact, mut args, slug) = match kind {
        "example" => {
            cargo.args(["-p", "gate_quest", "--example", name]);
            (name.to_string(), Vec::new(), format!("example-{name}"))
        }
        "bench" => {
            let (suite, filter) = name
                .split_once('/')
                .map_or((name, None), |(s, f)| (s, Some(f)));
            let s = SUITES
                .iter()
                .find(|s| s.name == suite)
                .ok_or_else(|| format!("no suite {suite:?} (see `cargo xtask perf list`)"))?;
            cargo.args(["-p", s.package, "--bench", s.bench]);
            let mut args: Vec<String> = ["--bench", "--profile-time", "10"]
                .map(String::from)
                .to_vec();
            args.extend(filter.map(String::from));
            let slug = format!("bench-{}", name.replace(['/', ' '], "_"));
            (s.bench.to_string(), args, slug)
        }
        "bin" => {
            cargo.args(["-p", name]);
            (name.to_string(), Vec::new(), format!("bin-{name}"))
        }
        _ => return Err(format!("target kind {kind:?}: use example, bench or bin")),
    };
    args.extend(passthrough.iter().cloned());
    eprintln!("perf: building {spec} (profile {PROFILE})");
    let out = util::output(cargo.stdout(Stdio::piped()))?;
    let artifact = artifact.replace('-', "_");
    let exe = out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["reason"] == "compiler-artifact")
        .filter(|v| {
            v["target"]["name"]
                .as_str()
                .is_some_and(|n| n.replace('-', "_") == artifact)
        })
        .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
        .next_back()
        .ok_or_else(|| format!("no executable built for {spec}"))?;
    Ok(Target { slug, exe, args })
}

fn perf_or_hint() -> Result<PathBuf, ExitCode> {
    util::find_perf().ok_or_else(|| {
        eprintln!("{}", util::PERF_HINT);
        ExitCode::from(2)
    })
}

pub fn flamegraph(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["call-graph", "freq"], &[])?;
    let [spec] = a.positional.as_slice() else {
        return Err("usage: perf flamegraph <target> [-- args]".into());
    };
    let perf = match perf_or_hint() {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let t = build(spec, &a.passthrough)?;
    let dir = util::perf_dir().join("profiles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let data = dir.join(format!("{}.data", t.slug));
    let call_graph = match a.opt("call-graph").unwrap_or("dwarf") {
        "dwarf" => "dwarf,32768",
        "fp" => "fp",
        "lbr" => "lbr",
        other => return Err(format!("--call-graph {other:?}: dwarf, fp or lbr")),
    };
    let freq = a.opt("freq").unwrap_or("997");
    eprintln!("perf: recording {} {}", t.exe.display(), t.args.join(" "));
    util::run(
        Command::new(&perf)
            .args(["record", "-F", freq, "-g", "--call-graph", call_graph, "-o"])
            .arg(&data)
            .arg("--")
            .arg(&t.exe)
            .args(&t.args)
            .current_dir(util::repo_root()),
    )?;
    eprintln!("perf: folding stacks");
    let script = dir.join(format!("{}.perf.txt", t.slug));
    let status = Command::new(&perf)
        .args(["script", "-i"])
        .arg(&data)
        .stdout(File::create(&script).map_err(|e| e.to_string())?)
        .status()
        .map_err(|e| format!("perf script: {e}"))?;
    if !status.success() {
        return Err(format!("perf script failed ({status})"));
    }
    let folded = dir.join(format!("{}.folded", t.slug));
    let mut folder =
        inferno::collapse::perf::Folder::from(inferno::collapse::perf::Options::default());
    folder
        .collapse(
            BufReader::new(File::open(&script).map_err(|e| e.to_string())?),
            BufWriter::new(File::create(&folded).map_err(|e| e.to_string())?),
        )
        .map_err(|e| format!("collapsing stacks: {e}"))?;
    let _ = std::fs::remove_file(&script);
    let svg = dir.join(format!("{}.svg", t.slug));
    let mut opts = inferno::flamegraph::Options::default();
    opts.title = format!("{spec} {}", a.passthrough.join(" "));
    opts.count_name = "samples".into();
    inferno::flamegraph::from_files(
        &mut opts,
        std::slice::from_ref(&folded),
        BufWriter::new(File::create(&svg).map_err(|e| e.to_string())?),
    )
    .map_err(|e| format!("drawing the flamegraph: {e}"))?;
    println!("flamegraph: {}", svg.display());
    println!("folded stacks: {}", folded.display());
    println!("perf data: {} (`perf report -i` it)", data.display());
    Ok(ExitCode::SUCCESS)
}

pub fn stat(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["events"], &[])?;
    let [spec] = a.positional.as_slice() else {
        return Err("usage: perf stat <target> [-- args]".into());
    };
    let perf = match perf_or_hint() {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let t = build(spec, &a.passthrough)?;
    let events = a.opt("events").unwrap_or(
        "task-clock,cycles,instructions,branches,branch-misses,cache-references,cache-misses",
    );
    let out = Command::new(&perf)
        .args(["stat", "-e", events, "--"])
        .arg(&t.exe)
        .args(&t.args)
        .current_dir(util::repo_root())
        .stdout(Stdio::inherit())
        .output()
        .map_err(|e| format!("perf stat: {e}"))?;
    let text = String::from_utf8_lossy(&out.stderr);
    eprint!("{text}");
    if text.contains("<not supported>") || text.contains("<not counted>") {
        eprintln!(
            "note: some hardware counters are not available here (common in VMs and \
             containers); software events such as task-clock still are."
        );
    }
    Ok(if out.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
