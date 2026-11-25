//! Tom Harte's single-step tests: one JSON file per opcode, each an array
//! of cases.
//!
//! ```json
//! {"name": "a9 2b 3c",
//!  "initial": {"pc": 1234, "s": 253, "a": 0, "x": 1, "y": 2, "p": 36,
//!              "ram": [[1234, 169], [1235, 43]]},
//!  "final": {"pc": 1236, ..., "ram": [[1234, 169], [1235, 43]]},
//!  "cycles": [[1234, 169, "read"], [1235, 43, "read"]]}
//! ```
//!
//! A case writes its registers (and the config's `fixed` state) and its
//! memory bytes, settles once, runs one cycle per entry of `cycles`
//! (reading the bus after the `sample` tick of each when the config maps
//! it), then reads the registers and the memory bytes of `final`. The
//! words a case wrote or the bus touched are set back to 0 before the next
//! case, so memory is never cleared whole.
//!
//! Work is cut into units of `chunk` cases of one file. Workers take units
//! in order from a shared queue, each with its own simulation; results are
//! put back in unit order, so the report is the same for any number of
//! workers. A file is read when its first unit is taken and dropped when
//! its last unit is done.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::time::Instant;

use serde_json::Value;

use super::report::{Body, Failure, FileResult, Story};
use super::{Config, Machine, Probe, RunOptions, Timing, glob, number, options_of};
use crate::mcp::batch::Place;
use crate::mcp::rig::{Signal, StateTarget};
use crate::sim::WorkspaceDocument;

/// One bus cycle: address, data, and whether it is a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cycle {
    pub addr: u64,
    pub data: u64,
    pub read: bool,
}

/// One case, its registers in the config's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub name: String,
    pub init: Vec<u64>,
    pub init_ram: Vec<(u64, u64)>,
    pub fin: Vec<u64>,
    pub fin_ram: Vec<(u64, u64)>,
    pub cycles: Vec<Cycle>,
}

fn ram_of(v: Option<&Value>, what: &str) -> Result<Vec<(u64, u64)>, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = v else {
        return Err(format!("{what}.ram is a list of [address, value]"));
    };
    items
        .iter()
        .map(|pair| match pair.as_array().map(Vec::as_slice) {
            Some([a, v]) => Ok((number(a)?, number(v)?)),
            _ => Err(format!("{what}.ram is a list of [address, value]")),
        })
        .collect()
}

/// Registers in the config's order, and `[address, value]` pairs.
type State = (Vec<u64>, Vec<(u64, u64)>);

fn state_of(
    v: Option<&Value>,
    what: &str,
    names: &[String],
    ignore: &[String],
) -> Result<State, String> {
    let Some(Value::Object(map)) = v else {
        return Err(format!("no \"{what}\" object"));
    };
    let mut regs = Vec::with_capacity(names.len());
    for name in names {
        let v = map
            .get(name)
            .ok_or_else(|| format!("{what} has no register \"{name}\""))?;
        regs.push(number(v).map_err(|e| format!("{what}.{name}: {e}"))?);
    }
    if let Some(k) = map
        .keys()
        .find(|k| *k != "ram" && !names.contains(k) && !ignore.contains(k))
    {
        return Err(format!(
            "{what} has \"{k}\", which is not a register of the config (add it to \
             \"registers\", or to \"ignore\")"
        ));
    }
    Ok((regs, ram_of(map.get("ram"), what)?))
}

/// Read the cases of one value (an array of cases).
pub fn parse_cases(
    value: &Value,
    names: &[String],
    ignore: &[String],
    limit: Option<usize>,
) -> Result<Vec<Case>, String> {
    let Value::Array(items) = value else {
        return Err("a case file holds an array of cases".into());
    };
    let n = limit.map_or(items.len(), |l| l.min(items.len()));
    items[..n]
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let at = |e: String| format!("case {i}: {e}");
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("#{i}"), str::to_string);
            let (init, init_ram) =
                state_of(item.get("initial"), "initial", names, ignore).map_err(at)?;
            let (fin, fin_ram) = state_of(item.get("final"), "final", names, ignore).map_err(at)?;
            let cycles = match item.get("cycles") {
                Some(Value::Array(cycles)) => cycles
                    .iter()
                    .map(|c| match c.as_array().map(Vec::as_slice) {
                        Some([a, d, rw]) => Ok(Cycle {
                            addr: number(a)?,
                            data: number(d)?,
                            read: match rw.as_str() {
                                Some("read") => true,
                                Some("write") => false,
                                _ => {
                                    return Err(
                                        "a cycle's third item is \"read\" or \"write\"".into()
                                    );
                                }
                            },
                        }),
                        _ => Err("a cycle is [address, value, \"read\" or \"write\"]".into()),
                    })
                    .collect::<Result<_, String>>()
                    .map_err(at)?,
                _ => return Err(at("no \"cycles\" list".into())),
            };
            Ok(Case {
                name,
                init,
                init_ram,
                fin,
                fin_ram,
                cycles,
            })
        })
        .collect()
}

fn read_file(path: &Path, config: &Config, limit: Option<usize>) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: not valid JSON: {e}", path.display()))?;
    let names: Vec<String> = config.registers.iter().map(|(n, _)| n.clone()).collect();
    parse_cases(&value, &names, &config.ignore, limit)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The case files of the config, in name order, that `only` keeps.
pub fn case_files(config: &Config, only: &[String]) -> Result<Vec<PathBuf>, String> {
    let Some(path) = &config.cases else {
        return Err("no \"cases\"".into());
    };
    let mut files = if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        files
    } else if path.is_file() {
        vec![path.clone()]
    } else {
        return Err(format!("{}: no such file or folder", path.display()));
    };
    if !only.is_empty() {
        files.retain(|f| {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            let stem = f
                .file_stem()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            only.iter().any(|p| glob(p, &name) || glob(p, &stem))
        });
        if files.is_empty() {
            return Err(format!(
                "no case file of {} matches {}",
                path.display(),
                only.join(", ")
            ));
        }
    }
    Ok(files)
}

/// The bus signals of a machine.
struct Bus {
    addr: Signal,
    data: Option<Signal>,
    read_data: Option<Signal>,
    write_data: Option<Signal>,
    rw: Option<Signal>,
    read_value: u64,
}

fn signal(p: Probe) -> Signal {
    match p {
        Probe::Signal(s) => s,
        Probe::State(t) => Signal {
            text: t.text.clone(),
            width: t.width,
            source: crate::mcp::rig::Source::State(t),
        },
    }
}

/// What a case did: the registers, the words at the addresses of
/// `final`, and the bus at each cycle (empty when the bus is not mapped).
#[derive(Debug, Clone, PartialEq)]
pub struct Actual {
    pub regs: Vec<u64>,
    pub ram: Vec<(u64, u64)>,
    pub cycles: Vec<Cycle>,
}

/// One worker's simulation.
pub(crate) struct Worker {
    m: Machine,
    bus: Option<Bus>,
    timing: Timing,
    /// Words to set back to 0 before the next case.
    dirty: Vec<(usize, usize)>,
    pub ticks: u64,
}

/// The names a Harte machine reads besides its registers.
fn bus_names(config: &Config) -> Vec<&str> {
    let Some(b) = &config.bus else {
        return Vec::new();
    };
    [
        Some(&b.addr),
        b.data.as_ref(),
        b.read_data.as_ref(),
        b.write_data.as_ref(),
        b.rw.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(String::as_str)
    .collect()
}

impl Worker {
    pub fn new(
        ws: &WorkspaceDocument,
        place: Place,
        config: &Config,
        options: logi_sim::compile::Options,
    ) -> Result<Worker, String> {
        let names = bus_names(config);
        let mut m = Machine::new(ws, place, config, options, &names)?;
        let bus = match &config.bus {
            None => None,
            Some(b) => {
                let mut get = |n: &Option<String>| -> Result<Option<Signal>, String> {
                    n.as_deref()
                        .map(|n| m.probe(config, n).map(signal))
                        .transpose()
                };
                Some(Bus {
                    addr: get(&Some(b.addr.clone()))?.expect("addr"),
                    data: get(&b.data)?,
                    read_data: get(&b.read_data)?,
                    write_data: get(&b.write_data)?,
                    rw: get(&b.rw)?,
                    read_value: b.read_value,
                })
            }
        };
        if config.memory.is_none() {
            return Err("a harte suite needs \"memory\" (where its RAM bytes go)".into());
        }
        Ok(Worker {
            m,
            bus,
            timing: config.timing,
            dirty: Vec::new(),
            ticks: 0,
        })
    }

    fn write(&mut self, words: &[((usize, usize), u64)]) -> Result<(), String> {
        let Machine { rig, mems, .. } = &mut self.m;
        for (i, mem) in mems.iter().enumerate() {
            let list: Vec<(usize, u64)> = words
                .iter()
                .filter(|((m, _), _)| *m == i)
                .map(|((_, w), v)| (*w, *v))
                .collect();
            if !list.is_empty() {
                rig.write_words(mem, &list)?;
            }
        }
        Ok(())
    }

    /// The bus now.
    fn cycle(&self, bus: &Bus) -> Cycle {
        let r = &self.m.rig;
        let read = bus.rw.as_ref().map(|s| r.value(s) == bus.read_value);
        let data = match (read, &bus.data) {
            (_, Some(d)) => r.value(d),
            (Some(true), None) => bus.read_data.as_ref().map_or(0, |s| r.value(s)),
            (_, None) => bus.write_data.as_ref().map_or(0, |s| r.value(s)),
        };
        Cycle {
            addr: r.value(&bus.addr),
            data,
            // Unknown without `rw`; set by the caller from the case.
            read: read.unwrap_or(true),
        }
    }

    /// Run one case.
    pub fn run(&mut self, case: &Case) -> Result<Actual, String> {
        // Set back what the last case touched, then write this one.
        let mut words: Vec<((usize, usize), u64)> =
            self.dirty.drain(..).map(|at| (at, 0)).collect();
        for &(addr, value) in &case.init_ram {
            let at = self.m.locate(addr)?;
            words.push((at, value));
            self.dirty.push(at);
        }
        for &(addr, _) in &case.fin_ram {
            self.dirty.push(self.m.locate(addr)?);
        }
        self.write(&words)?;
        let mut pokes: Vec<(&StateTarget, u64)> =
            self.m.fixed.iter().map(|(t, v)| (t, *v)).collect();
        pokes.extend(
            self.m
                .regs
                .iter()
                .map(|(_, t)| t)
                .zip(case.init.iter().copied()),
        );
        self.m.rig.write_state(&pokes)?;
        self.m.rig.settle();

        let t = self.timing;
        self.m.rig.run(t.before);
        let mut cycles = Vec::new();
        match &self.bus {
            Some(bus) => {
                for want in &case.cycles {
                    self.m.rig.run(t.sample);
                    let mut c = self.cycle(bus);
                    if bus.rw.is_none() {
                        c.read = want.read;
                    }
                    cycles.push(c);
                    self.m.rig.run(t.ticks_per_cycle - t.sample);
                }
            }
            None => self.m.rig.run(t.ticks_per_cycle * case.cycles.len() as u64),
        }
        self.m.rig.run(t.after);
        self.ticks += t.before + t.after + t.ticks_per_cycle * case.cycles.len() as u64;
        for c in &cycles {
            // A write may land anywhere the bus pointed.
            if let Ok(at) = self.m.locate(c.addr) {
                self.dirty.push(at);
            }
        }
        let regs = self
            .m
            .regs
            .iter()
            .map(|(_, t)| self.m.rig.peek(t))
            .collect();
        let ram = case
            .fin_ram
            .iter()
            .map(|&(addr, _)| Ok((addr, self.m.word(self.m.locate(addr)?))))
            .collect::<Result<_, String>>()?;
        Ok(Actual { regs, ram, cycles })
    }

    pub fn widths(&self) -> Vec<u8> {
        self.m.regs.iter().map(|(_, t)| t.width).collect()
    }

    pub fn has_bus(&self) -> bool {
        self.bus.is_some()
    }
}

fn cycle_text(c: &Cycle) -> String {
    format!(
        "0x{:04X} 0x{:02X} {}",
        c.addr,
        c.data,
        if c.read { "read" } else { "write" }
    )
}

/// What differs, one line each, and the first differing cycle.
pub(crate) fn differences(
    names: &[String],
    widths: &[u8],
    case: &Case,
    actual: &Actual,
) -> (Vec<String>, Option<usize>) {
    let mut out = Vec::new();
    let first = case
        .cycles
        .iter()
        .zip(&actual.cycles)
        .position(|(w, g)| w != g);
    if let Some(i) = first {
        out.push(format!(
            "cycle {}: expected {}, got {}",
            i + 1,
            cycle_text(&case.cycles[i]),
            cycle_text(&actual.cycles[i])
        ));
    }
    for (i, name) in names.iter().enumerate() {
        if case.fin[i] != actual.regs[i] {
            out.push(format!(
                "{name}: expected {}, got {}",
                super::hex(case.fin[i], widths[i]),
                super::hex(actual.regs[i], widths[i])
            ));
        }
    }
    for (&(addr, want), &(_, got)) in case.fin_ram.iter().zip(&actual.ram) {
        if want != got {
            out.push(format!(
                "ram[0x{addr:04X}]: expected 0x{want:02X}, got 0x{got:02X}"
            ));
        }
    }
    (out, first)
}

/// One unit of work: cases `from .. to` of file `file`.
struct Unit {
    file: usize,
    cases: Arc<Vec<Case>>,
    from: usize,
    to: usize,
}

/// What a unit found.
struct Done {
    file: usize,
    from: usize,
    to: usize,
    /// Failing cases: index, name, differences.
    failures: Vec<Failure>,
    /// The first failure in full.
    story: Option<Story>,
    ticks: u64,
}

/// The queue of units, in (file, case) order.
struct Queue {
    files: Vec<PathBuf>,
    next_file: usize,
    current: Option<(usize, Arc<Vec<Case>>, usize)>,
    /// Cases read from each file (`None`: not read).
    totals: Vec<Option<usize>>,
    failures: usize,
    stop_after: Option<usize>,
    limit: Option<usize>,
    chunk: usize,
}

impl Queue {
    fn next(&mut self, config: &Config) -> Result<Option<Unit>, String> {
        if self.stop_after.is_some_and(|n| self.failures >= n) {
            return Ok(None);
        }
        loop {
            if let Some((file, cases, at)) = &mut self.current
                && *at < cases.len()
            {
                let from = *at;
                let to = (from + self.chunk).min(cases.len());
                *at = to;
                return Ok(Some(Unit {
                    file: *file,
                    cases: Arc::clone(cases),
                    from,
                    to,
                }));
            }
            self.current = None;
            if self.next_file == self.files.len() {
                return Ok(None);
            }
            let file = self.next_file;
            self.next_file += 1;
            let cases = read_file(&self.files[file], config, self.limit)?;
            self.totals[file] = Some(cases.len());
            self.current = Some((file, Arc::new(cases), 0));
        }
    }
}

/// Run a Harte suite; the body of the report, the ticks run, the jobs.
pub(super) fn run(
    ws: &WorkspaceDocument,
    place: Place,
    config: &Config,
    options: &RunOptions,
) -> Result<(Body, u64, usize, f64), String> {
    let files = case_files(config, &options.only)?;
    let names: Vec<String> = config.registers.iter().map(|(n, _)| n.clone()).collect();
    let jobs = options.jobs.max(1);
    let compile = options_of(options);
    // One worker first: a config that does not fit the design fails once.
    let first = Worker::new(ws, place, config, compile.clone())?;
    let widths = first.widths();
    let bus = first.has_bus();
    let queue = Mutex::new(Queue {
        totals: vec![None; files.len()],
        files: files.clone(),
        next_file: 0,
        current: None,
        failures: 0,
        stop_after: options.stop_after,
        limit: options.limit,
        chunk: config.chunk,
    });
    let done: Mutex<Vec<Done>> = Mutex::new(Vec::new());
    let error: Mutex<Option<String>> = Mutex::new(None);
    let failed = AtomicBool::new(false);
    let fail = |e: String| {
        failed.store(true, Ordering::Relaxed);
        error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert(e);
    };
    // Every worker compiles before the clock starts.
    let ready = Barrier::new(jobs);
    let started: OnceLock<Instant> = OnceLock::new();
    let work = |worker: Result<Worker, String>| {
        ready.wait();
        started.get_or_init(Instant::now);
        let mut worker = match worker {
            Ok(w) => w,
            Err(e) => return fail(e),
        };
        loop {
            if failed.load(Ordering::Relaxed) {
                return;
            }
            let unit = match queue.lock().map(|mut q| q.next(config)) {
                Ok(Ok(Some(u))) => u,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => return fail(e),
            };
            let before = worker.ticks;
            let mut d = Done {
                file: unit.file,
                from: unit.from,
                to: unit.to,
                failures: Vec::new(),
                story: None,
                ticks: 0,
            };
            for index in unit.from..unit.to {
                let case = &unit.cases[index];
                let actual = match worker.run(case) {
                    Ok(a) => a,
                    Err(e) => {
                        return fail(format!(
                            "{}: case {index} ({}): {e}",
                            files[unit.file].display(),
                            case.name
                        ));
                    }
                };
                let (diffs, first_cycle) = differences(&names, &widths, case, &actual);
                if diffs.is_empty() {
                    continue;
                }
                if d.story.is_none() {
                    d.story = Some(Story {
                        index,
                        case: case.clone(),
                        actual: actual.clone(),
                        diffs: diffs.clone(),
                        first_cycle,
                    });
                }
                d.failures.push(Failure {
                    index,
                    name: case.name.clone(),
                    diffs,
                });
            }
            d.ticks = worker.ticks - before;
            if let Ok(mut q) = queue.lock() {
                q.failures += d.failures.len();
            }
            done.lock().unwrap_or_else(|p| p.into_inner()).push(d);
        }
    };
    std::thread::scope(|s| {
        let work = &work;
        s.spawn(move || work(Ok(first)));
        for _ in 1..jobs {
            s.spawn(|| work(Worker::new(ws, place, config, compile.clone())));
        }
    });
    if let Some(e) = error.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    let seconds = started.get().map_or(0.0, |t| t.elapsed().as_secs_f64());
    let totals = queue.into_inner().unwrap_or_else(|p| p.into_inner()).totals;
    let mut done = done.into_inner().unwrap_or_else(|p| p.into_inner());
    done.sort_by_key(|d| (d.file, d.from));

    // In unit order, up to the failure `stop_after` names.
    let mut results: Vec<FileResult> = files
        .iter()
        .enumerate()
        .map(|(i, f)| FileResult {
            file: f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            cases: totals[i].unwrap_or(0),
            run: 0,
            failed: 0,
            failures: Vec::new(),
            story: None,
        })
        .collect();
    let mut ticks = 0;
    let mut failures = 0;
    let mut stopped = false;
    for d in done {
        if stopped {
            break;
        }
        let r = &mut results[d.file];
        let mut to = d.to;
        for f in &d.failures {
            failures += 1;
            r.failed += 1;
            if r.failures.len() < report_limit() {
                r.failures.push(f.clone());
            }
            if options.stop_after.is_some_and(|n| failures >= n) {
                to = f.index + 1;
                stopped = true;
                break;
            }
        }
        r.run += to - d.from;
        if r.story.is_none() {
            r.story = d.story;
        }
        ticks += d.ticks;
    }
    let stopped =
        stopped || totals.iter().any(Option::is_none) || results.iter().any(|r| r.run < r.cases);
    let repro = results
        .iter()
        .find_map(|r| r.story.as_ref())
        .map(|s| repro(config, s, options.unoptimized));
    Ok((
        Body::Harte {
            registers: names,
            widths,
            bus,
            files: results,
            stopped,
            repro,
        },
        ticks,
        jobs,
        seconds,
    ))
}

/// Failing cases listed per file in the report.
fn report_limit() -> usize {
    10
}

/// A `run` script that replays one case on a fresh simulation, compiled
/// as the suite compiled it (`unoptimized`: without the passes
/// `options_of` turns off), so that a failure of only the unoptimized
/// program is still there when the script is run.
fn repro(config: &Config, story: &Story, unoptimized: bool) -> Value {
    let case = &story.case;
    let mut signals = serde_json::Map::new();
    for (n, p) in config.signals.iter().chain(&config.registers) {
        signals.insert(n.clone(), Value::String(p.clone()));
    }
    let mut steps: Vec<Value> = Vec::new();
    if !config.inputs.is_empty() {
        let set: serde_json::Map<String, Value> = config
            .inputs
            .iter()
            .map(|(n, v)| (n.clone(), Value::from(*v)))
            .collect();
        steps.push(serde_json::json!({ "set": set }));
    }
    steps.extend(config.start.iter().cloned());
    let mut poke = serde_json::Map::new();
    for (path, v) in &config.fixed {
        poke.insert(path.clone(), Value::from(*v));
    }
    for ((name, _), v) in config.registers.iter().zip(&case.init) {
        poke.insert(name.clone(), Value::from(*v));
    }
    // Memory bytes by part, as the memory map places them.
    let place = |addr: u64| -> (String, u64) {
        if let Some(map) = &config.memory {
            for r in &map.regions {
                if (r.from..=r.to).contains(&addr) {
                    return (r.part.clone(), addr - r.from + r.at);
                }
            }
            if let Some(part) = &map.part {
                return (part.clone(), addr & map.mask.unwrap_or(u64::MAX));
            }
        }
        ("ram".into(), addr)
    };
    for &(addr, v) in &case.init_ram {
        let (part, word) = place(addr);
        let list = poke.entry(part).or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(items) = list {
            items.push(serde_json::json!([format!("0x{word:X}"), v]));
        }
    }
    steps.push(serde_json::json!({ "poke": poke }));
    let t = config.timing;
    if let Some(b) = &config.bus {
        let mut names = vec![Value::from(b.addr.clone())];
        for s in [&b.data, &b.read_data, &b.write_data, &b.rw]
            .into_iter()
            .flatten()
        {
            names.push(Value::from(s.clone()));
        }
        steps.push(serde_json::json!({ "record": names }));
    }
    if t.before > 0 {
        steps.push(serde_json::json!({ "ticks": t.before }));
    }
    for c in &case.cycles {
        match &config.bus {
            Some(b) => {
                steps.push(serde_json::json!({ "ticks": t.sample }));
                let mut expect = serde_json::Map::new();
                expect.insert(b.addr.clone(), Value::from(c.addr));
                let data = match (&b.data, c.read) {
                    (Some(d), _) => d,
                    (None, true) => b.read_data.as_ref().unwrap_or(&b.addr),
                    (None, false) => b.write_data.as_ref().unwrap_or(&b.addr),
                };
                expect.insert(data.clone(), Value::from(c.data));
                if let Some(rw) = &b.rw {
                    let v = if c.read {
                        b.read_value
                    } else {
                        u64::from(b.read_value == 0)
                    };
                    expect.insert(rw.clone(), Value::from(v));
                }
                steps.push(serde_json::json!({ "expect": expect }));
                if t.ticks_per_cycle > t.sample {
                    steps.push(serde_json::json!({ "ticks": t.ticks_per_cycle - t.sample }));
                }
            }
            None => steps.push(serde_json::json!({ "ticks": t.ticks_per_cycle })),
        }
    }
    if t.after > 0 {
        steps.push(serde_json::json!({ "ticks": t.after }));
    }
    let mut expect = serde_json::Map::new();
    for ((name, _), v) in config.registers.iter().zip(&case.fin) {
        expect.insert(name.clone(), Value::from(*v));
    }
    for &(addr, v) in &case.fin_ram {
        let (part, word) = place(addr);
        let path = config
            .signals
            .iter()
            .find(|(n, _)| *n == part)
            .map_or(part, |(_, p)| p.clone());
        expect.insert(format!("{path}@0x{word:X}"), Value::from(v));
    }
    steps.push(serde_json::json!({ "expect": expect }));
    let mut script = serde_json::Map::new();
    if let Some(c) = &config.circuit {
        script.insert("circuit".into(), Value::String(c.clone()));
    }
    if unoptimized {
        // Read by the `run` tool (`Session::run_tool`).
        script.insert(
            "off".into(),
            serde_json::json!(crate::mcp::compiled::PASSES),
        );
    }
    script.insert("signals".into(), Value::Object(signals));
    script.insert("steps".into(), Value::Array(steps));
    Value::Object(script)
}
