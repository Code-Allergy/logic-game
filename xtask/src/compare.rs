//! `perf compare <base> [<head>]`: compare two results.
//!
//! Each side is a result file (`*.json`, from `perf bench`) or a git
//! revision. A revision is checked out with `git worktree add` under
//! `<target>/perf/worktrees/`, benchmarked there with this xtask's suite
//! list (suites the revision lacks are skipped) into the shared target
//! directory, and the worktree is removed afterwards.
//!
//! Without `<head>`: a file base compares with `<target>/perf/latest.json`,
//! a revision base with the working tree.
//!
//! For every benchmark in both, the table shows both times, the change of
//! the point estimate, and its noise bounds: the change from the base's
//! upper to the head's lower confidence bound, and from the base's lower
//! to the head's upper one. A change is called only when that whole range
//! is beyond `--noise` (2% by default); otherwise it reads `~`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;

use crate::bench::{self, Plan};
use crate::util::{self, Args};

pub fn cmd(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(
        args,
        &["suite", "filter", "noise"],
        &["quick", "no-metrics"],
    )?;
    let plan = Plan::from_args(&a)?;
    let noise: f64 = a
        .opt("noise")
        .map_or(Ok(2.0), str::parse)
        .map_err(|e| format!("--noise: {e}"))?;
    let (base, head) = match a.positional.as_slice() {
        [b] => {
            let head = if is_file(b) {
                Side::File(util::perf_dir().join("latest.json"))
            } else {
                Side::WorkingTree
            };
            (side(b), head)
        }
        [b, h] => (side(b), side(h)),
        _ => return Err("usage: perf compare <base> [<head>]".into()),
    };
    let base = load(&base, &plan)?;
    let head = load(&head, &plan)?;
    print_table(&base, &head, noise);
    Ok(ExitCode::SUCCESS)
}

enum Side {
    File(PathBuf),
    Rev(String),
    WorkingTree,
}

fn is_file(s: &str) -> bool {
    s.ends_with(".json") || Path::new(s).is_file()
}

fn side(s: &str) -> Side {
    if is_file(s) {
        Side::File(PathBuf::from(s))
    } else {
        Side::Rev(s.to_string())
    }
}

fn load(side: &Side, plan: &Plan) -> Result<Value, String> {
    let root = util::repo_root();
    match side {
        Side::File(p) => util::read_json(p),
        Side::WorkingTree => {
            let label = bench::default_label(&root);
            let v = bench::run_in(&root, plan, &label)?;
            save(&label, &v)?;
            Ok(v)
        }
        Side::Rev(rev) => {
            let sha = util::git(
                &root,
                &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
            )
            .map_err(|_| format!("{rev:?} is neither a result file nor a git revision"))?;
            let short = &sha[..10];
            let label = format!("rev-{short}");
            let cached = util::perf_dir()
                .join("results")
                .join(format!("{label}.json"));
            if let Ok(v) = util::read_json(&cached)
                && v["quick"].as_bool() == Some(plan.quick)
                && plan.suites.is_empty()
                && plan.filter.is_none()
            {
                eprintln!(
                    "perf: using the earlier result for {rev} ({})",
                    cached.display()
                );
                return Ok(v);
            }
            let wt = util::perf_dir().join("worktrees").join(short);
            if wt.exists() {
                let _ = util::git(&root, &["worktree", "remove", "--force", &path_str(&wt)]);
            }
            util::git(
                &root,
                &["worktree", "add", "--detach", &path_str(&wt), &sha],
            )?;
            eprintln!("perf: {rev} ({short}) checked out in {}", wt.display());
            let result = bench::run_in(&wt, plan, &label);
            // Clean up whatever happened: worktrees are large.
            let removed = util::git(&root, &["worktree", "remove", "--force", &path_str(&wt)]);
            if let Err(e) = removed {
                eprintln!("perf: could not remove {}: {e}", wt.display());
            }
            let v = result?;
            save(&label, &v)?;
            Ok(v)
        }
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn save(label: &str, v: &Value) -> Result<(), String> {
    util::write_json(
        &util::perf_dir()
            .join("results")
            .join(format!("{label}.json")),
        v,
    )
}

fn describe(v: &Value) -> String {
    let rev = v["git"]["rev"]
        .as_str()
        .map_or("?", |r| &r[..r.len().min(10)]);
    let dirty = if v["git"]["dirty"].as_bool() == Some(true) {
        "+changes"
    } else {
        ""
    };
    let quick = if v["quick"].as_bool() == Some(true) {
        ", quick"
    } else {
        ""
    };
    format!(
        "{} ({rev}{dirty}{quick})",
        v["label"].as_str().unwrap_or("?")
    )
}

pub fn print_table(base: &Value, head: &Value, noise: f64) {
    println!("base: {}\nhead: {}\n", describe(base), describe(head));
    let empty = serde_json::Map::new();
    let b = base["benchmarks"].as_object().unwrap_or(&empty);
    let h = head["benchmarks"].as_object().unwrap_or(&empty);
    let mut ids: Vec<&String> = b.keys().filter(|k| h.contains_key(*k)).collect();
    ids.sort();
    println!(
        "{:<54} {:>11} {:>11} {:>8} {:>19}  verdict",
        "benchmark", "base", "head", "change", "noise bounds"
    );
    let (mut faster, mut slower) = (0, 0);
    for id in &ids {
        let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(f64::NAN);
        let (bv, hv) = (&b[*id], &h[*id]);
        let change = (f(hv, "ns") / f(bv, "ns") - 1.0) * 100.0;
        let lo = (f(hv, "lo") / f(bv, "hi") - 1.0) * 100.0;
        let hi = (f(hv, "hi") / f(bv, "lo") - 1.0) * 100.0;
        let verdict = if lo > noise {
            slower += 1;
            "slower"
        } else if hi < -noise {
            faster += 1;
            "faster"
        } else {
            "~"
        };
        println!(
            "{:<54} {:>11} {:>11} {:>+7.1}% [{:>+7.1}%,{:>+7.1}%]  {verdict}",
            id,
            util::fmt_ns(f(bv, "ns")),
            util::fmt_ns(f(hv, "ns")),
            change,
            lo,
            hi
        );
    }
    let only = |x: &serde_json::Map<String, Value>, y: &serde_json::Map<String, Value>| {
        x.keys().filter(|k| !y.contains_key(*k)).count()
    };
    println!(
        "\n{} compared: {faster} faster, {slower} slower, {} within noise ({noise}%); \
         {} only in base, {} only in head",
        ids.len(),
        ids.len() - faster - slower,
        only(b, h),
        only(h, b)
    );
    // Deterministic metrics: list only the ones that changed.
    let (Some(bm), Some(hm)) = (base["metrics"].as_object(), head["metrics"].as_object()) else {
        return;
    };
    let mut changed: Vec<(&String, u64, u64)> = bm
        .iter()
        .filter_map(|(k, v)| {
            let (x, y) = (v["value"].as_u64()?, hm.get(k)?["value"].as_u64()?);
            (x != y).then_some((k, x, y))
        })
        .collect();
    changed.sort();
    if changed.is_empty() {
        println!("metrics: no changes ({} compared)", bm.len());
        return;
    }
    println!(
        "\n{:<54} {:>12} {:>12} {:>8}",
        "metric", "base", "head", "change"
    );
    for (k, x, y) in changed {
        let pct = if x == 0 {
            f64::INFINITY
        } else {
            (y as f64 / x as f64 - 1.0) * 100.0
        };
        println!("{k:<54} {x:>12} {y:>12} {pct:>+7.1}%");
    }
}
