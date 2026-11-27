//! What a suite run found: a short summary for the terminal,
//! `report.md` (a scoreboard per case file and the first failing case of
//! each in full), `report.json`, and `repro.json`, a `run` script that
//! replays the first failing Harte case through the MCP server.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::harte::{Actual, Case, Cycle};
use super::{Kind, hex};
use crate::mcp::compiled::{Insight, Match};

/// What the compiler did with the circuit (the NES plan, 0.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Compile {
    /// Compiled with `func_match`, `lift` and `words` off.
    pub unoptimized: bool,
    /// Instructions with the default options and with the word passes off.
    pub instructions: usize,
    pub plain_instructions: usize,
    pub depth: u32,
    /// Each chip and what the function matcher made of it.
    pub chips: Vec<(String, String)>,
    pub matched: usize,
}

impl Compile {
    pub fn of(insight: &Insight, unoptimized: bool) -> Compile {
        let chips: Vec<(String, String)> = insight
            .chips
            .iter()
            .map(|(name, m)| {
                (
                    name.clone(),
                    match m {
                        Match::Matched(note) => format!("matched as {note}"),
                        Match::Unmatched(why) => format!("not matched: {why}"),
                        Match::NotAnalysed => {
                            "not analysed (a clock or memory is inside)".to_string()
                        }
                    },
                )
            })
            .collect();
        Compile {
            unoptimized,
            instructions: insight.size.instructions,
            plain_instructions: insight.plain_instructions,
            depth: insight.size.depth,
            matched: insight
                .chips
                .values()
                .filter(|m| matches!(m, Match::Matched(_)))
                .count(),
            chips,
        }
    }

    /// The instructions of the program the suite ran.
    pub fn used(&self) -> usize {
        if self.unoptimized {
            self.plain_instructions
        } else {
            self.instructions
        }
    }

    fn line(&self) -> String {
        format!(
            "{}: {} instructions ({} with the default options, {} without func_match, lift and \
             words); {} of {} chips matched",
            if self.unoptimized {
                "compiled without func_match, lift and words (--unoptimized)"
            } else {
                "compiled with the default options"
            },
            self.used(),
            self.instructions,
            self.plain_instructions,
            self.matched,
            self.chips.len()
        )
    }
}

/// One failing case: its index in the file, name and differences.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub index: usize,
    pub name: String,
    pub diffs: Vec<String>,
}

/// A failing case in full.
#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub index: usize,
    pub case: Case,
    pub actual: Actual,
    pub diffs: Vec<String>,
    /// The first cycle whose bus differs.
    pub first_cycle: Option<usize>,
}

/// The cases of one file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileResult {
    pub file: String,
    /// Cases read from the file (after `--limit`).
    pub cases: usize,
    /// Cases run (fewer after `--stop-after`).
    pub run: usize,
    pub failed: usize,
    /// The first failing cases (at most ten).
    pub failures: Vec<Failure>,
    pub story: Option<Story>,
}

/// What a program suite (nestest, klaus, blargg) found.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgramResult {
    pub passed: bool,
    /// One line: what happened.
    pub verdict: String,
    /// More lines: the differing log row and its context, the trap, the
    /// text the test printed.
    pub details: String,
    pub cycles: u64,
    /// Cycles at which the boundary condition held.
    pub instructions: u64,
    pub ticks: u64,
    pub seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Harte {
        registers: Vec<String>,
        widths: Vec<u8>,
        /// The bus was compared.
        bus: bool,
        files: Vec<FileResult>,
        /// `--stop-after` ended the run before every case ran.
        stopped: bool,
        repro: Option<Value>,
    },
    Program(ProgramResult),
}

/// A suite run.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub kind: Kind,
    pub workspace: String,
    pub config: String,
    pub circuit: String,
    pub compile: Compile,
    pub jobs: usize,
    /// Seconds spent running cases (compiling excluded).
    pub seconds: f64,
    pub ticks: u64,
    pub body: Body,
}

fn rate(n: f64, seconds: f64) -> String {
    if seconds <= 0.0 {
        return "-".into();
    }
    let r = n / seconds;
    if r >= 1e6 {
        format!("{:.2} M", r / 1e6)
    } else if r >= 1e3 {
        format!("{:.1} k", r / 1e3)
    } else {
        format!("{r:.1} ")
    }
}

fn cycle_cell(c: Option<&Cycle>) -> String {
    match c {
        Some(c) => format!(
            "0x{:04X} 0x{:02X} {}",
            c.addr,
            c.data,
            if c.read { "read" } else { "write" }
        ),
        None => "-".into(),
    }
}

impl Report {
    /// Every case ran and passed (Harte), or the program passed.
    pub fn ok(&self) -> bool {
        match &self.body {
            Body::Harte { files, stopped, .. } => {
                !stopped && files.iter().all(|f| f.failed == 0) && self.cases() > 0
            }
            Body::Program(p) => p.passed,
        }
    }

    /// Cases run (Harte), or 1.
    pub fn cases(&self) -> usize {
        match &self.body {
            Body::Harte { files, .. } => files.iter().map(|f| f.run).sum(),
            Body::Program(_) => 1,
        }
    }

    fn failed(&self) -> usize {
        match &self.body {
            Body::Harte { files, .. } => files.iter().map(|f| f.failed).sum(),
            Body::Program(p) => usize::from(!p.passed),
        }
    }

    fn speed(&self) -> String {
        let build = if cfg!(debug_assertions) {
            " (built with debug assertions)"
        } else {
            ""
        };
        match &self.body {
            Body::Harte { .. } => format!(
                "{:.2} s on {} job(s): {}cases/s, {}ticks/s{build}",
                self.seconds,
                self.jobs,
                rate(self.cases() as f64, self.seconds),
                rate(self.ticks as f64, self.seconds)
            ),
            Body::Program(p) => format!(
                "{:.2} s: {} cycles, {} instructions; {}cycles/s, {}ticks/s{build}",
                p.seconds,
                p.cycles,
                p.instructions,
                rate(p.cycles as f64, p.seconds),
                rate(p.ticks as f64, p.seconds)
            ),
        }
    }

    /// A few lines for the terminal.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        match &self.body {
            Body::Harte { files, stopped, .. } => {
                let failing = files.iter().filter(|f| f.failed > 0).count();
                let _ = writeln!(
                    out,
                    "{} {}: {} of {} cases passed in {} file(s){}{}",
                    self.kind.name(),
                    if self.ok() { "PASSED" } else { "FAILED" },
                    self.cases() - self.failed(),
                    self.cases(),
                    files.len(),
                    if failing > 0 {
                        format!("; {failing} file(s) with failures")
                    } else {
                        String::new()
                    },
                    if *stopped { " (stopped early)" } else { "" }
                );
                if let Some(f) = files.iter().find(|f| f.failed > 0)
                    && let Some(first) = f.failures.first()
                {
                    let _ = writeln!(
                        out,
                        "first failure: {} case {} \"{}\": {}",
                        f.file,
                        first.index,
                        first.name,
                        first.diffs.first().cloned().unwrap_or_default()
                    );
                }
            }
            Body::Program(p) => {
                let _ = writeln!(
                    out,
                    "{} {}: {}",
                    self.kind.name(),
                    if p.passed { "PASSED" } else { "FAILED" },
                    p.verdict
                );
            }
        }
        let _ = writeln!(out, "{}", self.compile.line());
        let _ = writeln!(out, "{}", self.speed());
        out
    }

    /// The Markdown report.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# {} suite on \"{}\": {}\n",
            self.kind.name(),
            self.circuit,
            if self.ok() { "passed" } else { "FAILED" }
        );
        let _ = writeln!(out, "- Workspace: `{}`", self.workspace);
        let _ = writeln!(out, "- Config: `{}`", self.config);
        let _ = writeln!(out, "- {}", self.compile.line());
        let _ = writeln!(out, "- {}", self.speed());
        match &self.body {
            Body::Harte {
                registers,
                widths,
                bus,
                files,
                stopped,
                repro,
            } => {
                let _ = writeln!(
                    out,
                    "- {} of {} cases passed{}; the bus was {}compared",
                    self.cases() - self.failed(),
                    self.cases(),
                    if *stopped {
                        " (stopped early by --stop-after)"
                    } else {
                        ""
                    },
                    if *bus { "" } else { "not " }
                );
                if repro.is_some() {
                    let _ = writeln!(
                        out,
                        "- `repro.json` replays the first failing case: a `run` script for the \
                         MCP server (its `file` argument), on a fresh simulation"
                    );
                }
                let _ = writeln!(out, "\n## Scoreboard\n");
                let _ = writeln!(out, "| File | Passed | Total | First failure |");
                let _ = writeln!(out, "|---|---:|---:|---|");
                for f in files {
                    let first = f.failures.first().map_or(String::new(), |x| {
                        format!(
                            "case {} `{}`: {}",
                            x.index,
                            x.name,
                            x.diffs.first().cloned().unwrap_or_default()
                        )
                    });
                    if f.run == 0 && *stopped {
                        let _ = writeln!(out, "| {} | - | - | not run (stopped early) |", f.file);
                        continue;
                    }
                    let _ = writeln!(
                        out,
                        "| {} | {} | {} | {} |",
                        f.file,
                        f.run - f.failed,
                        f.run,
                        first
                    );
                }
                for f in files.iter().filter(|f| f.story.is_some()) {
                    let s = f.story.as_ref().expect("story");
                    story_markdown(&mut out, f, s, registers, widths, *bus);
                }
            }
            Body::Program(p) => {
                let _ = writeln!(out, "\n## Result\n\n{}\n", p.verdict);
                if !p.details.is_empty() {
                    let _ = writeln!(out, "```\n{}\n```", p.details.trim_end());
                }
            }
        }
        let _ = writeln!(out, "\n## Compiler\n");
        let _ = writeln!(
            out,
            "{} instructions with the default options (depth {}), {} without func_match, lift \
             and words. This run used {}.\n",
            self.compile.instructions,
            self.compile.depth,
            self.compile.plain_instructions,
            if self.compile.unoptimized {
                "the unoptimized program"
            } else {
                "the default options"
            }
        );
        if !self.compile.chips.is_empty() {
            let _ = writeln!(out, "| Chip | Function matcher |");
            let _ = writeln!(out, "|---|---|");
            for (name, m) in &self.compile.chips {
                let _ = writeln!(out, "| {name} | {} |", m.replace('|', "\\|"));
            }
        }
        out
    }

    /// The JSON report.
    pub fn json(&self) -> Value {
        let mut v = json!({
            "suite": self.kind.name(),
            "ok": self.ok(),
            "workspace": self.workspace,
            "config": self.config,
            "circuit": self.circuit,
            "compile": {
                "options": if self.compile.unoptimized { "unoptimized" } else { "default" },
                "instructions": self.compile.instructions,
                "plain_instructions": self.compile.plain_instructions,
                "depth": self.compile.depth,
                "chips_matched": self.compile.matched,
            },
            "jobs": self.jobs,
            "seconds": self.seconds,
            "ticks": self.ticks,
        });
        v["compile"]["chips"] = Value::Object(
            self.compile
                .chips
                .iter()
                .map(|(n, m)| (n.clone(), Value::String(m.clone())))
                .collect(),
        );
        match &self.body {
            Body::Harte {
                registers,
                files,
                stopped,
                bus,
                ..
            } => {
                v["cases"] = json!(self.cases());
                v["passed"] = json!(self.cases() - self.failed());
                v["failed"] = json!(self.failed());
                v["stopped"] = json!(stopped);
                v["bus_compared"] = json!(bus);
                v["cases_per_second"] = json!(self.cases() as f64 / self.seconds.max(1e-9));
                v["ticks_per_second"] = json!(self.ticks as f64 / self.seconds.max(1e-9));
                v["files"] = files
                    .iter()
                    .map(|f| {
                        let mut o = json!({
                            "file": f.file,
                            "cases": f.cases,
                            "run": f.run,
                            "passed": f.run - f.failed,
                            "failed": f.failed,
                            "failures": f.failures.iter().map(|x| json!({
                                "index": x.index, "name": x.name, "diffs": x.diffs
                            })).collect::<Vec<_>>(),
                        });
                        if let Some(s) = &f.story {
                            o["first_failure"] = story_json(s, registers);
                        }
                        o
                    })
                    .collect();
            }
            Body::Program(p) => {
                v["passed"] = json!(p.passed);
                v["verdict"] = json!(p.verdict);
                v["details"] = json!(p.details);
                v["cycles"] = json!(p.cycles);
                v["instructions"] = json!(p.instructions);
                v["seconds"] = json!(p.seconds);
                v["cycles_per_second"] = json!(p.cycles as f64 / p.seconds.max(1e-9));
                v["ticks_per_second"] = json!(p.ticks as f64 / p.seconds.max(1e-9));
            }
        }
        v
    }

    /// Write `report.md`, `report.json` and, when a Harte case failed,
    /// `repro.json` into `dir`; the files written.
    pub fn write(&self, dir: &Path) -> Result<Vec<PathBuf>, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut written = Vec::new();
        let mut put = |name: &str, text: String| -> Result<(), String> {
            let path = dir.join(name);
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(path);
            Ok(())
        };
        put("report.md", self.markdown())?;
        put(
            "report.json",
            serde_json::to_string_pretty(&self.json()).unwrap_or_default(),
        )?;
        let repro = dir.join("repro.json");
        match &self.body {
            Body::Harte {
                repro: Some(script),
                ..
            } => put(
                "repro.json",
                serde_json::to_string_pretty(script).unwrap_or_default(),
            )?,
            _ => {
                // A replay of an earlier run would mislead.
                let _ = std::fs::remove_file(repro);
            }
        }
        Ok(written)
    }
}

fn story_json(s: &Story, registers: &[String]) -> Value {
    let regs = |values: &[u64]| -> Value {
        Value::Object(
            registers
                .iter()
                .zip(values)
                .map(|(n, v)| (n.clone(), json!(v)))
                .collect(),
        )
    };
    let cycles = |c: &[Cycle]| -> Value {
        c.iter()
            .map(|c| json!([c.addr, c.data, if c.read { "read" } else { "write" }]))
            .collect()
    };
    json!({
        "index": s.index,
        "name": s.case.name,
        "diffs": s.diffs,
        "initial": {"registers": regs(&s.case.init), "ram": s.case.init_ram},
        "expected": {"registers": regs(&s.case.fin), "ram": s.case.fin_ram, "cycles": cycles(&s.case.cycles)},
        "actual": {"registers": regs(&s.actual.regs), "ram": s.actual.ram, "cycles": cycles(&s.actual.cycles)},
        "first_differing_cycle": s.first_cycle.map(|i| i + 1),
    })
}

/// Cycles shown before and after the first differing one.
const CONTEXT: usize = 3;

fn story_markdown(
    out: &mut String,
    f: &FileResult,
    s: &Story,
    registers: &[String],
    widths: &[u8],
    bus: bool,
) {
    let case = &s.case;
    let _ = writeln!(out, "\n## {}: case {} `{}`\n", f.file, s.index, case.name);
    for d in &s.diffs {
        let _ = writeln!(out, "- {d}");
    }
    let _ = writeln!(out, "\n| Register | Initial | Expected | Got |");
    let _ = writeln!(out, "|---|---|---|---|");
    for (i, name) in registers.iter().enumerate() {
        let (want, got) = (case.fin[i], s.actual.regs[i]);
        let mark = if want == got { "" } else { " **differs**" };
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {}{mark} |",
            hex(case.init[i], widths[i]),
            hex(want, widths[i]),
            hex(got, widths[i])
        );
    }
    let _ = writeln!(out, "\n| Address | Initial | Expected | Got |");
    let _ = writeln!(out, "|---|---|---|---|");
    let mut addrs: Vec<u64> = case
        .init_ram
        .iter()
        .chain(&case.fin_ram)
        .map(|(a, _)| *a)
        .collect();
    addrs.sort_unstable();
    addrs.dedup();
    for a in addrs {
        let find = |list: &[(u64, u64)]| {
            list.iter()
                .find(|(x, _)| *x == a)
                .map_or("-".to_string(), |(_, v)| format!("0x{v:02X}"))
        };
        let want = find(&case.fin_ram);
        let got = find(&s.actual.ram);
        let mark = if want == got { "" } else { " **differs**" };
        let _ = writeln!(
            out,
            "| 0x{a:04X} | {} | {want} | {got}{mark} |",
            find(&case.init_ram)
        );
    }
    if !bus {
        let _ = writeln!(
            out,
            "\n{} cycles were run; the bus is not mapped, so they were not compared.",
            case.cycles.len()
        );
        return;
    }
    let n = case.cycles.len();
    let (from, to) = match s.first_cycle {
        Some(i) if n > 2 * CONTEXT + 4 => (i.saturating_sub(CONTEXT), (i + CONTEXT + 1).min(n)),
        _ => (0, n),
    };
    let _ = writeln!(
        out,
        "\nCycles {} to {} of {n}{}:\n",
        from + 1,
        to,
        match s.first_cycle {
            Some(i) => format!(", the first difference at cycle {}", i + 1),
            None => ", all as expected".into(),
        }
    );
    let _ = writeln!(out, "| Cycle | Expected | Got |");
    let _ = writeln!(out, "|---:|---|---|");
    for i in from..to {
        let want = case.cycles.get(i);
        let got = s.actual.cycles.get(i);
        let mark = if want == got { "" } else { " **differs**" };
        let _ = writeln!(
            out,
            "| {} | {} | {}{mark} |",
            i + 1,
            cycle_cell(want),
            cycle_cell(got)
        );
    }
}
