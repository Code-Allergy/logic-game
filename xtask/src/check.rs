//! `perf check` and `perf bless`: the regression gate on deterministic
//! metrics (`perf-metrics`), against the checked-in `perf/baseline.json`.
//!
//! The baseline:
//!
//! ```json
//! { "schema": 1,
//!   "thresholds": { "size": 3.0, "ops": 3.0, "nodes": 3.0,
//!                   "allocs": 0, "instructions": 3.0 },
//!   "metrics": { "<name>": { "kind": "ops", "value": 1234 } } }
//! ```
//!
//! A metric fails when it grows beyond its kind's threshold: a percentage
//! for every kind but `allocs`, where it is an absolute count (0: any new
//! allocation fails). A metric that shrinks beyond the threshold passes
//! with a note to bless the new value; a missing one fails (bless when it
//! was removed on purpose); a new one is reported.
//!
//! `instructions` metrics (instructions per tick from `perf stat`) are
//! measured only when the baseline has some (`perf bless --instructions`)
//! and the machine reports the counter; otherwise they are skipped with a
//! note. Wall-clock times are never gated: see `perf compare`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::{Map, Value, json};

use crate::bench;
use crate::util::{self, Args};

fn default_thresholds() -> Value {
    json!({ "size": 3.0, "ops": 3.0, "nodes": 3.0, "allocs": 0, "instructions": 3.0 })
}

fn baseline_path(a: &Args) -> PathBuf {
    a.opt("baseline").map_or_else(
        || util::repo_root().join("perf/baseline.json"),
        PathBuf::from,
    )
}

fn measure(a: &Args) -> Result<Map<String, Value>, String> {
    let m = match a.opt("metrics") {
        Some(p) => {
            let v = util::read_json(Path::new(p))?;
            // A `perf bench` result or raw `perf-metrics` output.
            v.get("metrics").cloned().unwrap_or(v)
        }
        None => bench::run_metrics(&util::repo_root())?.ok_or("no perf-metrics package")?,
    };
    m.as_object()
        .cloned()
        .ok_or_else(|| "metrics are not an object".into())
}

pub fn check(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["metrics", "baseline"], &[])?;
    let path = baseline_path(&a);
    let baseline = util::read_json(&path)?;
    let mut head = measure(&a)?;
    let base = baseline["metrics"]
        .as_object()
        .ok_or("baseline has no metrics")?;
    let thresholds = &baseline["thresholds"];
    let wants_instr = base.values().any(|v| v["kind"] == "instructions");
    if wants_instr && a.opt("metrics").is_none() {
        match measure_instructions()? {
            Some(m) => head.extend(m),
            None => eprintln!(
                "perf: the baseline has instruction counts but this machine cannot measure them \
                 (no perf, or no hardware counters): skipped"
            ),
        }
    }

    let mut failures = Vec::new();
    let mut improved = Vec::new();
    let mut checked = 0;
    for (name, b) in base {
        let kind = b["kind"].as_str().unwrap_or("?");
        let bv = b["value"].as_u64().unwrap_or(0);
        let Some(h) = head.get(name) else {
            if kind == "instructions" && !wants_measured(&head) {
                continue;
            }
            failures.push(format!(
                "{name}: missing (bless if it was removed on purpose)"
            ));
            continue;
        };
        checked += 1;
        let hv = h["value"].as_u64().unwrap_or(0);
        let t = thresholds[kind]
            .as_f64()
            .or_else(|| default_thresholds()[kind].as_f64())
            .unwrap_or(0.0);
        let (worse, better) = if kind == "allocs" {
            (hv as f64 > bv as f64 + t, (hv as f64) < bv as f64 - t)
        } else {
            let limit = bv as f64 * t / 100.0;
            (
                hv as f64 > bv as f64 + limit,
                (hv as f64) < bv as f64 - limit,
            )
        };
        let pct = if bv == 0 {
            if hv == 0 { 0.0 } else { f64::INFINITY }
        } else {
            (hv as f64 / bv as f64 - 1.0) * 100.0
        };
        let unit = if kind == "allocs" { "" } else { "%" };
        let line = format!("{name}: {bv} -> {hv} ({pct:+.2}%, limit {t}{unit}, {kind})");
        if worse {
            failures.push(line);
        } else if better {
            improved.push(line);
        }
    }
    let new: Vec<&String> = head.keys().filter(|k| !base.contains_key(*k)).collect();

    println!("perf check: {checked} metrics against {}", path.display());
    if !improved.is_empty() {
        println!("\nimproved beyond the threshold (run `cargo xtask perf bless` to keep it):");
        for l in &improved {
            println!("  {l}");
        }
    }
    if !new.is_empty() {
        println!("\nnew metrics, not in the baseline (bless to gate them):");
        for k in &new {
            println!("  {k} = {}", head[*k]["value"]);
        }
    }
    if failures.is_empty() {
        println!("\nOK: no regression.");
        return Ok(ExitCode::SUCCESS);
    }
    println!("\nREGRESSION ({}):", failures.len());
    for l in &failures {
        println!("  {l}");
    }
    println!(
        "\nIf the change is intended, run `cargo xtask perf bless` and commit perf/baseline.json \
         (see docs/performance.md)."
    );
    Ok(ExitCode::from(1))
}

fn wants_measured(head: &Map<String, Value>) -> bool {
    head.values().any(|v| v["kind"] == "instructions")
}

pub fn bless(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["metrics", "baseline"], &["instructions"])?;
    let path = baseline_path(&a);
    let old = util::read_json(&path).ok();
    let mut metrics = measure(&a)?;
    let had_instr = old.as_ref().is_some_and(|o| {
        o["metrics"]
            .as_object()
            .is_some_and(|m| m.values().any(|v| v["kind"] == "instructions"))
    });
    if a.flag("instructions") || had_instr {
        match measure_instructions()? {
            Some(m) => metrics.extend(m),
            None if a.flag("instructions") => {
                return Err("cannot measure instructions here (no perf, or no hardware \
                            counters)"
                    .into());
            }
            None => {
                eprintln!("perf: cannot measure instructions here; the blessed baseline drops them")
            }
        }
    }
    let thresholds = old
        .as_ref()
        .and_then(|o| o.get("thresholds").cloned())
        .unwrap_or_else(default_thresholds);
    let mut sorted: Vec<(String, Value)> = metrics.into_iter().collect();
    sorted.sort_by(|x, y| x.0.cmp(&y.0));
    let baseline = json!({
        "schema": 1,
        "about": "Deterministic performance metrics gated by `cargo xtask perf check`. \
                  Regenerate with `cargo xtask perf bless`; see docs/performance.md.",
        "thresholds": thresholds,
        "metrics": Value::Object(sorted.into_iter().collect()),
    });
    util::write_json(&path, &baseline)?;
    println!(
        "blessed {} metrics into {}",
        baseline["metrics"].as_object().map_or(0, Map::len),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// Instructions per tick of the gallery CPUs, from `perf stat` (two runs of
/// different lengths, so start-up cancels out). `None` when `perf` or the
/// hardware counter is unavailable.
fn measure_instructions() -> Result<Option<Map<String, Value>>, String> {
    let Some(perf) = util::find_perf() else {
        return Ok(None);
    };
    let root = util::repo_root();
    util::run(util::cargo(&root).args(["build", "-q", "-p", "perf-metrics"]))?;
    let bin = util::target_dir().join("debug/perf-metrics");
    let count = |case: &str, n: u64| -> Result<Option<u64>, String> {
        let out = Command::new(&perf)
            .args(["stat", "-x", ",", "-e", "instructions:u", "--"])
            .arg(&bin)
            .args(["ticks", case, &n.to_string()])
            .stdout(Stdio::null())
            .output()
            .map_err(|e| format!("perf stat: {e}"))?;
        let text = String::from_utf8_lossy(&out.stderr);
        Ok(text
            .lines()
            .find(|l| l.contains("instructions"))
            .and_then(|l| l.split(',').next())
            .and_then(|v| v.trim().parse().ok()))
    };
    let (n1, n2) = (20_000u64, 40_000u64);
    let mut m = Map::new();
    for case in ["cpu16", "cpu16_rtl"] {
        let (Some(a), Some(b)) = (count(case, n1)?, count(case, n2)?) else {
            return Ok(None);
        };
        let per_tick = b.saturating_sub(a) / (n2 - n1);
        m.insert(
            format!("instructions.{case}.default.per_tick"),
            json!({ "kind": "instructions", "value": per_tick }),
        );
    }
    Ok(Some(m))
}
