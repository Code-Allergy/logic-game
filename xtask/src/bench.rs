//! `perf bench`: run the criterion suites and collect one JSON result.
//!
//! Each suite runs with its own `CRITERION_HOME` under
//! `<target>/perf/criterion/<label>/<suite>`, so a result only holds what
//! this run measured. The result (`schema` 1):
//!
//! ```json
//! { "schema": 1, "label": "...", "git": { "rev": "...", "dirty": false },
//!   "time": 1700000000, "quick": false, "host": { "cpus": 8, "os": "linux" },
//!   "benchmarks": { "<group>/<id>": { "ns": 1.0, "lo": 0.9, "hi": 1.1,
//!                                      "elements": 1000 } },
//!   "metrics": { "<name>": { "kind": "size", "value": 42 } } }
//! ```
//!
//! `ns` is criterion's typical estimate (the slope, or the mean) of one
//! iteration, `lo`/`hi` its 95% confidence interval, `elements` the
//! declared throughput if any.

use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value, json};

use crate::util::{self, Args};

pub struct Suite {
    pub name: &'static str,
    pub package: &'static str,
    pub bench: &'static str,
    /// The bench's source, relative to the repository (a revision without
    /// it skips the suite).
    pub file: &'static str,
    pub about: &'static str,
}

pub const SUITES: &[Suite] = &[
    Suite {
        name: "sim",
        package: "logi-sim",
        bench: "sim",
        file: "logi-sim/benches/sim.rs",
        about: "tick throughput (random 10k-1M, sequential), compile time and passes, threads",
    },
    Suite {
        name: "engine",
        package: "logi-sim",
        bench: "engine",
        file: "logi-sim/benches/engine.rs",
        about: "engine overhead: batching, probe frames, memory probes, reset, install, worker",
    },
    Suite {
        name: "gallery",
        package: "perf",
        bench: "gallery",
        file: "perf/benches/gallery.rs",
        about: "cpu16 / cpu16_rtl ticks and compile",
    },
    Suite {
        name: "app",
        package: "perf",
        bench: "app",
        file: "perf/benches/app.rs",
        about: "documents, design_of, routing, edits, undo, challenge check, make chip",
    },
    Suite {
        name: "frame",
        package: "perf",
        bench: "frame",
        file: "perf/benches/frame.rs",
        about: "headless frame time: idle, drag, running CPU",
    },
];

/// What to run.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub quick: bool,
    pub suites: Vec<String>,
    pub filter: Option<String>,
    pub metrics: bool,
}

impl Plan {
    pub fn from_args(a: &Args) -> Result<Self, String> {
        let suites = a.all("suite");
        for s in &suites {
            if !SUITES.iter().any(|x| x.name == s) {
                return Err(format!("no suite {s:?} (see `cargo xtask perf list`)"));
            }
        }
        Ok(Plan {
            quick: a.flag("quick"),
            suites,
            filter: a.opt("filter").map(str::to_string),
            metrics: !a.flag("no-metrics"),
        })
    }
}

pub fn cmd(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(
        args,
        &["suite", "filter", "label", "out"],
        &["quick", "no-metrics"],
    )?;
    if !a.positional.is_empty() {
        return Err(format!("unexpected {:?}", a.positional));
    }
    let plan = Plan::from_args(&a)?;
    let root = util::repo_root();
    let label = a
        .opt("label")
        .map_or_else(|| default_label(&root), str::to_string);
    let result = run_in(&root, &plan, &label)?;
    let path = util::perf_dir()
        .join("results")
        .join(format!("{label}.json"));
    util::write_json(&path, &result)?;
    util::write_json(&util::perf_dir().join("latest.json"), &result)?;
    if let Some(out) = a.opt("out") {
        util::write_json(Path::new(out), &result)?;
    }
    print_summary(&result);
    println!("\nwrote {} (and latest.json)", path.display());
    Ok(ExitCode::SUCCESS)
}

/// `run-<unix time>-<short rev>`.
pub fn default_label(dir: &Path) -> String {
    let (rev, dirty) = util::git_state(dir);
    let rev = rev.map_or_else(|| "norev".into(), |r| r[..r.len().min(10)].to_string());
    format!(
        "run-{}-{rev}{}",
        util::now(),
        if dirty { "-dirty" } else { "" }
    )
}

/// Run `plan` in the checkout at `dir` and build the result.
pub fn run_in(dir: &Path, plan: &Plan, label: &str) -> Result<Value, String> {
    let home = util::perf_dir().join("criterion").join(label);
    let mut benchmarks = Map::new();
    for suite in SUITES {
        if !plan.suites.is_empty() && !plan.suites.iter().any(|s| s == suite.name) {
            continue;
        }
        if !dir.join(suite.file).exists() {
            eprintln!(
                "perf: suite {} is not in {} (skipped)",
                suite.name,
                dir.display()
            );
            continue;
        }
        let suite_home = home.join(suite.name);
        let _ = std::fs::remove_dir_all(&suite_home);
        eprintln!("perf: running suite {} ({})", suite.name, suite.about);
        let mut c = util::cargo(dir);
        define_profile(dir, &mut c);
        c.args([
            "bench",
            "--profile",
            "profiling",
            "-p",
            suite.package,
            "--bench",
            suite.bench,
            "--",
        ])
        .env("CRITERION_HOME", &suite_home);
        if let Some(f) = &plan.filter {
            c.arg(f);
        }
        c.arg("--noplot");
        if plan.quick {
            c.arg("--quick").env("PERF_QUICK", "1");
        }
        util::run(&mut c)?;
        collect(&suite_home, &mut benchmarks)?;
    }
    let metrics = if plan.metrics {
        run_metrics(dir)?
    } else {
        None
    };
    let (rev, dirty) = util::git_state(dir);
    Ok(json!({
        "schema": 1,
        "label": label,
        "git": { "rev": rev, "dirty": dirty },
        "time": util::now(),
        "quick": plan.quick,
        "host": {
            "cpus": std::thread::available_parallelism().map_or(1, |n| n.get()),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "benchmarks": Value::Object(benchmarks),
        "metrics": metrics.unwrap_or(Value::Null),
    }))
}

/// A revision from before the `profiling` profile existed gets it from
/// cargo's config environment (all but its logi-sim override), so both
/// sides of a comparison build the same way.
fn define_profile(dir: &Path, c: &mut std::process::Command) {
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
    if manifest.contains("[profile.profiling]") {
        return;
    }
    eprintln!(
        "perf: {} has no `profiling` profile; defining it",
        dir.display()
    );
    c.env("CARGO_PROFILE_PROFILING_INHERITS", "release")
        .env("CARGO_PROFILE_PROFILING_OPT_LEVEL", "3")
        .env("CARGO_PROFILE_PROFILING_LTO", "false")
        .env("CARGO_PROFILE_PROFILING_CODEGEN_UNITS", "16")
        .env("CARGO_PROFILE_PROFILING_DEBUG", "line-tables-only")
        .env("CARGO_PROFILE_PROFILING_STRIP", "false");
}

/// The deterministic metrics of the checkout at `dir` (`perf-metrics`),
/// or `None` if it has none.
pub fn run_metrics(dir: &Path) -> Result<Option<Value>, String> {
    if !dir.join("perf/metrics/Cargo.toml").exists() {
        eprintln!("perf: no perf-metrics in {} (skipped)", dir.display());
        return Ok(None);
    }
    eprintln!("perf: measuring metrics (perf-metrics)");
    let text = util::output(util::cargo(dir).args(["run", "-q", "-p", "perf-metrics"]))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("perf-metrics: {e}"))?;
    Ok(v.get("metrics").cloned())
}

/// Add every `new/` estimate under `home` to `out`.
fn collect(home: &Path, out: &mut Map<String, Value>) -> Result<(), String> {
    let mut stack = vec![home.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if p.file_name().is_some_and(|n| n == "new") {
                if let Some((id, v)) = read_estimate(&p)? {
                    out.insert(id, v);
                }
            } else {
                stack.push(p);
            }
        }
    }
    Ok(())
}

fn read_estimate(new: &Path) -> Result<Option<(String, Value)>, String> {
    let (b, e) = (new.join("benchmark.json"), new.join("estimates.json"));
    if !b.exists() || !e.exists() {
        return Ok(None);
    }
    let bench = util::read_json(&b)?;
    let est = util::read_json(&e)?;
    let id = bench["full_id"]
        .as_str()
        .ok_or("benchmark.json: no full_id")?;
    let typical = if est["slope"].is_object() {
        &est["slope"]
    } else {
        &est["mean"]
    };
    let ci = &typical["confidence_interval"];
    let mut v = json!({
        "ns": typical["point_estimate"],
        "lo": ci["lower_bound"],
        "hi": ci["upper_bound"],
    });
    if let Some(n) = bench["throughput"].get("Elements").and_then(Value::as_u64) {
        v["elements"] = json!(n);
    }
    Ok(Some((id.to_string(), v)))
}

fn print_summary(result: &Value) {
    let Some(b) = result["benchmarks"].as_object() else {
        return;
    };
    let mut ids: Vec<&String> = b.keys().collect();
    ids.sort();
    println!("\n{:<58} {:>12} {:>16}", "benchmark", "time", "throughput");
    for id in ids {
        let v = &b[id];
        let ns = v["ns"].as_f64().unwrap_or(0.0);
        let tp = v["elements"]
            .as_u64()
            .map(|n| format!("{:.1} Melem/s", n as f64 / ns * 1e3))
            .unwrap_or_default();
        println!("{:<58} {:>12} {:>16}", id, util::fmt_ns(ns), tp);
    }
    if let Some(m) = result["metrics"].as_object() {
        println!("\n{} deterministic metrics (see `perf check`)", m.len());
    }
}
