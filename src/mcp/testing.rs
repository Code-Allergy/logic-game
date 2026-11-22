//! Stored tests: a chip carries its tests in the workspace, and they run
//! as a suite (`docs/mcp-server.md`, "`test` and stored tests").
//!
//! A test is a JSON object with a `name` and one of
//!
//! - `table`, `formula`, `sequence`: as a challenge's test
//!   ([`crate::sim::challenges`]), on the chip's own ports;
//! - `equivalent`: `{"to": chip, "samples": n, "seed": s, "hold": k}`:
//!   the same random inputs into this chip and the other, every output
//!   compared after every tick, both from power-on;
//! - `script`: an experiment script ([`super::run`]) whose `expect`,
//!   `until` and `compare` steps must hold.
//!
//! Each test runs on a fresh simulation of its chip compiled alone, so
//! tests never see each other's state and can run in parallel.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use logi_sim::sim::Sim;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::batch::{Place, circuit_at, find_circuit};
use super::rig::Rig;
use super::run::{self, RunState};
use crate::sim::challenges::{self, Port, PortTest};
use crate::sim::{CircuitDocument, GateKind, WorkspaceDocument};

/// Random cases of an `equivalent` test unless it says otherwise.
pub const DEFAULT_SAMPLES: u64 = 1000;
/// The most cases an `equivalent` test may ask for.
pub const MAX_SAMPLES: u64 = 10_000_000;

const KINDS: [&str; 5] = ["table", "formula", "sequence", "equivalent", "script"];

/// The name of a test and its one kind with its body.
fn parts(test: &Value) -> Result<(&str, &str, &Value), String> {
    let Value::Object(map) = test else {
        return Err("a test is an object: {\"name\": ..., \"table\": {...}}".into());
    };
    let Some(Value::String(name)) = map.get("name") else {
        return Err("a test needs a \"name\"".into());
    };
    if name.trim().is_empty() {
        return Err("a test needs a \"name\"".into());
    }
    let mut kinds = map.iter().filter(|(k, _)| *k != "name");
    match (kinds.next(), kinds.next()) {
        (Some((kind, body)), None) if KINDS.contains(&kind.as_str()) => Ok((name, kind, body)),
        (Some((kind, _)), None) => Err(format!(
            "test \"{name}\": unknown kind \"{kind}\" (kinds: {})",
            KINDS.join(", ")
        )),
        _ => Err(format!(
            "test \"{name}\": a test has a \"name\" and one of {}",
            KINDS.join(", ")
        )),
    }
}

/// The name of a test, for listings (`?` when it has none).
pub fn name_of(test: &Value) -> &str {
    test.get("name").and_then(Value::as_str).unwrap_or("?")
}

fn ports(doc: &CircuitDocument) -> (Vec<Port>, Vec<Port>) {
    let (ins, outs) = doc.interface();
    let port = |n: &&crate::sim::DocNode| Port {
        name: n.label.clone().unwrap_or_default(),
        w: n.kind.width().unwrap_or(1),
    };
    (
        ins.iter().map(port).collect(),
        outs.iter().map(port).collect(),
    )
}

/// The workspace with the circuit at `place` as its root.
fn rooted(ws: &WorkspaceDocument, place: Place) -> WorkspaceDocument {
    let mut ws = ws.clone();
    if let Place::Definition(_) = place {
        ws.root = circuit_at(&ws, place).clone();
    }
    ws
}

fn fresh_sim(ws: &WorkspaceDocument) -> Result<Sim, String> {
    let design = crate::sim::netlist::design_of(ws)?;
    let program = logi_sim::compile::compile(&design, &logi_sim::compile::Options::default())
        .map_err(|errors| {
            let loops = errors
                .iter()
                .any(|e| matches!(e, logi_sim::compile::CompileError::CombinationalLoop { .. }));
            if loops {
                "it does not build: a loop of gates with no state in it".to_string()
            } else {
                format!(
                    "it does not build: {}",
                    errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            }
        })?;
    Sim::try_new(Arc::new(program)).map_err(|e| e.0)
}

struct Equivalent {
    to: Place,
    to_name: String,
    samples: u64,
    seed: u64,
    hold: u64,
}

fn equivalent(ws: &WorkspaceDocument, body: &Value) -> Result<Equivalent, String> {
    let Value::Object(body) = body else {
        return Err("\"equivalent\" takes {\"to\": chip, ...}".into());
    };
    let number = |key: &str, default: u64| match body.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .ok_or_else(|| format!("\"{key}\" must be a whole number")),
    };
    if let Some(k) = body
        .keys()
        .find(|k| !matches!(k.as_str(), "to" | "samples" | "seed" | "hold"))
    {
        return Err(format!(
            "unknown field \"{k}\" (equivalent has to, samples, seed, hold)"
        ));
    }
    let Some(Value::String(to)) = body.get("to") else {
        return Err("\"equivalent\" needs \"to\": the chip to compare with".into());
    };
    let place = find_circuit(ws, Some(to))?.ok_or_else(|| format!("no chip \"{to}\""))?;
    let samples = number("samples", DEFAULT_SAMPLES)?;
    if !(1..=MAX_SAMPLES).contains(&samples) {
        return Err(format!("\"samples\" must be 1 to {MAX_SAMPLES}"));
    }
    Ok(Equivalent {
        to: place,
        to_name: to.clone(),
        samples,
        seed: number("seed", 1)?,
        hold: number("hold", 1)?.max(1),
    })
}

/// Check that `test` can run on the circuit at `place`: its shape, and
/// the ports and chips it names. Nothing is simulated.
pub fn validate(ws: &WorkspaceDocument, place: Place, test: &Value) -> Result<(), String> {
    let (name, kind, body) = parts(test)?;
    let doc = circuit_at(ws, place);
    let (inputs, outputs) = ports(doc);
    let check = || -> Result<(), String> {
        match kind {
            "equivalent" => {
                let e = equivalent(ws, body)?;
                let other = ports(circuit_at(ws, e.to));
                if (inputs.clone(), outputs.clone()) != other {
                    let list = |p: &[Port]| {
                        p.iter()
                            .map(|p| format!("{}[{}]", p.name, p.w))
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    return Err(format!(
                        "the ports differ: this chip has in {} | out {}; \"{}\" has in {} | \
                         out {}",
                        list(&inputs),
                        list(&outputs),
                        e.to_name,
                        list(&other.0),
                        list(&other.1)
                    ));
                }
                Ok(())
            }
            "script" => match body {
                Value::Object(script) if script.get("steps").is_some_and(Value::is_array) => Ok(()),
                _ => Err("\"script\" takes {\"steps\": [...], ...}".into()),
            },
            _ => {
                let mut spec = Map::new();
                spec.insert(kind.to_string(), body.clone());
                challenges::load_test(Value::Object(spec), &inputs, &outputs).map(|_| ())
            }
        }
    };
    check().map_err(|e| format!("test \"{name}\": {e}"))
}

/// SplitMix64: a fixed, dependency-free sequence.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn mask(w: u8) -> u64 {
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}

/// Run one test on the circuit at `place`: the cases it ran, or why it
/// failed.
pub fn run_test(ws: &WorkspaceDocument, place: Place, test: &Value) -> Result<u64, String> {
    let (_, kind, body) = parts(test)?;
    let rooted_ws = rooted(ws, place);
    let (inputs, outputs) = ports(&rooted_ws.root);
    match kind {
        "equivalent" => {
            let e = equivalent(ws, body)?;
            let mut a = fresh_sim(&rooted_ws)?;
            let mut b =
                fresh_sim(&rooted(ws, e.to)).map_err(|err| format!("\"{}\": {err}", e.to_name))?;
            let port = |sim: &Sim, name: &str, input: bool| {
                let ports = &sim.program().ports;
                if input {
                    ports.input(name)
                } else {
                    ports.output(name)
                }
                .ok_or_else(|| format!("the ports differ: no port \"{name}\" on both chips"))
            };
            let mut ins = Vec::new();
            for p in &inputs {
                ins.push((p, port(&a, &p.name, true)?, port(&b, &p.name, true)?));
            }
            let mut outs = Vec::new();
            for p in &outputs {
                outs.push((p, port(&a, &p.name, false)?, port(&b, &p.name, false)?));
            }
            let mut state = e.seed;
            for case in 1..=e.samples {
                let values: Vec<u64> = ins
                    .iter()
                    .map(|(p, ..)| next(&mut state) & mask(p.w))
                    .collect();
                for ((_, pa, pb), v) in ins.iter().zip(&values) {
                    a.set_input(*pa, *v);
                    b.set_input(*pb, *v);
                }
                for _ in 0..e.hold {
                    a.tick();
                    b.tick();
                    let wrong: Vec<String> = outs
                        .iter()
                        .filter(|(_, pa, pb)| a.output(*pa) != b.output(*pb))
                        .map(|(p, pa, pb)| {
                            format!(
                                "{}: this chip gives {}, \"{}\" gives {}",
                                p.name,
                                a.output(*pa),
                                e.to_name,
                                b.output(*pb)
                            )
                        })
                        .collect();
                    if !wrong.is_empty() {
                        let given: Vec<String> = ins
                            .iter()
                            .zip(&values)
                            .map(|((p, ..), v)| format!("{}={v}", p.name))
                            .collect();
                        return Err(format!(
                            "case {case} (tick {}): inputs {}; {}",
                            a.tick_count(),
                            given.join(", "),
                            wrong.join("; ")
                        ));
                    }
                }
            }
            Ok(e.samples)
        }
        "script" => {
            let Value::Object(script) = body else {
                return Err("\"script\" takes {\"steps\": [...], ...}".into());
            };
            let mut state = RunState::new(Rig::of(rooted_ws)?);
            let outcome = run::run(&mut state, script)?;
            if outcome.failed {
                // The last lines say what failed.
                let text = outcome.text.trim_end();
                let from = text
                    .find("first difference")
                    .or_else(|| text.rfind("FAILED"));
                Err(text[from.unwrap_or(0)..].to_string())
            } else {
                Ok(script
                    .get("steps")
                    .and_then(Value::as_array)
                    .map_or(0, |s| s.len() as u64))
            }
        }
        _ => {
            let mut spec = Map::new();
            spec.insert(kind.to_string(), body.clone());
            let loaded = challenges::load_test(Value::Object(spec), &inputs, &outputs)?;
            let report = PortTest {
                inputs: &inputs,
                outputs: &outputs,
                test: &loaded,
            }
            .run(fresh_sim(&rooted_ws)?);
            match report.failure {
                None => Ok(report.cases as u64),
                Some(failure) => Err(failure.to_string()),
            }
        }
    }
}

/// One test's result.
#[derive(Debug, Clone, PartialEq)]
pub struct TestResult {
    pub circuit: String,
    pub name: String,
    /// Cases run, or the failure.
    pub outcome: Result<u64, String>,
}

/// What a suite did.
#[derive(Debug, Clone, PartialEq)]
pub struct SuiteReport {
    pub results: Vec<TestResult>,
    /// Circuits of the suite that have no tests.
    pub untested: Vec<String>,
}

impl SuiteReport {
    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_err()).count()
    }

    /// Failures in full, then one line of counts.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for r in &self.results {
            if let Err(e) = &r.outcome {
                let _ = writeln!(out, "FAILED {} / {}: {}", r.circuit, r.name, e.trim_end());
            }
        }
        let chips: HashSet<&str> = self.results.iter().map(|r| r.circuit.as_str()).collect();
        let cases: u64 = self
            .results
            .iter()
            .filter_map(|r| r.outcome.as_ref().ok())
            .sum();
        let _ = writeln!(
            out,
            "{} of {} tests passed in {} circuit(s) ({cases} cases)",
            self.results.len() - self.failed(),
            self.results.len(),
            chips.len()
        );
        if !self.untested.is_empty() {
            let _ = writeln!(out, "no tests: {}", self.untested.join(", "));
        }
        out
    }
}

/// Run the stored tests of the circuit named `circuit`, or with `None`
/// of the root and every chip it reaches; with `name`, only the tests of
/// that name.
pub fn run_suite(
    ws: &WorkspaceDocument,
    circuit: Option<&str>,
    name: Option<&str>,
) -> Result<SuiteReport, String> {
    let places: Vec<Place> = match circuit {
        Some(c) => vec![find_circuit(ws, Some(c))?.ok_or_else(|| format!("no circuit \"{c}\""))?],
        None => {
            // The root, then the chips it reaches, by name.
            let mut seen: HashSet<Uuid> = HashSet::new();
            let mut stack: Vec<Uuid> = ws.root.instanced_definitions().collect();
            while let Some(id) = stack.pop() {
                if seen.insert(id)
                    && let Some(def) = ws.definitions.get(&id)
                {
                    stack.extend(def.nodes.iter().filter_map(|n| match n.kind {
                        GateKind::SubCircuit { definition } => Some(definition),
                        _ => None,
                    }));
                }
            }
            // Chips not placed anywhere yet are part of the work too.
            seen.extend(ws.definitions.keys().copied());
            let mut chips: Vec<&Arc<CircuitDocument>> = seen
                .iter()
                .filter_map(|id| ws.definitions.get(id))
                .collect();
            chips.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
            std::iter::once(Place::Root)
                .chain(chips.into_iter().map(|d| Place::Definition(d.id)))
                .collect()
        }
    };
    let label = |place: Place| match place {
        Place::Root => "main".to_string(),
        Place::Definition(_) => circuit_at(ws, place).name.clone(),
    };
    let mut jobs: Vec<(Place, &Value)> = Vec::new();
    let mut untested = Vec::new();
    for &place in &places {
        let tests = ws.tests.get(&circuit_at(ws, place).id);
        let tests: Vec<&Value> = tests
            .into_iter()
            .flatten()
            .filter(|t| name.is_none_or(|n| name_of(t) == n))
            .collect();
        if tests.is_empty() && !circuit_at(ws, place).nodes.is_empty() {
            untested.push(label(place));
        }
        jobs.extend(tests.into_iter().map(|t| (place, t)));
    }
    if jobs.is_empty() && (circuit.is_some() || name.is_some()) {
        return Err(match name {
            Some(n) => format!("no test named \"{n}\""),
            None => format!(
                "\"{}\" has no tests; add them with edit: {{\"tests\": [...]}}",
                circuit.unwrap_or_default()
            ),
        });
    }

    // Each test is on its own simulation, so they run side by side.
    let results: Mutex<Vec<Option<Result<u64, String>>>> = Mutex::new(vec![None; jobs.len()]);
    let next_job = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(jobs.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next_job.fetch_add(1, Ordering::Relaxed);
                    let Some((place, test)) = jobs.get(i) else {
                        return;
                    };
                    let outcome = run_test(ws, *place, test);
                    results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(outcome);
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    Ok(SuiteReport {
        results: jobs
            .iter()
            .zip(results)
            .map(|((place, test), outcome)| TestResult {
                circuit: label(*place),
                name: name_of(test).to_string(),
                outcome: outcome.unwrap_or_else(|| Err("the test did not run".into())),
            })
            .collect(),
        untested,
    })
}
