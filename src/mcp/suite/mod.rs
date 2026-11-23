//! The native suite runner: large CPU test suites run against a design
//! outside the MCP server (`docs/suite-runner.md`; the NES plan, 0.5).
//!
//! A suite config (JSON) names the suite kind, the files of the suite and
//! how the suite's vocabulary maps onto the design: which state target is
//! each register, which memory holds the address space, which signals are
//! the bus, which inputs to hold and how many ticks make a cycle. Nothing
//! here knows a particular CPU.
//!
//! - `harte`: Tom Harte's single-step tests. Each case pokes registers and
//!   memory, runs the cycles it lists (comparing the bus on each when the
//!   config maps it), then compares registers and memory. Cases run on
//!   every core, each worker with its own simulation.
//! - `nestest`, `klaus`, `blargg`: one program runs from a start state
//!   until a log ends or differs, the program traps, or it reports a
//!   status at `$6000` ([`program`]).
//!
//! [`run`] returns a [`Report`], written as `report.json` and `report.md`
//! (and, for a failing Harte case, a `run` script that reproduces it).

mod harte;
mod program;
mod report;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use logi_sim::compile::Options;
use serde_json::{Map, Value};

use super::batch::{Place, circuit_at, find_circuit};
use super::compiled;
use super::rig::{Memory, Rig, Signal, StateTarget};
use super::run::{self, RunState};
use crate::sim::WorkspaceDocument;
use crate::sim::challenges::expr::Expr;
use crate::sim::source::{parse_int_text, valid_name};

pub use harte::{Actual, Case, Cycle, parse_cases};
pub use report::{Body, Compile, Failure, FileResult, ProgramResult, Report, Story};

/// The suites the runner knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Harte,
    Nestest,
    Klaus,
    Blargg,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Harte => "harte",
            Kind::Nestest => "nestest",
            Kind::Klaus => "klaus",
            Kind::Blargg => "blargg",
        }
    }
}

/// Addresses `from ..= to` are words `at ..` of memory `part`.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub from: u64,
    pub to: u64,
    pub part: String,
    pub at: u64,
}

/// How the suite's address space maps onto memories of the design.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryMap {
    /// The memory every address outside `regions` goes to.
    pub part: Option<String>,
    /// Applied to an address before it indexes `part` (mirroring).
    pub mask: Option<u64>,
    pub regions: Vec<Region>,
}

/// Which signals are the bus, sampled once per cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct BusMap {
    pub addr: String,
    /// One data signal for reads and writes, or one for each.
    pub data: Option<String>,
    pub read_data: Option<String>,
    pub write_data: Option<String>,
    /// Read or write; not compared when absent.
    pub rw: Option<String>,
    /// The value of `rw` on a read cycle.
    pub read_value: u64,
}

/// Ticks and cycles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub ticks_per_cycle: u64,
    /// The tick of a cycle (0 to `ticks_per_cycle`) after which the bus
    /// and the boundary condition are read; 0 reads them before the
    /// cycle's first tick.
    pub sample: u64,
    /// Ticks run after the pokes, before the first cycle.
    pub before: u64,
    /// Ticks run after the last cycle, before the final state is read.
    pub after: u64,
}

/// A suite config, read with [`Config::read`] or [`Config::parse`].
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub kind: Kind,
    /// The folder relative paths of the config start from.
    pub base: PathBuf,
    /// The circuit to simulate (the root when absent).
    pub circuit: Option<String>,
    /// Short names for signal paths.
    pub signals: Vec<(String, String)>,
    /// Register names of the suite and the state they are.
    pub registers: Vec<(String, String)>,
    /// State written to these values at the start of every case.
    pub fixed: Vec<(String, u64)>,
    /// Keys of a case's state that are not registers and are not checked.
    pub ignore: Vec<String>,
    pub memory: Option<MemoryMap>,
    pub bus: Option<BusMap>,
    /// Root inputs held from the start.
    pub inputs: Vec<(String, u64)>,
    pub timing: Timing,
    /// `run` steps run once on a fresh simulation, file paths made
    /// relative to `base`.
    pub start: Vec<Value>,
    /// Harte: a case file, or a folder of them.
    pub cases: Option<PathBuf>,
    /// Cases per unit of work (Harte).
    pub chunk: usize,
    /// When an instruction starts (an expression read once per cycle);
    /// every cycle when absent.
    pub boundary: Option<String>,
    /// The most cycles a program suite runs.
    pub max_cycles: u64,
    /// Nestest: the log, the recorded columns and how to read the log.
    pub log: Option<PathBuf>,
    pub record: Vec<String>,
    pub compare: Map<String, Value>,
    /// Nestest: a column that counts cycles, and its first value.
    pub cycle_column: Option<(String, u64)>,
    /// Klaus: the register or signal that is the program counter, and
    /// the address of the success trap.
    pub pc: String,
    pub success: Option<u64>,
    /// Blargg: where the status byte is.
    pub status_at: u64,
}

/// What a run may do, from the command line.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOptions {
    /// Workers (simulations) for Harte suites.
    pub jobs: usize,
    /// Case files whose name matches one of these patterns (`*` and `?`).
    pub only: Vec<String>,
    /// At most this many cases of each file.
    pub limit: Option<usize>,
    /// Stop once this many cases failed.
    pub stop_after: Option<usize>,
    /// Compile with function matching, word lifting and word cleanup off.
    pub unoptimized: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
            only: Vec::new(),
            limit: None,
            stop_after: None,
            unoptimized: false,
        }
    }
}

/// A number: JSON number, `"0x1F"`, `"$1F"`, `"0b101"` or decimal text.
pub(crate) fn number(v: &Value) -> Result<u64, String> {
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(|| format!("{n} is not a value")),
        Value::String(s) => {
            let t = s.trim();
            match t.strip_prefix('$') {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => parse_int_text(t),
            }
            .ok_or_else(|| format!("\"{s}\" is not a value"))
        }
        Value::Bool(b) => Ok(u64::from(*b)),
        other => Err(format!("{other} is not a value")),
    }
}

/// `0x..` with as many digits as `width` bits need (at least two).
pub(crate) fn hex(value: u64, width: u8) -> String {
    let digits = usize::from(width).div_ceil(4).max(2);
    format!("0x{value:0digits$X}")
}

fn pairs<T>(
    v: Value,
    what: &str,
    mut f: impl FnMut(&str, &Value) -> Result<T, String>,
) -> Result<Vec<(String, T)>, String> {
    let Value::Object(map) = v else {
        return Err(format!("\"{what}\" takes {{name: ..., ...}}"));
    };
    map.iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                f(k, v).map_err(|e| format!("\"{what}\".{k}: {e}"))?,
            ))
        })
        .collect()
}

fn text(v: &Value, what: &str) -> Result<String, String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("\"{what}\" takes a text"))
}

fn take_object(map: &mut Map<String, Value>, key: &str) -> Result<Map<String, Value>, String> {
    match map.shift_remove(key) {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m),
        Some(_) => Err(format!("\"{key}\" takes an object")),
    }
}

/// The unknown key of `map`, if any, ignoring comments (`_...`, `//...`).
fn unknown(map: &Map<String, Value>, what: &str, known: &str) -> Result<(), String> {
    match map
        .keys()
        .find(|k| !k.starts_with('_') && !k.starts_with("//"))
    {
        Some(k) => Err(format!("{what}: unknown field \"{k}\" (it has {known})")),
        None => Ok(()),
    }
}

impl Config {
    /// Read a config file; relative paths in it start from its folder.
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?;
        // Absolute, so the paths in a repro script work from anywhere.
        let base = path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        Config::parse(&value, &base).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Read a config; relative paths start from `base`.
    pub fn parse(value: &Value, base: &Path) -> Result<Config, String> {
        let Value::Object(map) = value else {
            return Err("a suite config is an object".into());
        };
        let mut map = map.clone();
        let path = |p: String| -> PathBuf {
            let p = PathBuf::from(p);
            if p.is_absolute() { p } else { base.join(p) }
        };
        let kind = match map.shift_remove("kind").as_ref().and_then(Value::as_str) {
            Some("harte") => Kind::Harte,
            Some("nestest") => Kind::Nestest,
            Some("klaus") => Kind::Klaus,
            Some("blargg") => Kind::Blargg,
            _ => {
                return Err("\"kind\" is \"harte\", \"nestest\", \"klaus\" or \"blargg\"".into());
            }
        };
        let opt_text =
            |map: &mut Map<String, Value>, key: &str| -> Result<Option<String>, String> {
                match map.shift_remove(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => text(&v, key).map(Some),
                }
            };
        let opt_number = |map: &mut Map<String, Value>, key: &str| -> Result<Option<u64>, String> {
            match map.shift_remove(key) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => number(&v).map(Some).map_err(|e| format!("\"{key}\": {e}")),
            }
        };
        let circuit = opt_text(&mut map, "circuit")?;
        let signals = pairs(
            Value::Object(take_object(&mut map, "signals")?),
            "signals",
            |k, v| {
                if !valid_name(k) {
                    return Err("a name is letters, digits and '_'".into());
                }
                text(v, k)
            },
        )?;
        let registers = pairs(
            Value::Object(take_object(&mut map, "registers")?),
            "registers",
            |k, v| {
                if !valid_name(k) {
                    return Err("a name is letters, digits and '_'".into());
                }
                text(v, k)
            },
        )?;
        let fixed = pairs(
            Value::Object(take_object(&mut map, "fixed")?),
            "fixed",
            |_, v| number(v),
        )?;
        let inputs = pairs(
            Value::Object(take_object(&mut map, "inputs")?),
            "inputs",
            |_, v| number(v),
        )?;
        let ignore = match map.shift_remove("ignore") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| text(v, "ignore"))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("\"ignore\" takes a list of names".into()),
        };

        let memory = match map.shift_remove("memory") {
            None | Some(Value::Null) => None,
            Some(Value::String(part)) => Some(MemoryMap {
                part: Some(part),
                mask: None,
                regions: Vec::new(),
            }),
            Some(Value::Object(mut m)) => {
                let part = opt_text(&mut m, "part")?;
                let mask = opt_number(&mut m, "mask")?;
                let regions = match m.shift_remove("regions") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(items)) => items
                        .into_iter()
                        .enumerate()
                        .map(|(i, r)| {
                            let at = |e: String| format!("\"memory\".regions[{i}]: {e}");
                            let Value::Object(mut r) = r else {
                                return Err(at("a region is {from, to, part, at}".into()));
                            };
                            let from = opt_number(&mut r, "from")
                                .map_err(at)?
                                .ok_or_else(|| at("needs \"from\"".into()))?;
                            let to = opt_number(&mut r, "to")
                                .map_err(at)?
                                .ok_or_else(|| at("needs \"to\"".into()))?;
                            let part = opt_text(&mut r, "part")
                                .map_err(at)?
                                .ok_or_else(|| at("needs \"part\"".into()))?;
                            let base = opt_number(&mut r, "at").map_err(at)?.unwrap_or(0);
                            unknown(&r, "a region", "from, to, part, at").map_err(at)?;
                            if to < from {
                                return Err(at("\"to\" is below \"from\"".into()));
                            }
                            Ok(Region {
                                from,
                                to,
                                part,
                                at: base,
                            })
                        })
                        .collect::<Result<_, _>>()?,
                    Some(_) => return Err("\"memory\".regions takes a list".into()),
                };
                unknown(&m, "\"memory\"", "part, mask, regions")?;
                if part.is_none() && regions.is_empty() {
                    return Err("\"memory\" needs \"part\" or \"regions\"".into());
                }
                Some(MemoryMap {
                    part,
                    mask,
                    regions,
                })
            }
            Some(_) => return Err("\"memory\" takes a memory part or {part, mask, regions}".into()),
        };

        let bus = match map.shift_remove("bus") {
            None | Some(Value::Null) => None,
            Some(Value::Object(mut b)) => {
                let addr = opt_text(&mut b, "addr")?.ok_or("\"bus\" needs \"addr\"")?;
                let data = opt_text(&mut b, "data")?;
                let read_data = opt_text(&mut b, "read_data")?;
                let write_data = opt_text(&mut b, "write_data")?;
                let rw = opt_text(&mut b, "rw")?;
                let read_value = opt_number(&mut b, "read_value")?.unwrap_or(1);
                unknown(
                    &b,
                    "\"bus\"",
                    "addr, data, read_data, write_data, rw, read_value",
                )?;
                if data.is_none() && (read_data.is_none() || write_data.is_none()) {
                    return Err(
                        "\"bus\" needs \"data\", or both \"read_data\" and \"write_data\"".into(),
                    );
                }
                if data.is_none() && rw.is_none() {
                    return Err("\"bus\": separate read and write data need \"rw\"".into());
                }
                Some(BusMap {
                    addr,
                    data,
                    read_data,
                    write_data,
                    rw,
                    read_value,
                })
            }
            Some(_) => return Err("\"bus\" takes {addr, data, rw, ...}".into()),
        };

        let mut t = take_object(&mut map, "timing")?;
        let ticks_per_cycle = opt_number(&mut t, "ticks_per_cycle")?.unwrap_or(1);
        let sample = opt_number(&mut t, "sample")?.unwrap_or(ticks_per_cycle);
        let before = opt_number(&mut t, "before")?.unwrap_or(0);
        let after = opt_number(&mut t, "after")?.unwrap_or(0);
        unknown(&t, "\"timing\"", "ticks_per_cycle, sample, before, after")?;
        if ticks_per_cycle == 0 || !(0..=ticks_per_cycle).contains(&sample) {
            return Err(
                "\"timing\": ticks_per_cycle must be at least 1 and sample between 0 and it".into(),
            );
        }
        let timing = Timing {
            ticks_per_cycle,
            sample,
            before,
            after,
        };

        let start = match map.shift_remove("start") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(steps)) => steps
                .into_iter()
                .map(|mut step| {
                    // A file to load is found from the config's folder.
                    if let Some(Value::String(file)) =
                        step.get_mut("load").and_then(|l| l.get_mut("file"))
                    {
                        *file = path(file.clone()).to_string_lossy().into_owned();
                    }
                    step
                })
                .collect(),
            Some(_) => return Err("\"start\" takes a list of run steps".into()),
        };
        let cases = opt_text(&mut map, "cases")?.map(path);
        let chunk = opt_number(&mut map, "chunk")?.unwrap_or(250).max(1) as usize;
        let boundary = opt_text(&mut map, "boundary")?;
        let max_cycles = opt_number(&mut map, "max_cycles")?.unwrap_or(10_000_000);
        let log = opt_text(&mut map, "log")?.map(path);
        let record = match map.shift_remove("record") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| text(v, "record"))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("\"record\" takes a list of names".into()),
        };
        let compare = take_object(&mut map, "compare")?;
        let cycle_column = match map.shift_remove("cycle_column") {
            None | Some(Value::Null) => None,
            Some(Value::Object(mut c)) => {
                let name = opt_text(&mut c, "name")?.ok_or("\"cycle_column\" needs \"name\"")?;
                let first = opt_number(&mut c, "start")?.unwrap_or(0);
                unknown(&c, "\"cycle_column\"", "name, start")?;
                Some((name, first))
            }
            Some(_) => return Err("\"cycle_column\" takes {name, start}".into()),
        };
        let pc = opt_text(&mut map, "pc")?.unwrap_or_else(|| "pc".into());
        let success = opt_number(&mut map, "success")?;
        let status_at = opt_number(&mut map, "status_at")?.unwrap_or(0x6000);
        unknown(
            &map,
            "the config",
            "kind, circuit, signals, registers, fixed, ignore, memory, bus, inputs, timing, \
             start, cases, chunk, boundary, max_cycles, log, record, compare, cycle_column, pc, \
             success, status_at",
        )?;

        match kind {
            Kind::Harte => {
                if cases.is_none() {
                    return Err("a harte suite needs \"cases\" (a file or folder)".into());
                }
                if registers.is_empty() {
                    return Err("a harte suite needs \"registers\"".into());
                }
            }
            Kind::Nestest => {
                if log.is_none() || record.is_empty() {
                    return Err("a nestest suite needs \"log\" and \"record\"".into());
                }
            }
            Kind::Klaus => {
                if success.is_none() {
                    return Err("a klaus suite needs \"success\" (the trap address)".into());
                }
            }
            Kind::Blargg => {
                if memory.is_none() {
                    return Err(
                        "a blargg suite needs \"memory\" (where the status at $6000 is)".into(),
                    );
                }
            }
        }
        Ok(Config {
            kind,
            base: base.to_path_buf(),
            circuit,
            signals,
            registers,
            fixed,
            ignore,
            memory,
            bus,
            inputs,
            timing,
            start,
            cases,
            chunk,
            boundary,
            max_cycles,
            log,
            record,
            compare,
            cycle_column,
            pc,
            success,
            status_at,
        })
    }

    /// The path of an alias or register name, or `name` itself.
    fn path_of<'a>(&'a self, name: &'a str) -> &'a str {
        self.signals
            .iter()
            .chain(&self.registers)
            .find(|(n, _)| n == name)
            .map_or(name, |(_, p)| p.as_str())
    }
}

/// Read a workspace: a folder holding `manifest.json`, or the manifest.
pub fn read_workspace(path: &Path) -> Result<WorkspaceDocument, String> {
    let folder = if path.is_file() {
        path.parent().unwrap_or(Path::new("."))
    } else {
        path
    };
    let folder = folder
        .canonicalize()
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".into());
    crate::editor::workspace::read_workspace_at(&folder, &name)
        .map_err(|e| format!("{}: {e}", folder.display()))
}

/// The compiler options of a run.
fn options_of(run: &RunOptions) -> Options {
    if run.unoptimized {
        compiled::options_without(&["func_match".into(), "lift".into(), "words".into()])
    } else {
        Options::default()
    }
}

/// Something a suite reads: a register (state) or a signal.
#[derive(Debug, Clone)]
pub(crate) enum Probe {
    State(StateTarget),
    Signal(Signal),
}

impl Probe {
    fn width(&self) -> u8 {
        match self {
            Probe::State(t) => t.width,
            Probe::Signal(s) => s.width,
        }
    }
}

/// An expression over registers and signals, read once per cycle.
pub(crate) struct Cond {
    expr: Expr,
    vars: Vec<(String, Probe)>,
}

/// One simulation set up for a suite: the rig, its registers, the memory
/// map and the signals the suite reads, resolved once.
pub(crate) struct Machine {
    pub rig: Rig,
    /// The registers of the config, in its order.
    pub regs: Vec<(String, StateTarget)>,
    pub fixed: Vec<(StateTarget, u64)>,
    pub mems: Vec<Memory>,
    /// Index of each memory in the simulation.
    mem_index: Vec<u32>,
    /// The memory every unmapped address goes to, and its mask.
    main: Option<(usize, u64)>,
    regions: Vec<(u64, u64, usize, u64)>,
    probes: HashMap<String, Probe>,
}

impl Machine {
    /// Compile the circuit, resolve the config's registers, memories and
    /// `names` (registers or signals), then set the inputs and run the
    /// start steps.
    pub fn new(
        ws: &WorkspaceDocument,
        place: Place,
        config: &Config,
        options: Options,
        names: &[&str],
    ) -> Result<Machine, String> {
        let mut rooted = ws.clone();
        if let Place::Definition(_) = place {
            rooted.root = circuit_at(ws, place).clone();
        }
        let mut rig = Rig::with_options(rooted, options)?;
        let mut regs = Vec::new();
        for (name, path) in &config.registers {
            let target = rig
                .target(path)
                .map_err(|e| format!("register \"{name}\": {e}"))?;
            regs.push((name.clone(), target));
        }
        let mut fixed = Vec::new();
        for (path, value) in &config.fixed {
            let target = rig
                .target(config.path_of(path))
                .map_err(|e| format!("\"fixed\": {e}"))?;
            target.fits(*value).map_err(|e| format!("\"fixed\": {e}"))?;
            fixed.push((target, *value));
        }
        let mut mems: Vec<Memory> = Vec::new();
        // A memory's index in `mems` and its address bits.
        let mut memory = |rig: &mut Rig, part: &str| -> Result<(usize, u8), String> {
            let m = rig.memory(config.path_of(part))?;
            if m.rom {
                return Err(format!(
                    "memory \"{part}\" is a ROM; the memory map holds state (RAM); load a ROM \
                     with a \"load\" start step"
                ));
            }
            let bits = m.addr_bits;
            Ok(match mems.iter().position(|x| x.path == m.path) {
                Some(i) => (i, bits),
                None => {
                    mems.push(m);
                    (mems.len() - 1, bits)
                }
            })
        };
        let mut main = None;
        let mut regions = Vec::new();
        if let Some(map) = &config.memory {
            if let Some(part) = &map.part {
                let (i, bits) = memory(&mut rig, part)?;
                let size = 1u64 << bits;
                let mask = map.mask.unwrap_or(u64::MAX);
                if map.mask.is_some_and(|m| m >= size) {
                    return Err(format!(
                        "\"memory\": the mask 0x{mask:X} reaches past the {size} words of \"{part}\""
                    ));
                }
                main = Some((i, mask));
            }
            for r in &map.regions {
                let (i, bits) = memory(&mut rig, &r.part)?;
                let size = 1u64 << bits;
                if r.at + (r.to - r.from) >= size {
                    return Err(format!(
                        "\"memory\": region 0x{:X}..0x{:X} reaches past the {size} words of \"{}\"",
                        r.from, r.to, r.part
                    ));
                }
                regions.push((r.from, r.to, i, r.at));
            }
        }
        let mut machine = Machine {
            rig,
            regs,
            fixed,
            mems,
            mem_index: Vec::new(),
            main,
            regions,
            probes: HashMap::new(),
        };
        let mut watched = Vec::new();
        for name in names {
            if let Probe::Signal(s) = machine.probe(config, name)? {
                watched.push(s);
            }
        }
        machine.rig.watch(&watched)?;
        for (name, value) in &config.inputs {
            machine
                .rig
                .set(name, *value)
                .map_err(|e| format!("\"inputs\": {e}"))?;
        }
        if !config.start.is_empty() {
            let mut aliases: Map<String, Value> = Map::new();
            for (n, p) in config.signals.iter().chain(&config.registers) {
                aliases.insert(n.clone(), Value::String(p.clone()));
            }
            let script: Map<String, Value> = serde_json::json!({
                "signals": aliases,
                "steps": config.start,
                "max_ticks": run::MAX_TICKS,
            })
            .as_object()
            .cloned()
            .unwrap_or_default();
            let mut state = RunState::new(machine.rig);
            let outcome = run::run(&mut state, &script).map_err(|e| format!("\"start\": {e}"));
            machine.rig = state.rig;
            let outcome = outcome?;
            if outcome.failed {
                return Err(format!("\"start\" failed:\n{}", outcome.text));
            }
        }
        machine.mem_index =
            machine
                .mems
                .iter()
                .map(|m| {
                    machine.rig.sim().mem_index(&m.path).ok_or_else(|| {
                        "a memory of the map holds no state in this simulation".into()
                    })
                })
                .collect::<Result<_, String>>()?;
        Ok(machine)
    }

    /// Resolve a register or signal name (an alias, a register or a path).
    pub fn probe(&mut self, config: &Config, name: &str) -> Result<Probe, String> {
        if let Some(p) = self.probes.get(name) {
            return Ok(p.clone());
        }
        let probe = if let Some((_, t)) = self.regs.iter().find(|(n, _)| n == name) {
            Probe::State(t.clone())
        } else {
            let mut s = self.rig.resolve(config.path_of(name))?;
            s.text = name.to_string();
            Probe::Signal(s)
        };
        self.probes.insert(name.to_string(), probe.clone());
        Ok(probe)
    }

    pub fn value(&self, probe: &Probe) -> u64 {
        match probe {
            Probe::State(t) => self.rig.peek(t),
            Probe::Signal(s) => self.rig.value(s),
        }
    }

    /// Parse an expression whose names are registers or signals; they
    /// must be among the names the machine was made with.
    pub fn cond(&mut self, config: &Config, text: &str) -> Result<Cond, String> {
        let expr = Expr::parse(text).map_err(|e| format!("\"{text}\": {e}"))?;
        let mut vars = Vec::new();
        for name in expr.names() {
            if !vars.iter().any(|(n, _)| n == name) {
                vars.push((name.to_string(), self.probe(config, name)?));
            }
        }
        Ok(Cond { expr, vars })
    }

    pub fn holds(&self, cond: &Cond) -> bool {
        let values: Vec<(&str, u64)> = cond
            .vars
            .iter()
            .map(|(n, p)| (n.as_str(), self.value(p)))
            .collect();
        cond.expr.eval(&|name: &str| {
            values
                .iter()
                .find(|(n, _)| *n == name)
                .map_or(0, |(_, v)| *v)
        }) != 0
    }

    /// The memory and word an address of the suite is.
    pub fn locate(&self, addr: u64) -> Result<(usize, usize), String> {
        for &(from, to, m, at) in &self.regions {
            if (from..=to).contains(&addr) {
                return Ok((m, (addr - from + at) as usize));
            }
        }
        match self.main {
            Some((m, mask)) => {
                let word = addr & mask;
                if word >> self.mems[m].addr_bits != 0 {
                    return Err(format!(
                        "address 0x{addr:04X} is past the {} words of the memory (give the \
                         memory map a \"mask\" or a region)",
                        1u64 << self.mems[m].addr_bits
                    ));
                }
                Ok((m, word as usize))
            }
            None => Err(format!(
                "address 0x{addr:04X} is in no region of the memory map"
            )),
        }
    }

    /// The word at a located address.
    pub fn word(&self, (m, word): (usize, usize)) -> u64 {
        self.rig
            .sim()
            .mem(self.mem_index[m])
            .get(word)
            .copied()
            .unwrap_or(0)
    }

    /// The word at an address of the suite.
    pub fn read(&self, addr: u64) -> Result<u64, String> {
        Ok(self.word(self.locate(addr)?))
    }
}

/// Run a suite on the circuit of `ws` the config names.
pub fn run(
    ws: &WorkspaceDocument,
    config: &Config,
    options: &RunOptions,
    sources: (&str, &str),
) -> Result<Report, String> {
    let place = find_circuit(ws, config.circuit.as_deref())?.ok_or_else(|| {
        format!(
            "no circuit \"{}\" in the workspace",
            config.circuit.clone().unwrap_or_default()
        )
    })?;
    let insight = compiled::insight(ws, place)?;
    let compile = Compile::of(&insight, options.unoptimized);
    let (body, ticks, jobs, seconds) = match config.kind {
        Kind::Harte => harte::run(ws, place, config, options)?,
        _ => {
            let p = program::run(ws, place, config, options)?;
            let (ticks, seconds) = (p.ticks, p.seconds);
            (Body::Program(p), ticks, 1, seconds)
        }
    };
    Ok(Report {
        kind: config.kind,
        workspace: sources.0.to_string(),
        config: sources.1.to_string(),
        circuit: insight.circuit.clone(),
        compile,
        jobs,
        seconds,
        ticks,
        body,
    })
}

/// `name` matches `pattern` (`*` any run, `?` one character).
pub(crate) fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some((c, rest)) => n.split_first().is_some_and(|(d, nrest)| {
                (*c == '?' || c.eq_ignore_ascii_case(d)) && go(rest, nrest)
            }),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}
