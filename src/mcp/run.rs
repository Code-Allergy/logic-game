//! Experiment scripts: one call runs a whole experiment on a private
//! simulation (`docs/mcp-server.md`, "`run`").
//!
//! ```json
//! {"signals": {"pc": "cpu/pc.q"},
//!  "steps": [
//!    {"load": {"part": "rom", "file": "test.bin"}},
//!    {"set": {"reset": 1}}, {"ticks": 2}, {"set": {"reset": 0}},
//!    {"poke": {"cpu/a": 0x12, "ram": [[0x1234, 5]]}},
//!    {"record": ["pc", "acc"], "when": "sync"},
//!    {"compare": {"file": "expected.log", "radix": 16}},
//!    {"until": "pc == 0x40", "max_ticks": 100000},
//!    {"read": ["acc", "ram@0x00..0x10"]}
//!  ],
//!  "trace": "out/run.csv"}
//! ```
//!
//! Every signal a script names is resolved before anything runs, so a
//! misspelt name costs no simulation. A run ends at the first failed
//! `expect`, `until` or `compare`, and says where.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::memfile;
use super::rig::{Memory, Rig, Signal, Snapshot, StateTarget};
use crate::sim::challenges::expr::Expr;
use crate::sim::source::{parse_int_text, valid_name};

/// Ticks one call may run unless it asks for more.
pub const DEFAULT_MAX_TICKS: u64 = 1_000_000;
/// The most ticks a call may ask for.
pub const MAX_TICKS: u64 = 1_000_000_000;
/// Rows a call may record.
pub const MAX_ROWS: u64 = 5_000_000;
/// Memory words one `read` item returns.
pub const MAX_READ_WORDS: usize = 256;
/// Recorded rows shown from each end of a recording.
const EXCERPT: usize = 5;

/// A simulation that outlives a call: the rig, its named snapshots and
/// signal names.
pub struct RunState {
    pub rig: Rig,
    pub snapshots: HashMap<String, Snapshot>,
    pub aliases: HashMap<String, String>,
}

impl RunState {
    pub fn new(rig: Rig) -> Self {
        Self {
            rig,
            snapshots: HashMap::new(),
            aliases: HashMap::new(),
        }
    }
}

/// What a run did.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub text: String,
    /// An `expect`, `until` or `compare` failed.
    pub failed: bool,
}

/// An expression with the signals its names read.
struct Cond {
    text: String,
    expr: Expr,
    vars: Vec<(String, Signal)>,
}

impl Cond {
    fn eval(&self, rig: &Rig) -> u64 {
        let values: Vec<(&str, u64)> = self
            .vars
            .iter()
            .map(|(n, s)| (n.as_str(), rig.value(s)))
            .collect();
        self.expr.eval(&|name: &str| {
            values
                .iter()
                .find(|(n, _)| *n == name)
                .map_or(0, |(_, v)| *v)
        })
    }
}

enum ReadItem {
    Signal(Signal),
    Words {
        text: String,
        memory: Memory,
        from: usize,
        to: usize,
    },
}

/// Expected rows: per recorded column, the value or nothing to check.
pub(crate) struct Expected {
    pub(crate) what: String,
    pub(crate) rows: Vec<Vec<Option<u64>>>,
    pub(crate) context: usize,
}

enum Step {
    Set(Vec<(String, u64)>),
    /// State targets and their values, written together.
    Poke(Vec<(StateTarget, u64)>),
    Ticks(u64),
    Until {
        cond: Cond,
        max: u64,
    },
    Expect(Vec<(Signal, u64)>),
    Read(Vec<ReadItem>),
    Load {
        memory: Memory,
        load: memfile::Load,
    },
    Store {
        memory: Memory,
        file: PathBuf,
    },
    Record {
        signals: Vec<Signal>,
        when: Option<Cond>,
    },
    Compare(Expected),
    Snapshot(String),
    Restore(String),
    Reset,
}

fn number(v: &Value) -> Result<u64, String> {
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(|| format!("{n} is not a value")),
        Value::String(s) => parse_int_text(s).ok_or_else(|| format!("\"{s}\" is not a value")),
        Value::Bool(b) => Ok(u64::from(*b)),
        other => Err(format!("{other} is not a value")),
    }
}

fn hex(value: u64, width: u8) -> String {
    if width <= 4 {
        value.to_string()
    } else {
        format!(
            "0x{value:0digits$X}",
            digits = usize::from(width).div_ceil(4)
        )
    }
}

struct Reader<'a> {
    state: &'a mut RunState,
    /// Every signal the script reads, to watch before running.
    watched: Vec<Signal>,
    /// The columns of the recording in force while reading the steps.
    recording: Vec<Signal>,
}

impl Reader<'_> {
    fn signal(&mut self, name: &str) -> Result<Signal, String> {
        let path = self
            .state
            .aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string());
        let mut signal = self.state.rig.resolve(&path)?;
        signal.text = name.to_string();
        self.watched.push(signal.clone());
        Ok(signal)
    }

    fn cond(&mut self, text: &str) -> Result<Cond, String> {
        let expr = Expr::parse(text).map_err(|e| format!("\"{text}\": {e}"))?;
        let mut vars = Vec::new();
        for name in expr.names() {
            if !vars.iter().any(|(n, _)| n == name) {
                vars.push((name.to_string(), self.signal(name)?));
            }
        }
        Ok(Cond {
            text: text.to_string(),
            expr,
            vars,
        })
    }

    fn step(&mut self, step: &Value) -> Result<Step, String> {
        let Value::Object(step) = step else {
            return Err("a step is an object, e.g. {\"ticks\": 10}".into());
        };
        let mut map = step.clone();
        let take = |map: &mut Map<String, Value>, key: &str| map.shift_remove(key);
        let done = |map: &Map<String, Value>| match map.keys().next() {
            None => Ok(()),
            Some(k) => Err(format!("unknown field \"{k}\"")),
        };
        let strings = |v: &Value, what: &str| -> Result<Vec<String>, String> {
            match v {
                Value::String(s) => Ok(vec![s.clone()]),
                Value::Array(items) => items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("\"{what}\" takes signal names (strings)"))
                    })
                    .collect(),
                _ => Err(format!("\"{what}\" takes an array of signal names")),
            }
        };

        if let Some(v) = take(&mut map, "set") {
            done(&map)?;
            let Value::Object(pairs) = v else {
                return Err("\"set\" takes {input: value, ...}".into());
            };
            let pairs = pairs
                .iter()
                .map(|(k, v)| {
                    let value = number(v).map_err(|e| format!("\"{k}\": {e}"))?;
                    // Checked now, so that a bad `set` costs no simulation.
                    self.state.rig.check_input(k, value)?;
                    Ok((k.clone(), value))
                })
                .collect::<Result<_, String>>()?;
            Ok(Step::Set(pairs))
        } else if let Some(v) = take(&mut map, "poke") {
            done(&map)?;
            let Value::Object(pairs) = v else {
                return Err(
                    "\"poke\" takes {state: value, ..., memory: [[address, value], ...]}".into(),
                );
            };
            let mut list = Vec::new();
            for (name, v) in &pairs {
                let path = self
                    .state
                    .aliases
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                if let Value::Array(words) = v {
                    let memory = self.state.rig.memory(&path)?;
                    for (i, pair) in words.iter().enumerate() {
                        let at = |e: String| format!("\"{name}\"[{i}]: {e}");
                        let (Some(addr), Some(value), 2) = (
                            pair.get(0),
                            pair.get(1),
                            pair.as_array().map_or(0, Vec::len),
                        ) else {
                            return Err(at("a word is [address, value]".into()));
                        };
                        let addr = number(addr).map_err(at)? as usize;
                        let mut target = self.state.rig.word_target(&memory, addr).map_err(at)?;
                        target.text = format!("{name}@0x{addr:X}");
                        let value = number(value).map_err(at)?;
                        target.fits(value)?;
                        list.push((target, value));
                    }
                } else {
                    let mut target = self.state.rig.target(&path)?;
                    target.text = name.clone();
                    let value = number(v).map_err(|e| format!("\"{name}\": {e}"))?;
                    target.fits(value)?;
                    list.push((target, value));
                }
            }
            Ok(Step::Poke(list))
        } else if let Some(v) = take(&mut map, "ticks") {
            done(&map)?;
            Ok(Step::Ticks(number(&v)?))
        } else if let Some(v) = take(&mut map, "until") {
            let max = match take(&mut map, "max_ticks") {
                Some(v) => number(&v)?,
                None => return Err("\"until\" needs \"max_ticks\"".into()),
            };
            done(&map)?;
            let Value::String(text) = v else {
                return Err("\"until\" takes an expression (a string)".into());
            };
            Ok(Step::Until {
                cond: self.cond(&text)?,
                max,
            })
        } else if let Some(v) = take(&mut map, "expect") {
            done(&map)?;
            let Value::Object(pairs) = v else {
                return Err("\"expect\" takes {signal: value, ...}".into());
            };
            let mut list = Vec::new();
            for (k, v) in &pairs {
                let signal = self.signal(k)?;
                list.push((signal, number(v)?));
            }
            Ok(Step::Expect(list))
        } else if let Some(v) = take(&mut map, "read") {
            done(&map)?;
            let mut items = Vec::new();
            for text in strings(&v, "read")? {
                items.push(self.read_item(&text)?);
            }
            Ok(Step::Read(items))
        } else if let Some(v) = take(&mut map, "load") {
            done(&map)?;
            let Value::Object(mut spec) = v else {
                return Err("\"load\" takes {\"part\": memory, \"file\": path, ...}".into());
            };
            let Some(Value::String(part)) = spec.shift_remove("part") else {
                return Err("\"load\" needs \"part\" (a memory)".into());
            };
            let memory = self.state.rig.memory(&part)?;
            let load = memfile::read(&Value::Object(spec), memory.width)?;
            // Checked now, so that a load that does not fit costs no
            // simulation.
            memfile::end(memory.addr_bits, &load)?;
            Ok(Step::Load { memory, load })
        } else if let Some(v) = take(&mut map, "store") {
            done(&map)?;
            let (Some(Value::String(part)), Some(Value::String(file))) =
                (v.get("part"), v.get("file"))
            else {
                return Err("\"store\" takes {\"part\": memory, \"file\": path}".into());
            };
            Ok(Step::Store {
                memory: self.state.rig.memory(part)?,
                file: PathBuf::from(file),
            })
        } else if let Some(v) = take(&mut map, "record") {
            let when = match take(&mut map, "when") {
                None => None,
                Some(Value::String(text)) => Some(self.cond(&text)?),
                Some(_) => return Err("\"when\" takes an expression (a string)".into()),
            };
            done(&map)?;
            let mut signals = Vec::new();
            for name in strings(&v, "record")? {
                signals.push(self.signal(&name)?);
            }
            self.recording = signals.clone();
            Ok(Step::Record { signals, when })
        } else if let Some(v) = take(&mut map, "compare") {
            done(&map)?;
            if self.recording.is_empty() {
                return Err("\"compare\" checks recorded rows; put a \"record\" step first".into());
            }
            Ok(Step::Compare(expected(&v, &self.recording)?))
        } else if let Some(v) = take(&mut map, "snapshot") {
            done(&map)?;
            match v {
                Value::String(name) => Ok(Step::Snapshot(name)),
                _ => Err("\"snapshot\" takes a name".into()),
            }
        } else if let Some(v) = take(&mut map, "restore") {
            done(&map)?;
            match v {
                Value::String(name) => Ok(Step::Restore(name)),
                _ => Err("\"restore\" takes a name".into()),
            }
        } else if take(&mut map, "reset").is_some() {
            done(&map)?;
            Ok(Step::Reset)
        } else {
            Err(
                "unknown step (steps: set, poke, ticks, until, expect, read, load, store, \
                 record, compare, snapshot, restore, reset)"
                    .into(),
            )
        }
    }

    fn read_item(&mut self, text: &str) -> Result<ReadItem, String> {
        if let Some((part, range)) = text.split_once('@')
            && let Some((a, b)) = range.split_once("..")
        {
            let memory = self.state.rig.memory(part.trim())?;
            let (Some(from), Some(to)) = (parse_int_text(a.trim()), parse_int_text(b.trim()))
            else {
                return Err(format!("\"{text}\": the range is not two addresses"));
            };
            let (from, to) = (from as usize, to as usize);
            if from >= to || to > 1usize << memory.addr_bits {
                return Err(format!(
                    "\"{text}\": the range must be inside the memory's {} words",
                    1usize << memory.addr_bits
                ));
            }
            if to - from > MAX_READ_WORDS {
                return Err(format!(
                    "\"{text}\": {} words; a read returns at most {MAX_READ_WORDS} (use \
                     \"store\" for more)",
                    to - from
                ));
            }
            return Ok(ReadItem::Words {
                text: text.to_string(),
                memory,
                from,
                to,
            });
        }
        self.signal(text).map(ReadItem::Signal)
    }
}

/// Read the rows a `compare` step expects, as values per recorded column
/// (only the columns' `text` is used). The native suite runner reads
/// CPU logs with it too.
pub(crate) fn expected(spec: &Value, columns: &[Signal]) -> Result<Expected, String> {
    let Value::Object(spec) = spec else {
        return Err("\"compare\" takes {\"file\": path} or {\"rows\": [[...]]}".into());
    };
    let mut map = spec.clone();
    let context = match map.shift_remove("context") {
        Some(v) => number(&v)? as usize,
        None => 5,
    };
    let radix16 = match map.shift_remove("radix") {
        None => false,
        Some(v) => match number(&v)? {
            16 => true,
            10 => false,
            _ => return Err("\"radix\" is 10 or 16".into()),
        },
    };
    let skip_lines = match map.shift_remove("skip_lines") {
        Some(v) => number(&v)? as usize,
        None => 0,
    };
    let column = |name: &str| {
        columns
            .iter()
            .position(|c| c.text.eq_ignore_ascii_case(name))
    };
    // Which recorded column each position of a row holds.
    let positions: Option<Vec<Option<usize>>> = match map.shift_remove("columns") {
        None => None,
        Some(Value::Array(names)) => Some(
            names
                .iter()
                .map(|n| match n {
                    Value::Null => Ok(None),
                    Value::String(s) => column(s)
                        .map(Some)
                        .ok_or_else(|| format!("\"columns\": \"{s}\" is not a recorded signal")),
                    _ => Err("\"columns\" holds signal names, or null to skip one".to_string()),
                })
                .collect::<Result<_, _>>()?,
        ),
        Some(_) => return Err("\"columns\" must be an array".into()),
    };
    let file = map.shift_remove("file");
    let rows = map.shift_remove("rows");
    if let Some(k) = map.keys().next() {
        return Err(format!(
            "unknown field \"{k}\" (a compare has file or rows, columns, radix, skip_lines, \
             context)"
        ));
    }
    let value = |token: &str| -> Option<u64> {
        let t = token.trim_start_matches('$');
        if radix16 {
            let t = t.trim_start_matches("0x").trim_start_matches("0X");
            u64::from_str_radix(t, 16).ok()
        } else {
            parse_int_text(t)
        }
    };
    let blank = || vec![None; columns.len()];
    match (file, rows) {
        (Some(Value::String(path)), None) => {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            let mut positions = positions;
            let mut out = Vec::new();
            for (n, line) in text.lines().enumerate().skip(skip_lines) {
                let line = line.split('#').next().unwrap_or_default();
                let tokens: Vec<&str> = line
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|t| !t.is_empty())
                    .collect();
                if tokens.is_empty() {
                    continue;
                }
                // A first line of signal names says what the columns are.
                // A recorded signal's name is a name even where it reads
                // as a value (`A` in hex).
                if positions.is_none()
                    && out.is_empty()
                    && tokens
                        .iter()
                        .all(|t| !t.contains(':') && (value(t).is_none() || column(t).is_some()))
                {
                    let named: Vec<Option<usize>> = tokens.iter().map(|t| column(t)).collect();
                    if named.iter().all(Option::is_none) {
                        return Err(format!(
                            "{path}: line {}: none of these names is a recorded signal",
                            n + 1
                        ));
                    }
                    positions = Some(named);
                    continue;
                }
                let mut row = blank();
                let mut seen = false;
                let mut position = 0;
                // The column a `NAME:` without a value named: its value is
                // the next token (`PPU:  0`, `A: 1F`).
                let mut named: Option<usize> = None;
                for token in &tokens {
                    // `A:1F` names its column.
                    if let Some((name, v)) = token.split_once(':')
                        && let Some(c) = column(name)
                    {
                        named = None;
                        if v.is_empty() {
                            named = Some(c);
                        } else {
                            row[c] = Some(value(v).ok_or_else(|| {
                                format!("{path}: line {}: \"{token}\" is not a value", n + 1)
                            })?);
                            seen = true;
                        }
                        continue;
                    }
                    if let Some(c) = named.take() {
                        row[c] = Some(value(token).ok_or_else(|| {
                            format!("{path}: line {}: \"{token}\" is not a value", n + 1)
                        })?);
                        seen = true;
                        continue;
                    }
                    let target = match &positions {
                        Some(p) => p.get(position).copied().flatten(),
                        None => (position < columns.len()).then_some(position),
                    };
                    position += 1;
                    if let Some(c) = target {
                        row[c] = Some(value(token).ok_or_else(|| {
                            format!("{path}: line {}: \"{token}\" is not a value", n + 1)
                        })?);
                        seen = true;
                    }
                }
                if seen {
                    out.push(row);
                }
            }
            if out.is_empty() {
                return Err(format!("{path}: no rows to compare with"));
            }
            Ok(Expected {
                what: path,
                rows: out,
                context,
            })
        }
        (None, Some(Value::Array(items))) => {
            let positions = positions.unwrap_or_else(|| (0..columns.len()).map(Some).collect());
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let Value::Array(values) = item else {
                    return Err(format!("rows[{i}] must be an array of values"));
                };
                let mut row = blank();
                for (v, c) in values.iter().zip(&positions) {
                    if let (Some(c), false) = (c, v.is_null()) {
                        row[*c] = Some(number(v).map_err(|e| format!("rows[{i}]: {e}"))?);
                    }
                }
                out.push(row);
            }
            if out.is_empty() {
                return Err("\"rows\" is empty: there is nothing to compare with".into());
            }
            Ok(Expected {
                what: "rows".into(),
                rows: out,
                context,
            })
        }
        _ => Err("\"compare\" takes {\"file\": path} or {\"rows\": [[...]]}".into()),
    }
}

/// Where recorded rows go besides the excerpt: a CSV or VCD file.
enum Trace {
    Csv(std::io::BufWriter<std::fs::File>),
    Vcd {
        out: std::io::BufWriter<std::fs::File>,
        last: Vec<Option<u64>>,
        declared: bool,
    },
}

fn vcd_id(i: usize) -> String {
    // Printable ASCII from '!' on, as VCD identifiers are.
    let mut i = i;
    let mut id = String::new();
    loop {
        id.push((b'!' + (i % 94) as u8) as char);
        i /= 94;
        if i == 0 {
            return id;
        }
    }
}

impl Trace {
    fn open(path: &Path) -> Result<Trace, String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let out = std::io::BufWriter::new(file);
        Ok(
            if path.extension().and_then(|e| e.to_str()) == Some("vcd") {
                Trace::Vcd {
                    out,
                    last: Vec::new(),
                    declared: false,
                }
            } else {
                Trace::Csv(out)
            },
        )
    }

    fn columns(&mut self, columns: &[Signal]) -> Result<(), String> {
        let io = |e: std::io::Error| format!("trace: {e}");
        match self {
            Trace::Csv(out) => {
                let names: Vec<&str> = columns.iter().map(|c| c.text.as_str()).collect();
                writeln!(out, "tick,{}", names.join(",")).map_err(io)
            }
            Trace::Vcd {
                out,
                last,
                declared,
            } => {
                if *declared {
                    return Err(
                        "a VCD trace holds one set of signals; use one \"record\" step, or a \
                         .csv trace"
                            .into(),
                    );
                }
                *declared = true;
                *last = vec![None; columns.len()];
                writeln!(out, "$timescale 1 ns $end\n$scope module gatequest $end").map_err(io)?;
                for (i, c) in columns.iter().enumerate() {
                    let name: String = c
                        .text
                        .chars()
                        .map(|ch| {
                            if ch.is_ascii_alphanumeric() || ch == '_' {
                                ch
                            } else {
                                '_'
                            }
                        })
                        .collect();
                    writeln!(out, "$var wire {} {} {name} $end", c.width, vcd_id(i)).map_err(io)?;
                }
                writeln!(out, "$upscope $end\n$enddefinitions $end").map_err(io)
            }
        }
    }

    fn row(&mut self, tick: u64, columns: &[Signal], row: &[u64]) -> Result<(), String> {
        let io = |e: std::io::Error| format!("trace: {e}");
        match self {
            Trace::Csv(out) => {
                write!(out, "{tick}").map_err(io)?;
                for (v, c) in row.iter().zip(columns) {
                    write!(out, ",{}", hex(*v, c.width)).map_err(io)?;
                }
                writeln!(out).map_err(io)
            }
            Trace::Vcd { out, last, .. } => {
                let mut stamped = false;
                for (i, (v, c)) in row.iter().zip(columns).enumerate() {
                    if last[i] == Some(*v) {
                        continue;
                    }
                    last[i] = Some(*v);
                    if !stamped {
                        writeln!(out, "#{tick}").map_err(io)?;
                        stamped = true;
                    }
                    if c.width == 1 {
                        writeln!(out, "{v}{}", vcd_id(i)).map_err(io)?;
                    } else {
                        writeln!(out, "b{v:b} {}", vcd_id(i)).map_err(io)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn finish(&mut self) -> Result<(), String> {
        match self {
            Trace::Csv(out) | Trace::Vcd { out, .. } => {
                out.flush().map_err(|e| format!("trace: {e}"))
            }
        }
    }
}

/// The recording in force while the steps run.
#[derive(Default)]
struct Recording {
    signals: Vec<Signal>,
    when: Option<Cond>,
    /// Rows of this recording, and of every recording of the call.
    rows: u64,
    total: u64,
    first: Vec<(u64, Vec<u64>)>,
    recent: VecDeque<(u64, Vec<u64>)>,
    expected: Option<Expected>,
    /// The next expected row.
    at: usize,
    compared: Vec<String>,
}

impl Recording {
    /// Say what was compared and recorded, then forget the rows: the next
    /// `record` step starts a recording of its own, with its own columns.
    /// `ended` says why a comparison stopped short, if it may have.
    fn report(&mut self, text: &mut String, ended: Option<&str>) {
        for line in self.compared.drain(..) {
            let _ = writeln!(text, "{line}");
        }
        if let (Some(expected), Some(ended)) = (&self.expected, ended) {
            let _ = writeln!(
                text,
                "compared {} of {} rows of {}: all match so far ({ended})",
                self.at,
                expected.rows.len(),
                expected.what
            );
        }
        if self.rows > 0 {
            let _ = writeln!(text, "recorded {} rows:", self.rows);
            for (t, r) in &self.first {
                let _ = writeln!(text, "  {}", row_text(&self.signals, *t, r));
            }
            let shown = self.first.len() as u64;
            let tail: Vec<_> = self
                .recent
                .iter()
                .filter(|(t, _)| self.first.last().is_none_or(|(ft, _)| t > ft))
                .collect();
            let tail = &tail[tail.len().saturating_sub(EXCERPT)..];
            if self.rows > shown + tail.len() as u64 {
                let _ = writeln!(text, "  ...");
            }
            for (t, r) in tail {
                let _ = writeln!(text, "  {}", row_text(&self.signals, *t, r));
            }
        }
        self.rows = 0;
        self.first.clear();
        self.recent.clear();
        self.expected = None;
    }
}

fn row_text(signals: &[Signal], tick: u64, row: &[u64]) -> String {
    let cells: Vec<String> = signals
        .iter()
        .zip(row)
        .map(|(s, v)| format!("{}={}", s.text, hex(*v, s.width)))
        .collect();
    format!("tick {tick}: {}", cells.join(" "))
}

struct Runner<'a> {
    state: &'a mut RunState,
    out: String,
    recording: Recording,
    trace: Option<Trace>,
    /// Ticks this call may still run.
    budget: u64,
    ran: u64,
}

impl Runner<'_> {
    /// Record the row of the tick just run; `Err` is a failed compare.
    fn record(&mut self) -> Result<Result<(), String>, String> {
        let rec = &mut self.recording;
        if rec.signals.is_empty() {
            return Ok(Ok(()));
        }
        let rig = &self.state.rig;
        if rec.when.as_ref().is_some_and(|w| w.eval(rig) == 0) {
            return Ok(Ok(()));
        }
        let tick = rig.tick_count();
        let row: Vec<u64> = rec.signals.iter().map(|s| rig.value(s)).collect();
        rec.rows += 1;
        rec.total += 1;
        if rec.total > MAX_ROWS {
            return Err(format!(
                "more than {MAX_ROWS} rows recorded; narrow the recording with \"when\""
            ));
        }
        if let Some(trace) = &mut self.trace {
            trace.row(tick, &rec.signals, &row)?;
        }
        if rec.first.len() < EXCERPT {
            rec.first.push((tick, row.clone()));
        }
        let keep = EXCERPT.max(rec.expected.as_ref().map_or(0, |e| e.context + 1));
        // `keep` shrinks when a comparison ends: drop down to it, or the
        // buffer would never be at the limit again and keep every row.
        while rec.recent.len() >= keep {
            rec.recent.pop_front();
        }
        rec.recent.push_back((tick, row.clone()));

        let Some(expected) = &rec.expected else {
            return Ok(Ok(()));
        };
        let want = &expected.rows[rec.at];
        let wrong: Vec<usize> = (0..row.len())
            .filter(|&c| want[c].is_some_and(|w| w != row[c]))
            .collect();
        if !wrong.is_empty() {
            let mut text = format!(
                "first difference at row {} of {} (tick {tick}):\n",
                rec.at + 1,
                expected.what
            );
            for &c in &wrong {
                let s = &rec.signals[c];
                let _ = writeln!(
                    text,
                    "  {}: expected {}, got {}",
                    s.text,
                    hex(want[c].unwrap_or(0), s.width),
                    hex(row[c], s.width)
                );
            }
            let _ = writeln!(text, "rows before it (and the differing one):");
            let from = rec.recent.len().saturating_sub(expected.context + 1);
            for (t, r) in rec.recent.iter().skip(from) {
                let _ = writeln!(text, "  {}", row_text(&rec.signals, *t, r));
            }
            return Ok(Err(text));
        }
        rec.at += 1;
        if rec.at == expected.rows.len() {
            rec.compared.push(format!(
                "compared {} rows with {}: all match",
                expected.rows.len(),
                expected.what
            ));
            rec.expected = None;
        }
        Ok(Ok(()))
    }

    /// Run up to `ticks` ticks, stopping when `until` holds. `Ok(Err)`
    /// is a failure of the experiment, `Err` a broken script or limit.
    fn advance(
        &mut self,
        ticks: u64,
        until: Option<&Cond>,
    ) -> Result<Result<bool, String>, String> {
        if until.is_some_and(|c| c.eval(&self.state.rig) != 0) {
            return Ok(Ok(true));
        }
        let over = |left: u64| {
            format!(
                "the call would pass its limit of ticks ({left} left); raise \"max_ticks\" of \
                 the script (at most {MAX_TICKS})"
            )
        };
        // An `until` may stop early, so only the ticks it runs count: it
        // starts even when its own maximum is more than what is left.
        if until.is_none() && ticks > self.budget {
            return Err(over(self.budget));
        }
        if until.is_none() && self.recording.signals.is_empty() {
            self.budget -= ticks;
            self.state.rig.run(ticks);
            self.ran += ticks;
            return Ok(Ok(false));
        }
        for _ in 0..ticks {
            if self.budget == 0 {
                return Err(over(0));
            }
            self.budget -= 1;
            self.state.rig.tick();
            self.ran += 1;
            if let Err(failure) = self.record()? {
                return Ok(Err(failure));
            }
            if until.is_some_and(|c| c.eval(&self.state.rig) != 0) {
                return Ok(Ok(true));
            }
        }
        Ok(Ok(false))
    }

    /// Run one step; `Ok(Err)` ends the run as failed.
    fn step(&mut self, step: Step) -> Result<Result<(), String>, String> {
        match step {
            Step::Set(pairs) => {
                for (name, value) in pairs {
                    self.state.rig.set(&name, value)?;
                }
            }
            Step::Poke(list) => {
                let pokes: Vec<(&StateTarget, u64)> = list.iter().map(|(t, v)| (t, *v)).collect();
                self.state.rig.poke(&pokes)?;
                let _ = writeln!(
                    self.out,
                    "poked {} value(s) at tick {}",
                    pokes.len(),
                    self.state.rig.tick_count()
                );
            }
            Step::Ticks(n) => {
                if let Err(f) = self.advance(n, None)? {
                    return Ok(Err(f));
                }
            }
            Step::Until { cond, max } => match self.advance(max, Some(&cond))? {
                Err(f) => return Ok(Err(f)),
                Ok(true) => {
                    let _ = writeln!(
                        self.out,
                        "until {}: reached at tick {}",
                        cond.text,
                        self.state.rig.tick_count()
                    );
                }
                Ok(false) => {
                    let values: Vec<String> = cond
                        .vars
                        .iter()
                        .map(|(n, s)| format!("{n}={}", hex(self.state.rig.value(s), s.width)))
                        .collect();
                    return Ok(Err(format!(
                        "until {}: not reached in {max} ticks (tick {}; {})",
                        cond.text,
                        self.state.rig.tick_count(),
                        values.join(" ")
                    )));
                }
            },
            Step::Expect(list) => {
                let wrong: Vec<String> = list
                    .iter()
                    .filter_map(|(s, want)| {
                        let got = self.state.rig.value(s);
                        (got != *want).then(|| {
                            format!(
                                "{}: expected {}, got {}",
                                s.text,
                                hex(*want, s.width),
                                hex(got, s.width)
                            )
                        })
                    })
                    .collect();
                if !wrong.is_empty() {
                    return Ok(Err(format!(
                        "expect at tick {}: {}",
                        self.state.rig.tick_count(),
                        wrong.join("; ")
                    )));
                }
            }
            Step::Read(items) => {
                let _ = writeln!(self.out, "read at tick {}:", self.state.rig.tick_count());
                for item in items {
                    match item {
                        ReadItem::Signal(s) => {
                            let v = self.state.rig.value(&s);
                            let _ = writeln!(self.out, "  {} = {} ({v})", s.text, hex(v, s.width));
                        }
                        ReadItem::Words {
                            text,
                            memory,
                            from,
                            to,
                        } => {
                            let words = self.state.rig.words(&memory);
                            let digits = usize::from(memory.width).div_ceil(4);
                            for (i, addr) in (from..to).enumerate() {
                                if i % 16 == 0 {
                                    if i > 0 {
                                        self.out.push('\n');
                                    }
                                    let _ = write!(self.out, "  {text} 0x{addr:04X}:");
                                }
                                let w = words.get(addr).copied().unwrap_or(0);
                                let _ = write!(self.out, " {w:0digits$X}");
                            }
                            self.out.push('\n');
                        }
                    }
                }
            }
            Step::Load { memory, load } => {
                self.state.rig.load(&memory, &load)?;
                let _ = writeln!(self.out, "loaded {} words at {}", load.words.len(), load.at);
            }
            Step::Store { memory, file } => {
                let words = self.state.rig.words(&memory);
                let hex_text = matches!(
                    file.extension().and_then(|e| e.to_str()),
                    Some("hex" | "txt")
                );
                let bytes: Vec<u8> = if hex_text {
                    let digits = usize::from(memory.width).div_ceil(4);
                    let mut text = String::new();
                    for (i, w) in words.iter().enumerate() {
                        let _ = write!(text, "{w:0digits$X}");
                        text.push(if i % 16 == 15 { '\n' } else { ' ' });
                    }
                    text.into_bytes()
                } else {
                    let n = usize::from(memory.width).div_ceil(8);
                    words
                        .iter()
                        .flat_map(|w| w.to_le_bytes().into_iter().take(n))
                        .collect()
                };
                if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
                let _ = writeln!(
                    self.out,
                    "stored {} words in {}",
                    words.len(),
                    file.display()
                );
            }
            Step::Record { signals, when } => {
                if let (Some(trace), false) = (&mut self.trace, signals.is_empty()) {
                    trace.columns(&signals)?;
                }
                // The rows so far have the columns of the recording before.
                let rec = &mut self.recording;
                rec.report(&mut self.out, Some("another recording began"));
                rec.signals = signals;
                rec.when = when;
            }
            Step::Compare(expected) => {
                self.recording.expected = Some(expected);
                self.recording.at = 0;
            }
            Step::Snapshot(name) => {
                let snapshot = self.state.rig.snapshot();
                let _ = writeln!(
                    self.out,
                    "snapshot \"{name}\" at tick {}",
                    snapshot.state.tick
                );
                self.state.snapshots.insert(name, snapshot);
            }
            Step::Restore(name) => {
                let Some(snapshot) = self.state.snapshots.get(&name) else {
                    let mut names: Vec<&str> =
                        self.state.snapshots.keys().map(String::as_str).collect();
                    names.sort_unstable();
                    return Err(format!(
                        "no snapshot \"{name}\" (snapshots: {})",
                        if names.is_empty() {
                            "none".into()
                        } else {
                            names.join(", ")
                        }
                    ));
                };
                self.state.rig.restore(snapshot);
                let _ = writeln!(
                    self.out,
                    "restored \"{name}\" (tick {})",
                    snapshot.state.tick
                );
            }
            Step::Reset => self.state.rig.reset(),
        }
        Ok(Ok(()))
    }
}

/// Run `script` on `state`. `Err` is a script that could not be read or
/// run (nothing was simulated if it could not be read); a failed
/// `expect`, `until` or `compare` is an [`Outcome`] with `failed`.
pub fn run(state: &mut RunState, script: &Map<String, Value>) -> Result<Outcome, String> {
    let mut map = script.clone();
    if let Some(v) = map.shift_remove("signals") {
        let Value::Object(names) = v else {
            return Err("\"signals\" takes {name: signal path, ...}".into());
        };
        for (name, path) in names {
            let Value::String(path) = path else {
                return Err(format!(
                    "\"signals\": \"{name}\" must be a signal path (a string)"
                ));
            };
            if !valid_name(&name) {
                return Err(format!(
                    "\"signals\": \"{name}\" must be a name: letters, digits and '_'"
                ));
            }
            state.aliases.insert(name, path);
        }
    }
    let budget = match map.shift_remove("max_ticks") {
        None => DEFAULT_MAX_TICKS,
        Some(v) => number(&v)?.min(MAX_TICKS),
    };
    let trace_path = match map.shift_remove("trace") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) => Some(PathBuf::from(path)),
        Some(_) => return Err("\"trace\" takes a file path".into()),
    };
    let Some(Value::Array(steps)) = map.shift_remove("steps") else {
        return Err("a script needs \"steps\" (an array)".into());
    };
    if let Some(k) = map.keys().next() {
        return Err(format!(
            "unknown field \"{k}\" (a script has steps, signals, trace, max_ticks)"
        ));
    }

    // Read every step, then make every signal readable at once.
    let mut reader = Reader {
        state,
        watched: Vec::new(),
        recording: Vec::new(),
    };
    let mut read = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        read.push(reader.step(step).map_err(|e| format!("steps[{i}]: {e}"))?);
    }
    let watched = std::mem::take(&mut reader.watched);
    state.rig.watch(&watched)?;

    let mut runner = Runner {
        state,
        out: String::new(),
        recording: Recording::default(),
        trace: trace_path.as_deref().map(Trace::open).transpose()?,
        budget,
        ran: 0,
    };
    let mut failure = None;
    for (i, step) in read.into_iter().enumerate() {
        match runner.step(step).map_err(|e| format!("steps[{i}]: {e}"))? {
            Ok(()) => {}
            Err(f) => {
                failure = Some(format!("steps[{i}]: {f}"));
                break;
            }
        }
    }
    if let Some(trace) = &mut runner.trace {
        trace.finish()?;
    }

    let mut text = std::mem::take(&mut runner.out);
    let ended = failure.is_none().then_some("the run ended first");
    runner.recording.report(&mut text, ended);
    if let Some(path) = &trace_path {
        let _ = writeln!(text, "trace: {}", path.display());
    }
    let _ = writeln!(
        text,
        "ran {} ticks; now at tick {}",
        runner.ran,
        runner.state.rig.tick_count()
    );
    let failed = failure.is_some();
    match failure {
        Some(f) => {
            let _ = writeln!(text, "FAILED {f}");
        }
        None => text.push_str("ok\n"),
    }
    Ok(Outcome { text, failed })
}
