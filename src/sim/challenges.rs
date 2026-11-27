//! Build-it challenges on the demo road: a brief, a starter workspace with
//! the ports placed and labelled, the parts the player may use, and a test
//! that [`Challenge::check`] runs on the player's circuit.
//!
//! The challenges are listed in the demo manifest (`"challenges"`, see
//! `docs/save-format.md`); each one's starter is a version 6 file
//! `challenge_<id>.json` beside the demos, expanded under the key
//! `challenges/<id>`. A test is one of
//!
//! - **table**: rows of input values followed by the expected outputs
//!   (combinational circuits);
//! - **formula**: an expression per output over the inputs ([`expr`]),
//!   checked for every input combination, or for `samples` random ones
//!   from a fixed `seed` (plus all-zero and all-ones) when the inputs have
//!   more than 16 bits between them;
//! - **sequence**: steps that set inputs, run some ticks from power-on and
//!   then expect outputs (sequential circuits).
//!
//! A check never touches the running app: it builds a separate
//! [`logi_sim::sim::Sim`] from the workspace ([`super::netlist::design_of`])
//! and runs the cases on it, which takes microseconds to milliseconds.

pub mod expr;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::source::{self, SourceFile};
use super::{DemoCatalog, GateKind, WorkspaceDocument};
use expr::Expr;

/// Inputs of a `formula` test with more bits than this between them need
/// `samples` (2^16 cases is the most an exhaustive check runs).
pub const MAX_EXHAUSTIVE_BITS: u32 = 16;

/// The `type` names of the primitives, as the save format writes them
/// (`allowed` and `forbidden` use them; `SubCircuit` is not one).
pub const PRIMITIVE_TYPES: &[&str] = &[
    "Input",
    "Output",
    "Display",
    "Const",
    "Not",
    "And",
    "Or",
    "Xor",
    "Nand",
    "Nor",
    "Xnor",
    "Mux",
    "Switch",
    "Add",
    "Eq",
    "LtU",
    "ReduceOr",
    "ReduceAnd",
    "ReduceXor",
    "Split",
    "Merge",
    "Extend",
    "Clock",
    "Rom",
    "Delay",
    "Register",
    "DLatch",
    "SrLatch",
    "Ram",
    "Ram2",
    "Screen",
];

pub(crate) fn parse_int(text: &str) -> Option<u64> {
    source::parse_int_text(text)
}

/// The save format's `type` of a primitive kind (`None` for a subcircuit).
pub fn type_name(kind: &GateKind) -> Option<String> {
    if matches!(kind, GateKind::SubCircuit { .. }) {
        return None;
    }
    serde_json::to_value(kind)
        .ok()?
        .get("type")?
        .as_str()
        .map(str::to_string)
}

// ─────────────────────────────────────────────────────────────────────────────
// The manifest entry
// ─────────────────────────────────────────────────────────────────────────────

/// A challenge as the manifest writes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestChallenge {
    id: String,
    level: u32,
    title: String,
    brief: String,
    #[serde(default)]
    allowed: Option<Vec<String>>,
    #[serde(default)]
    forbidden: Vec<String>,
    #[serde(default)]
    uses: Vec<String>,
    test: TestSpec,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum TestSpec {
    Table {
        inputs: Vec<String>,
        outputs: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    Formula {
        expect: Map<String, Value>,
        #[serde(default)]
        samples: Option<u32>,
        #[serde(default)]
        seed: u64,
    },
    Sequence {
        steps: Vec<StepSpec>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StepSpec {
    #[serde(default)]
    set: Map<String, Value>,
    #[serde(default = "one")]
    ticks: u32,
    #[serde(default)]
    expect: Map<String, Value>,
}

fn one() -> u32 {
    1
}

fn value(v: &Value) -> Result<u64, String> {
    match v {
        Value::Number(n) => n.as_u64().ok_or_else(|| format!("{n} is not a value")),
        Value::String(s) => parse_int(s).ok_or_else(|| format!("\"{s}\" is not a value")),
        other => Err(format!("{other} is not a value")),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A loaded challenge
// ─────────────────────────────────────────────────────────────────────────────

/// What a challenge tests.
#[derive(Debug, Clone)]
pub enum ChallengeTest {
    /// Rows of input values and the outputs they give.
    Table {
        inputs: Vec<String>,
        outputs: Vec<String>,
        rows: Vec<Vec<u64>>,
    },
    /// Each output as an expression over the inputs.
    Formula {
        expect: Vec<(String, Expr)>,
        /// Random cases instead of all of them.
        samples: Option<u32>,
        seed: u64,
    },
    /// Steps from power-on.
    Sequence { steps: Vec<Step> },
}

/// One step of a sequence: set inputs, run ticks, compare outputs.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub set: Vec<(String, u64)>,
    pub ticks: u32,
    pub expect: Vec<(String, u64)>,
}

/// A port of the starter: its label and width.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub name: String,
    pub w: u8,
}

/// One loaded challenge.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub id: String,
    pub level: u32,
    pub title: String,
    /// What to build, in a sentence or two.
    pub brief: String,
    /// The only primitives the circuit may use besides ports (`None`: any).
    pub allowed: Option<Vec<String>>,
    /// Primitives, and `"demo/circuit"` chips, it must not use.
    pub forbidden: Vec<String>,
    /// The forbidden chips: reference, definition id, name for the player.
    forbidden_defs: Vec<(String, Uuid, String)>,
    pub test: ChallengeTest,
    /// The starter file as written.
    pub source: SourceFile,
    /// The starter: the ports, placed and labelled.
    pub starter: Arc<WorkspaceDocument>,
    /// The starter's inputs and outputs, top to bottom.
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}

/// The file of a challenge's starter, beside the demos.
pub fn starter_file(id: &str) -> String {
    format!("challenge_{id}.json")
}

/// The key that derives a starter's ids.
pub fn challenge_key(id: &str) -> String {
    format!("challenges/{id}")
}

impl Challenge {
    /// The name of the player's workspace for this challenge (the editor
    /// recognises a challenge by it, also after a save and a reload).
    pub fn workspace_name(&self) -> String {
        format!("{} (challenge)", self.title)
    }

    /// The parts rule in words ("Only NAND", "No ADD"), if there is one.
    pub fn parts_rule(&self) -> Option<String> {
        let display = |t: &str| -> String {
            source::primitive_kind(t, &Map::new())
                .map(|k| k.display_name().to_string())
                .unwrap_or_else(|_| t.to_uppercase())
        };
        let mut parts = Vec::new();
        if let Some(allowed) = &self.allowed {
            let names: Vec<String> = allowed.iter().map(|t| display(t)).collect();
            parts.push(format!("Only {} (and the ports)", names.join(", ")));
        }
        if !self.forbidden.is_empty() {
            let names: Vec<String> = self
                .forbidden
                .iter()
                .map(
                    |f| match self.forbidden_defs.iter().find(|(r, ..)| r == f) {
                        Some((_, _, label)) => label.clone(),
                        None => display(f),
                    },
                )
                .collect();
            parts.push(format!("No {}", names.join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// Whether the rules let the circuit use primitive `kind` (ports always).
    pub fn allows_kind(&self, kind: &GateKind) -> bool {
        if kind.is_port() {
            return true;
        }
        let Some(name) = type_name(kind) else {
            return true; // chips: checked by what is inside
        };
        self.allowed.as_ref().is_none_or(|a| a.contains(&name)) && !self.forbidden.contains(&name)
    }

    /// The first part of `workspace` the rules refuse: (what, where).
    pub fn refused_part(&self, workspace: &WorkspaceDocument) -> Option<(String, String)> {
        let circuits =
            std::iter::once(&workspace.root).chain(workspace.definitions.values().map(|d| &**d));
        for circuit in circuits {
            for node in &circuit.nodes {
                let refused = match &node.kind {
                    GateKind::SubCircuit { definition } => self
                        .forbidden_defs
                        .iter()
                        .find(|(_, id, _)| id == definition)
                        .map(|(_, _, label)| label.clone()),
                    kind => (!self.allows_kind(kind)).then(|| kind.display_name().to_string()),
                };
                if let Some(what) = refused {
                    return Some((what, circuit.name.clone()));
                }
            }
        }
        None
    }

    /// Whether a chip (a workspace placed as a cell) is all allowed parts.
    pub fn allows_chip(&self, chip: &WorkspaceDocument) -> bool {
        !self
            .forbidden_defs
            .iter()
            .any(|(_, id, _)| *id == chip.root.id)
            && self.refused_part(chip).is_none()
    }

    /// Run the test on `workspace`.
    pub fn check(&self, workspace: &WorkspaceDocument) -> CheckReport {
        let fail = |failure| CheckReport {
            cases: 0,
            failure: Some(failure),
        };
        if let Some((part, circuit)) = self.refused_part(workspace) {
            return fail(Failure::Part {
                part,
                circuit: (circuit != workspace.root.name).then_some(circuit),
                rule: self.parts_rule().unwrap_or_default(),
            });
        }
        let design = match super::netlist::design_of(workspace) {
            Ok(d) => d,
            Err(e) => return fail(Failure::Build(e)),
        };
        let program =
            match logi_sim::compile::compile(&design, &logi_sim::compile::Options::default()) {
                Ok(p) => p,
                Err(errors) => {
                    let loops = errors.iter().any(|e| {
                        matches!(e, logi_sim::compile::CompileError::CombinationalLoop { .. })
                    });
                    return fail(Failure::Build(if loops {
                        "it has a loop of gates with no Delay or latch in it, so it never \
                         settles"
                            .into()
                    } else {
                        errors
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("; ")
                    }));
                }
            };
        // The ports the test drives and reads, as the starter has them.
        let ports = &program.ports;
        for p in &self.inputs {
            match ports.input(&p.name) {
                None => return fail(Failure::Port(format!("there is no input \"{}\"", p.name))),
                Some(i) if ports.inputs[i].width != p.w => {
                    return fail(Failure::Port(format!(
                        "input \"{}\" is {} bits wide; it should be {}",
                        p.name, ports.inputs[i].width, p.w
                    )));
                }
                _ => {}
            }
        }
        for p in &self.outputs {
            match ports.output(&p.name) {
                None => return fail(Failure::Port(format!("there is no output \"{}\"", p.name))),
                Some(i) if ports.outputs[i].width != p.w => {
                    return fail(Failure::Port(format!(
                        "output \"{}\" is {} bits wide; it should be {}",
                        p.name, ports.outputs[i].width, p.w
                    )));
                }
                _ => {}
            }
        }
        let sim = match logi_sim::sim::Sim::try_new(Arc::new(program)) {
            Ok(s) => s,
            Err(e) => return fail(Failure::Build(e.0)),
        };
        PortTest {
            inputs: &self.inputs,
            outputs: &self.outputs,
            test: &self.test,
        }
        .run(sim)
    }
}

/// A test with the ports it drives and reads: what a challenge runs on
/// the player's circuit, and what a stored test runs on a chip
/// (`crate::mcp::testing`).
pub struct PortTest<'a> {
    pub inputs: &'a [Port],
    pub outputs: &'a [Port],
    pub test: &'a ChallengeTest,
}

impl PortTest<'_> {
    /// The width of the port `name` among `ports` (the inputs or the
    /// outputs: an input and an output may share a label, with different
    /// widths).
    fn width(ports: &[Port], name: &str) -> u8 {
        ports.iter().find(|p| p.name == name).map_or(64, |p| p.w)
    }

    /// `values` of `ports` (the inputs or the outputs) as signals.
    fn signals(ports: &[Port], values: impl IntoIterator<Item = (String, u64)>) -> Vec<Signal> {
        values
            .into_iter()
            .map(|(name, value)| Signal {
                w: Self::width(ports, &name),
                name,
                value,
            })
            .collect()
    }

    /// Run the test on a simulation at power-on.
    pub fn run(&self, mut sim: logi_sim::sim::Sim) -> CheckReport {
        let read = |sim: &logi_sim::sim::Sim, names: &[String]| -> Vec<(String, u64)> {
            names.iter().map(|n| (n.clone(), sim.get(n))).collect()
        };
        // A combinational case: drive the inputs, settle, compare.
        let case = |sim: &mut logi_sim::sim::Sim,
                    n: usize,
                    inputs: Vec<(String, u64)>,
                    expected: Vec<(String, u64)>|
         -> Option<Failure> {
            for (name, v) in &inputs {
                sim.set(name, *v);
            }
            sim.tick();
            let names: Vec<String> = expected.iter().map(|(n, _)| n.clone()).collect();
            let got = read(sim, &names);
            (got != expected).then(|| {
                Failure::Case(Box::new(FailedCase {
                    case: n,
                    tick: None,
                    inputs: Self::signals(self.inputs, inputs),
                    expected: Self::signals(self.outputs, expected),
                    got: Self::signals(self.outputs, got),
                }))
            })
        };
        let mut cases = 0;
        match &self.test {
            ChallengeTest::Table {
                inputs,
                outputs: outs,
                rows,
            } => {
                for row in rows {
                    cases += 1;
                    let (ins, exp) = row.split_at(inputs.len());
                    let ins = inputs.iter().cloned().zip(ins.iter().copied()).collect();
                    let exp = outs.iter().cloned().zip(exp.iter().copied()).collect();
                    if let Some(f) = case(&mut sim, cases, ins, exp) {
                        return CheckReport::failed(cases, f);
                    }
                }
            }
            ChallengeTest::Formula {
                expect,
                samples,
                seed,
            } => {
                for values in formula_cases(self.inputs, *samples, *seed) {
                    cases += 1;
                    let ins: Vec<(String, u64)> = self
                        .inputs
                        .iter()
                        .map(|p| p.name.clone())
                        .zip(values)
                        .collect();
                    let var = |n: &str| ins.iter().find(|(k, _)| k == n).map_or(0, |(_, v)| *v);
                    let exp = expect
                        .iter()
                        .map(|(name, e)| {
                            (
                                name.clone(),
                                e.eval(&var) & mask(Self::width(self.outputs, name)),
                            )
                        })
                        .collect();
                    if let Some(f) = case(&mut sim, cases, ins, exp) {
                        return CheckReport::failed(cases, f);
                    }
                }
            }
            ChallengeTest::Sequence { steps } => {
                // Every input starts at 0, from power-on.
                let mut held: Vec<(String, u64)> =
                    self.inputs.iter().map(|p| (p.name.clone(), 0)).collect();
                for step in steps {
                    cases += 1;
                    for (name, v) in &step.set {
                        sim.set(name, *v);
                        if let Some(h) = held.iter_mut().find(|(n, _)| n == name) {
                            h.1 = *v & mask(Self::width(self.inputs, name));
                        }
                    }
                    sim.run(u64::from(step.ticks));
                    if step.expect.is_empty() {
                        continue;
                    }
                    let names: Vec<String> = step.expect.iter().map(|(n, _)| n.clone()).collect();
                    let got = read(&sim, &names);
                    if got != step.expect {
                        return CheckReport::failed(
                            cases,
                            Failure::Case(Box::new(FailedCase {
                                case: cases,
                                tick: Some(sim.tick_count()),
                                inputs: Self::signals(self.inputs, held.clone()),
                                expected: Self::signals(self.outputs, step.expect.clone()),
                                got: Self::signals(self.outputs, got),
                            })),
                        );
                    }
                }
            }
        }
        CheckReport {
            cases,
            failure: None,
        }
    }
}

/// Read a test written as `{"table": {…}}`, `{"formula": {…}}` or
/// `{"sequence": {…}}` (see `docs/save-format.md`) for a circuit with
/// these ports.
pub fn load_test(spec: Value, inputs: &[Port], outputs: &[Port]) -> Result<ChallengeTest, String> {
    let spec: TestSpec = serde_json::from_value(spec).map_err(|e| e.to_string())?;
    load_spec(spec, inputs, outputs)
}

fn load_spec(spec: TestSpec, inputs: &[Port], outputs: &[Port]) -> Result<ChallengeTest, String> {
    let place = |e: String| e;
    let find = |name: &str, list: &[Port], what: &str| -> Result<(), String> {
        if list.iter().any(|p| p.name == name) {
            Ok(())
        } else {
            Err(format!("there is no {what} \"{name}\""))
        }
    };
    let test = match spec {
        TestSpec::Table {
            inputs: ins,
            outputs: outs,
            rows,
        } => {
            for n in &ins {
                find(n, inputs, "input")?;
            }
            for n in &outs {
                find(n, outputs, "output")?;
            }
            let rows = rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    if row.len() != ins.len() + outs.len() {
                        return Err(place(format!(
                            "row {} has {} values, not {}",
                            i + 1,
                            row.len(),
                            ins.len() + outs.len()
                        )));
                    }
                    row.iter()
                        .map(|v| value(v).map_err(|e| place(format!("row {}: {e}", i + 1))))
                        .collect()
                })
                .collect::<Result<Vec<Vec<u64>>, String>>()?;
            if rows.is_empty() {
                return Err(place("the table has no rows".into()));
            }
            ChallengeTest::Table {
                inputs: ins,
                outputs: outs,
                rows,
            }
        }
        TestSpec::Formula {
            expect,
            samples,
            seed,
        } => {
            let mut list = Vec::new();
            for (name, text) in &expect {
                find(name, outputs, "output")?;
                let Value::String(text) = text else {
                    return Err(place(format!("\"{name}\": write the formula as a string")));
                };
                let e =
                    Expr::parse(text).map_err(|e| place(format!("\"{name}\": \"{text}\": {e}")))?;
                for n in e.names() {
                    find(n, inputs, "input")?;
                }
                list.push((name.clone(), e));
            }
            if list.is_empty() {
                return Err(place("the formula test expects nothing".into()));
            }
            let bits: u32 = inputs.iter().map(|p| u32::from(p.w)).sum();
            if samples.is_none() && bits > MAX_EXHAUSTIVE_BITS {
                return Err(place(format!(
                    "{bits} input bits are too many to try them all; give \"samples\""
                )));
            }
            ChallengeTest::Formula {
                expect: list,
                samples,
                seed,
            }
        }
        TestSpec::Sequence { steps } => {
            let pairs = |map: &Map<String, Value>,
                         list: &[Port],
                         what: &str|
             -> Result<Vec<(String, u64)>, String> {
                map.iter()
                    .map(|(n, v)| {
                        find(n, list, what)?;
                        Ok((n.clone(), value(v).map_err(place)?))
                    })
                    .collect()
            };
            let steps = steps
                .iter()
                .map(|s| {
                    Ok(Step {
                        set: pairs(&s.set, inputs, "input")?,
                        ticks: s.ticks,
                        expect: pairs(&s.expect, outputs, "output")?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            if !steps.iter().any(|s| !s.expect.is_empty()) {
                return Err(place("the sequence expects nothing".into()));
            }
            ChallengeTest::Sequence { steps }
        }
    };
    Ok(test)
}

fn mask(w: u8) -> u64 {
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}

/// The input values a formula test runs: every combination (inputs in
/// order, the first one counting fastest), or all-zero, all-ones and
/// `samples` random ones from `seed`. Made one case at a time: a test of
/// many samples holds one of them, and stops at the first that fails.
pub fn formula_cases<'a>(
    inputs: &'a [Port],
    samples: Option<u32>,
    seed: u64,
) -> Box<dyn Iterator<Item = Vec<u64>> + 'a> {
    match samples {
        None => {
            let bits: u32 = inputs.iter().map(|p| u32::from(p.w)).sum();
            Box::new(
                (0..1u64 << bits.min(MAX_EXHAUSTIVE_BITS)).map(move |mut n| {
                    inputs
                        .iter()
                        .map(|p| {
                            let v = n & mask(p.w);
                            n >>= p.w;
                            v
                        })
                        .collect()
                }),
            )
        }
        Some(count) => {
            let mut state = seed;
            let mut next = move || {
                // SplitMix64: a fixed, dependency-free sequence.
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^ (z >> 31)
            };
            let ends = [
                vec![0; inputs.len()],
                inputs.iter().map(|p| mask(p.w)).collect(),
            ];
            Box::new(ends.into_iter().chain(
                (0..count).map(move |_| inputs.iter().map(|p| next() & mask(p.w)).collect()),
            ))
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Results
// ─────────────────────────────────────────────────────────────────────────────

/// A named value of a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub name: String,
    pub value: u64,
    pub w: u8,
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.w <= 1 || self.value < 10 {
            write!(f, "{}={}", self.name, self.value)
        } else {
            write!(f, "{}={} (0x{:X})", self.name, self.value, self.value)
        }
    }
}

/// The first case the circuit got wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCase {
    /// 1-based case (row, sample or step).
    pub case: usize,
    /// For a sequence, the tick after the step.
    pub tick: Option<u64>,
    pub inputs: Vec<Signal>,
    pub expected: Vec<Signal>,
    pub got: Vec<Signal>,
}

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The circuit does not build (a loop, a broken definition).
    Build(String),
    /// A port the test needs is missing or has the wrong width.
    Port(String),
    /// A part the rules refuse.
    Part {
        part: String,
        /// The definition it is in, when not the root.
        circuit: Option<String>,
        rule: String,
    },
    /// A case gave the wrong outputs.
    Case(Box<FailedCase>),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |s: &[Signal]| {
            s.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Failure::Build(e) => write!(f, "The circuit does not build: {e}."),
            Failure::Port(e) => write!(f, "Ports: {e}. Keep the starter's ports and labels."),
            Failure::Part {
                part,
                circuit,
                rule,
            } => {
                write!(f, "Not allowed here: {part}")?;
                if let Some(c) = circuit {
                    write!(f, " (inside \"{c}\")")?;
                }
                write!(f, ". {rule}.")
            }
            Failure::Case(c) => {
                match c.tick {
                    Some(t) => write!(f, "Step {} (tick {t}): ", c.case)?,
                    None => write!(f, "Case {}: ", c.case)?,
                }
                write!(
                    f,
                    "inputs {}; expected {}, got {}.",
                    list(&c.inputs),
                    list(&c.expected),
                    list(&c.got)
                )
            }
        }
    }
}

/// The result of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Cases run (the failing one included).
    pub cases: usize,
    pub failure: Option<Failure>,
}

impl CheckReport {
    fn failed(cases: usize, failure: Failure) -> Self {
        Self {
            cases,
            failure: Some(failure),
        }
    }

    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }

    /// One line for the HUD.
    pub fn summary(&self) -> String {
        match &self.failure {
            None => format!(
                "Passed: all {} {} right.",
                self.cases,
                if self.cases == 1 { "case" } else { "cases" }
            ),
            Some(f) => f.to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Loading
// ─────────────────────────────────────────────────────────────────────────────

/// Load the manifest's challenges into `catalog` (after its demos), with
/// `read` giving the text of a file beside the demos.
pub(super) fn load(
    catalog: &mut DemoCatalog,
    entries: Vec<ManifestChallenge>,
    read: &impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    for entry in entries {
        let file = starter_file(&entry.id);
        let place = |e: String| format!("challenge \"{}\" ({file}): {e}", entry.id);
        if catalog.challenge(&entry.id).is_some() {
            return Err(place("listed twice".into()));
        }
        if !catalog.levels.iter().any(|l| l.level == entry.level) {
            return Err(place(format!("level {} is not in \"levels\"", entry.level)));
        }
        if entry.title.trim().is_empty() || entry.brief.trim().is_empty() {
            return Err(place("needs a title and a brief".into()));
        }
        let source = source::parse(&read(&file)?).map_err(|e| place(e.to_string()))?;
        let used = source.external_uses();
        let listed: BTreeSet<String> = entry.uses.iter().cloned().collect();
        if used != listed || listed.len() != entry.uses.len() {
            return Err(place(format!(
                "\"uses\" in the manifest is {:?} but the file uses {:?}",
                entry.uses, used
            )));
        }
        for r in &used {
            let demo = r.split('/').next().unwrap_or_default();
            match catalog.demos.iter().find(|d| d.id == demo) {
                Some(d) if d.level <= entry.level => {}
                _ => {
                    return Err(place(format!(
                        "\"{r}\" is not a demo of this level or below"
                    )));
                }
            }
        }
        let expansion = source::expand(&source, &challenge_key(&entry.id), &catalog.externals())
            .map_err(|e| place(e.to_string()))?;
        let starter = expansion.workspace;

        // Ports, top to bottom as drawn.
        let ports = |input: bool| -> Result<Vec<Port>, String> {
            let mut nodes: Vec<_> = starter
                .root
                .nodes
                .iter()
                .filter(|n| match n.kind {
                    GateKind::Input { .. } => input,
                    GateKind::Output { .. } => !input,
                    _ => false,
                })
                .collect();
            nodes.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
            let ports: Vec<Port> = nodes
                .iter()
                .map(|n| Port {
                    name: n.label.clone().unwrap_or_default(),
                    w: n.kind.width().unwrap_or(1),
                })
                .collect();
            let names: BTreeSet<&str> = ports.iter().map(|p| p.name.as_str()).collect();
            if names.len() != ports.len() {
                return Err(place("two ports share a label".into()));
            }
            Ok(ports)
        };
        let (inputs, outputs) = (ports(true)?, ports(false)?);
        if inputs.is_empty() || outputs.is_empty() {
            return Err(place("the starter needs inputs and outputs".into()));
        }

        // Parts rules.
        for t in entry.allowed.iter().flatten() {
            if !PRIMITIVE_TYPES.contains(&t.as_str()) {
                return Err(place(format!(
                    "\"allowed\": \"{t}\" is not a primitive type"
                )));
            }
        }
        let mut forbidden_defs = Vec::new();
        for f in &entry.forbidden {
            if f.contains('/') {
                let id = catalog
                    .definition_id(f)
                    .ok_or_else(|| place(format!("\"forbidden\": no circuit \"{f}\"")))?;
                let name = catalog
                    .definition(id)
                    .map_or_else(|| f.clone(), |d| d.name.clone());
                forbidden_defs.push((f.clone(), id, format!("the {name} chip")));
            } else if !PRIMITIVE_TYPES.contains(&f.as_str()) {
                return Err(place(format!(
                    "\"forbidden\": \"{f}\" is neither a primitive type nor \"demo/circuit\""
                )));
            }
        }

        let test = load_spec(entry.test, &inputs, &outputs).map_err(place)?;

        catalog.challenges.push(Challenge {
            id: entry.id,
            level: entry.level,
            title: entry.title,
            brief: entry.brief,
            allowed: entry.allowed,
            forbidden: entry.forbidden,
            forbidden_defs,
            test,
            source,
            starter: Arc::new(starter),
            inputs,
            outputs,
        });
    }
    Ok(())
}
