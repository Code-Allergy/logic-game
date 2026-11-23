//! Program suites: one program runs from a start state, cycle by cycle,
//! and is checked at each instruction boundary (the config's `boundary`
//! expression, read once per cycle after the `sample` tick; every cycle
//! when absent).
//!
//! - `nestest`: the `record` columns at each boundary are compared with
//!   the next row of the log (read as the `run` tool's `compare` reads a
//!   file); the first differing row is reported with the rows before it.
//!   It passes when every row of the log matched.
//! - `klaus`: a trap is the program counter the same at two boundaries in
//!   a row (an instruction that jumps to itself). It passes when the trap
//!   is at `success`.
//! - `blargg`: the status byte at `$6000` (`status_at`) reads `$80` while
//!   the test runs, once `DE B0 61` is at `$6001`; any other value after
//!   that is the result (0 passes), and the text at `$6004` is what the
//!   test printed.
//!
//! The shared part, [`drive`], loads and starts the machine (through
//! [`Machine::new`]: inputs and `start` steps) and runs until a check
//! says the program is done or `max_cycles` pass.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::Instant;

use serde_json::Value;

use super::report::ProgramResult;
use super::{Config, Kind, Machine, Probe, RunOptions, hex, options_of};
use crate::mcp::batch::Place;
use crate::mcp::rig::{Signal, Source};
use crate::mcp::run;
use crate::sim::WorkspaceDocument;
use crate::sim::challenges::expr::Expr;

/// The end of a program suite: passed or not, a line, more lines.
struct Verdict {
    passed: bool,
    text: String,
    details: String,
}

/// What a suite checks at each boundary.
trait Check {
    /// `Some` when the program is done.
    fn boundary(&mut self, m: &Machine, cycle: u64) -> Result<Option<Verdict>, String>;
    /// The verdict when `max_cycles` passed first.
    fn timeout(&self, m: &Machine, cycles: u64) -> Verdict;
}

/// Run the machine cycle by cycle, calling `check` at each boundary.
fn drive(
    m: &mut Machine,
    config: &Config,
    boundary: Option<&super::Cond>,
    check: &mut dyn Check,
) -> Result<ProgramResult, String> {
    let t = config.timing;
    let started = Instant::now();
    let ticks0 = m.rig.tick_count();
    m.rig.run(t.before);
    let mut instructions = 0;
    let mut cycle = 0;
    let verdict = loop {
        if cycle == config.max_cycles {
            break check.timeout(m, cycle);
        }
        m.rig.run(t.sample);
        if boundary.is_none_or(|b| m.holds(b)) {
            instructions += 1;
            if let Some(v) = check.boundary(m, cycle)? {
                cycle += 1;
                break v;
            }
        }
        m.rig.run(t.ticks_per_cycle - t.sample);
        cycle += 1;
    };
    Ok(ProgramResult {
        passed: verdict.passed,
        verdict: verdict.text,
        details: verdict.details,
        cycles: cycle,
        instructions,
        ticks: m.rig.tick_count() - ticks0,
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn names_of(text: &str) -> Result<Vec<String>, String> {
    Ok(Expr::parse(text)
        .map_err(|e| format!("\"{text}\": {e}"))?
        .names()
        .into_iter()
        .map(str::to_string)
        .collect())
}

/// Run a nestest, klaus or blargg suite.
pub(super) fn run(
    ws: &WorkspaceDocument,
    place: Place,
    config: &Config,
    options: &RunOptions,
) -> Result<ProgramResult, String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(b) = &config.boundary {
        names.extend(names_of(b)?);
    }
    match config.kind {
        Kind::Nestest => names.extend(config.record.iter().cloned()),
        Kind::Klaus => names.push(config.pc.clone()),
        _ => {}
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut m = Machine::new(ws, place, config, options_of(options), &refs)?;
    let boundary = config
        .boundary
        .as_deref()
        .map(|b| m.cond(config, b))
        .transpose()?;
    let mut check: Box<dyn Check> = match config.kind {
        Kind::Nestest => Box::new(Nestest::new(&mut m, config)?),
        Kind::Klaus => Box::new(Klaus {
            pc: m.probe(config, &config.pc)?,
            success: config.success.unwrap_or(0),
            recent: VecDeque::new(),
        }),
        Kind::Blargg => {
            // Fail on a bad address now rather than at the first boundary.
            m.locate(config.status_at)?;
            Box::new(Blargg {
                at: config.status_at,
                running: false,
            })
        }
        Kind::Harte => return Err("a harte suite is not a program".into()),
    };
    drive(&mut m, config, boundary.as_ref(), check.as_mut())
}

// ── nestest ──────────────────────────────────────────────────────────────

struct Nestest {
    probes: Vec<Probe>,
    /// The columns, for reading the log and for messages.
    columns: Vec<Signal>,
    /// The cycle column's first value; values are read in base 16.
    cycle: Option<u64>,
    radix16: bool,
    what: String,
    rows: Vec<Vec<Option<u64>>>,
    context: usize,
    at: usize,
    recent: VecDeque<(u64, Vec<u64>)>,
}

impl Nestest {
    fn new(m: &mut Machine, config: &Config) -> Result<Nestest, String> {
        let mut probes = Vec::new();
        let mut columns = Vec::new();
        for name in &config.record {
            let p = m.probe(config, name)?;
            columns.push(Signal {
                text: name.clone(),
                source: Source::Zero,
                width: p.width(),
            });
            probes.push(p);
        }
        if let Some((name, _)) = &config.cycle_column {
            columns.push(Signal {
                text: name.clone(),
                source: Source::Zero,
                width: 64,
            });
        }
        let mut spec = config.compare.clone();
        let log = config.log.as_ref().ok_or("no \"log\"")?;
        spec.insert(
            "file".into(),
            Value::String(log.to_string_lossy().into_owned()),
        );
        let radix16 = spec
            .get("radix")
            .and_then(|r| super::number(r).ok())
            .is_some_and(|r| r == 16);
        let expected = run::expected(&Value::Object(spec), &columns)?;
        Ok(Nestest {
            probes,
            columns,
            cycle: config.cycle_column.as_ref().map(|(_, s)| *s),
            radix16,
            what: expected.what,
            rows: expected.rows,
            context: expected.context,
            at: 0,
            recent: VecDeque::new(),
        })
    }

    /// The cycle count as the log's reader reads it: a log writes it in
    /// decimal, which a base-16 reading turns into these digits in hex.
    fn cycle_value(&self, n: u64) -> u64 {
        if self.radix16 {
            u64::from_str_radix(&n.to_string(), 16).unwrap_or(u64::MAX)
        } else {
            n
        }
    }

    fn cell(&self, column: usize, v: u64) -> String {
        let c = &self.columns[column];
        if self.cycle.is_some() && column == self.columns.len() - 1 {
            if self.radix16 {
                format!("{v:X}")
            } else {
                v.to_string()
            }
        } else {
            hex(v, c.width)
        }
    }

    fn row_text(&self, row: &[u64]) -> String {
        row.iter()
            .enumerate()
            .map(|(c, v)| format!("{}={}", self.columns[c].text, self.cell(c, *v)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Check for Nestest {
    fn boundary(&mut self, m: &Machine, cycle: u64) -> Result<Option<Verdict>, String> {
        let mut row: Vec<u64> = self.probes.iter().map(|p| m.value(p)).collect();
        if let Some(first) = self.cycle {
            row.push(self.cycle_value(first + cycle));
        }
        if self.recent.len() > self.context {
            self.recent.pop_front();
        }
        self.recent.push_back((m.rig.tick_count(), row.clone()));
        let want = &self.rows[self.at];
        let wrong: Vec<usize> = (0..row.len())
            .filter(|&c| want[c].is_some_and(|w| w != row[c]))
            .collect();
        if !wrong.is_empty() {
            let mut details = format!(
                "first difference at row {} of {} (cycle {cycle}, tick {}):\n",
                self.at + 1,
                self.what,
                m.rig.tick_count()
            );
            for &c in &wrong {
                let _ = writeln!(
                    details,
                    "  {}: expected {}, got {}",
                    self.columns[c].text,
                    self.cell(c, want[c].unwrap_or(0)),
                    self.cell(c, row[c])
                );
            }
            let expected: Vec<String> = want
                .iter()
                .enumerate()
                .filter_map(|(c, v)| {
                    v.map(|v| format!("{}={}", self.columns[c].text, self.cell(c, v)))
                })
                .collect();
            let _ = writeln!(details, "expected row: {}", expected.join(" "));
            let _ = writeln!(details, "rows before it (and the differing one):");
            let first = self.at + 1 - self.recent.len().min(self.at + 1);
            for (i, (tick, r)) in self.recent.iter().enumerate() {
                let _ = writeln!(
                    details,
                    "  row {} (tick {tick}): {}",
                    first + i + 1,
                    self.row_text(r)
                );
            }
            return Ok(Some(Verdict {
                passed: false,
                text: format!(
                    "row {} of {} rows differs ({})",
                    self.at + 1,
                    self.rows.len(),
                    wrong
                        .iter()
                        .map(|&c| self.columns[c].text.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                details,
            }));
        }
        self.at += 1;
        if self.at == self.rows.len() {
            return Ok(Some(Verdict {
                passed: true,
                text: format!("all {} rows of {} match", self.rows.len(), self.what),
                details: String::new(),
            }));
        }
        Ok(None)
    }

    fn timeout(&self, _: &Machine, cycles: u64) -> Verdict {
        Verdict {
            passed: false,
            text: format!(
                "{} of {} rows matched; max_cycles ({cycles}) passed first",
                self.at,
                self.rows.len()
            ),
            details: String::new(),
        }
    }
}

// ── klaus ────────────────────────────────────────────────────────────────

struct Klaus {
    pc: Probe,
    success: u64,
    /// The program counter at recent boundaries.
    recent: VecDeque<u64>,
}

impl Klaus {
    fn trail(&self) -> String {
        let pcs: Vec<String> = self.recent.iter().map(|p| format!("0x{p:04X}")).collect();
        format!("program counter at the last boundaries: {}", pcs.join(" "))
    }
}

impl Check for Klaus {
    fn boundary(&mut self, m: &Machine, _: u64) -> Result<Option<Verdict>, String> {
        let pc = m.value(&self.pc);
        let trapped = self.recent.back() == Some(&pc);
        if self.recent.len() == 16 {
            self.recent.pop_front();
        }
        self.recent.push_back(pc);
        if !trapped {
            return Ok(None);
        }
        Ok(Some(if pc == self.success {
            Verdict {
                passed: true,
                text: format!("trapped at the success address 0x{pc:04X}"),
                details: String::new(),
            }
        } else {
            Verdict {
                passed: false,
                text: format!(
                    "trapped at 0x{pc:04X}, not at the success address 0x{:04X}",
                    self.success
                ),
                details: self.trail(),
            }
        }))
    }

    fn timeout(&self, _: &Machine, cycles: u64) -> Verdict {
        Verdict {
            passed: false,
            text: format!("no trap in {cycles} cycles (max_cycles)"),
            details: self.trail(),
        }
    }
}

// ── blargg ───────────────────────────────────────────────────────────────

struct Blargg {
    at: u64,
    /// `$80` was seen with the signature.
    running: bool,
}

impl Blargg {
    fn signed(&self, m: &Machine) -> Result<bool, String> {
        Ok(m.read(self.at + 1)? == 0xDE
            && m.read(self.at + 2)? == 0xB0
            && m.read(self.at + 3)? == 0x61)
    }

    /// The zero-terminated text at `$6004`.
    fn text(&self, m: &Machine) -> String {
        let mut bytes = Vec::new();
        for i in 0..4096 {
            match m.read(self.at + 4 + i) {
                Ok(0) | Err(_) => break,
                Ok(b) => bytes.push(b as u8),
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Check for Blargg {
    fn boundary(&mut self, m: &Machine, _: u64) -> Result<Option<Verdict>, String> {
        let status = m.read(self.at)?;
        if !self.signed(m)? {
            return Ok(None);
        }
        if status == 0x80 {
            self.running = true;
            return Ok(None);
        }
        if !self.running {
            return Ok(None);
        }
        let text = self.text(m);
        Ok(Some(if status == 0x81 {
            Verdict {
                passed: false,
                text: "the test asks for a reset ($81), which the runner does not give".into(),
                details: text,
            }
        } else {
            Verdict {
                passed: status == 0,
                text: format!(
                    "status 0x{status:02X} at 0x{:04X}{}",
                    self.at,
                    if status == 0 { " (passed)" } else { "" }
                ),
                details: text,
            }
        }))
    }

    fn timeout(&self, m: &Machine, cycles: u64) -> Verdict {
        let status = m.read(self.at).unwrap_or(0);
        Verdict {
            passed: false,
            text: if self.running {
                format!("still running ($80) after {cycles} cycles (max_cycles)")
            } else {
                format!(
                    "no status protocol in {cycles} cycles: 0x{status:02X} at 0x{:04X} and no \
                     DE B0 61 after it with $80",
                    self.at
                )
            },
            details: self.text(m),
        }
    }
}
