//! App-side costs, headless (no window, no renderer). Groups:
//!
//! - `doc/parse_v6`, `doc/expand_v6`: a gallery file's version 6 text to
//!   its `SourceFile`, and that to documents (`src/sim/source.rs`).
//! - `doc/write_v5`, `doc/read_v5`: the expanded workspace to and from
//!   the version 5 save (`src/editor/workspace.rs`; the read includes
//!   reading the file).
//! - `design_of`: a workspace to a `logi_sim` design
//!   (`src/sim/netlist.rs`).
//! - `routing/full`: every wire of the root circuit routed from scratch;
//!   `routing/drag`: one well-connected part moved two grid cells, routed
//!   incrementally as during a drag (kept paths, `strict` off;
//!   `src/sim/routing.rs`).
//! - `bridge/edit`: one edit that changes the design (a part's kind) in
//!   the open `cpu16`, through the ECS, the bridge and an inline compile
//!   (threadless runtime), plus the undo snapshot it records.
//! - `history/undo_redo`: undoing and redoing one step of the open
//!   `cpu16` (snapshot restore, both ways; `src/sim/history.rs`).
//! - `challenges/check`: the Check of a challenge on its reference
//!   solution (`src/sim/challenges.rs`).
//! - `chip/plan`: Make chip of every non-port part of a root circuit
//!   (`plan_chip`, `src/sim/chip.rs`).
//!
//! - `routing/rubber_band`: the rubber band while drawing a wire
//!   (`src/sim/wiring.rs`).
//!
//! Not yet here: the circuit statistics of issue #107; see
//! `docs/performance.md`.

use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::time::Duration;

use bevy::prelude::*;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use gate_quest::editor::workspace::{read_workspace_in, save_workspace_in, workspace_to_json};
use gate_quest::sim::netlist::design_of;
use gate_quest::sim::routing::{self, GRID, Keep, SinkPath};
use gate_quest::sim::{
    CircuitLibrary, CreateGate, GateKind, HistoryCommand, SetGateKind, SimRuntime,
    WorkspaceDocument, plan_chip,
};
use gate_quest::sim::{source, wiring};

fn ids() -> Vec<&'static str> {
    if perf::quick() {
        perf::CPUS.to_vec()
    } else {
        perf::LARGE.to_vec()
    }
}

fn library(ws: &WorkspaceDocument) -> CircuitLibrary {
    CircuitLibrary {
        definitions: ws.definitions.clone(),
    }
}

fn group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(20).measurement_time(Duration::from_secs(3));
    g
}

fn documents(c: &mut Criterion) {
    let externals = perf::catalog().externals();
    let dir = std::env::temp_dir().join(format!("gatequest-perf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut g = group(c, "doc/parse_v6");
        for id in ids() {
            let text = perf::gallery_text(id);
            g.bench_function(id, |b| b.iter(|| black_box(source::parse(text).unwrap())));
        }
    }
    {
        let mut g = group(c, "doc/expand_v6");
        for id in ids() {
            let file = source::parse(perf::gallery_text(id)).unwrap();
            let key = gate_quest::sim::demos::gallery_key(id);
            g.bench_function(id, |b| {
                b.iter(|| black_box(source::expand(&file, &key, &externals).unwrap()))
            });
        }
    }
    {
        let mut g = group(c, "doc/write_v5");
        for id in ids() {
            let ws = perf::workspace(id);
            g.bench_function(id, |b| {
                b.iter(|| black_box(workspace_to_json(id, &ws).unwrap()))
            });
        }
    }
    {
        let mut g = group(c, "doc/read_v5");
        for id in ids() {
            save_workspace_in(&dir, id, &perf::workspace(id)).unwrap();
            g.bench_function(id, |b| {
                b.iter(|| black_box(read_workspace_in(&dir, id).unwrap()))
            });
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let mut g = group(c, "design_of");
    for id in ids() {
        let ws = perf::workspace(id);
        g.bench_function(id, |b| b.iter(|| black_box(design_of(&ws).unwrap())));
    }
}

/// The root node index with the most wire ends (the part a drag moves).
fn busiest(nets: &routing::DocumentNets, ws: &WorkspaceDocument) -> usize {
    let mut ends: HashMap<usize, usize> = HashMap::new();
    for ((d, _), sinks) in &nets.nets {
        *ends.entry(*d).or_default() += sinks.len();
        for (s, _, _) in sinks {
            *ends.entry(*s).or_default() += 1;
        }
    }
    ends.into_iter()
        .filter(|(n, _)| !ws.root.nodes[*n].kind.is_port())
        .max_by_key(|&(n, k)| (k, std::cmp::Reverse(n)))
        .map(|(n, _)| n)
        .expect("a connected part")
}

fn route(c: &mut Criterion) {
    let cases: Vec<(&str, WorkspaceDocument)> = ids()
        .into_iter()
        .map(|id| (id, perf::workspace(id)))
        .collect();
    {
        let mut g = group(c, "routing/full");
        g.sample_size(10);
        for (id, ws) in &cases {
            let nets = routing::document_nets(&ws.root, &library(ws));
            g.bench_function(*id, |b| {
                b.iter(|| black_box(routing::route_all(&nets.problem)))
            });
        }
    }
    let mut g = group(c, "routing/drag");
    for (id, ws) in &cases {
        let nets = routing::document_nets(&ws.root, &library(ws));
        let before = routing::route_all(&nets.problem);
        let moved = busiest(&nets, ws);
        // The problem with node `moved` shifted by `dx`, and the paths a
        // drag keeps: every sink whose pins did not move.
        let shifted = |dx: f32| {
            let mut p = nets.problem.clone();
            let d = Vec2::new(dx, 0.0);
            p.boxes[moved].min += d;
            p.boxes[moved].max += d;
            let mut keep: Vec<Vec<Option<SinkPath>>> = Vec::new();
            for (i, ((drv, _), sinks)) in nets.nets.iter().enumerate() {
                if *drv == moved {
                    p.nets[i].driver += d;
                    keep.push(Vec::new());
                    continue;
                }
                keep.push(
                    sinks
                        .iter()
                        .enumerate()
                        .map(|(j, (s, _, _))| {
                            if *s == moved {
                                p.nets[i].sinks[j] += d;
                                None
                            } else {
                                Some(before.nets[i].sinks[j].clone())
                            }
                        })
                        .collect(),
                );
            }
            (p, keep)
        };
        let (problem, keep) = shifted(2.0 * GRID);
        let keep: &Keep = &keep;
        g.bench_function(*id, |b| {
            b.iter(|| black_box(routing::route(&problem, keep, false)))
        });
    }
}

/// The rubber band while drawing a wire (`wiring::band_route`): from the
/// driver pin of the first net to a sink pin of a net halfway down the
/// list, around the tiles near them.
fn rubber_band(c: &mut Criterion) {
    let mut g = group(c, "routing/rubber_band");
    for id in ids() {
        let ws = perf::workspace(id);
        let nets = routing::document_nets(&ws.root, &library(&ws));
        let p = &nets.problem;
        let driver = p.nets[0].driver;
        let sink = p.nets[p.nets.len() / 2].sinks[0];
        g.bench_function(id, |b| {
            b.iter(|| black_box(wiring::band_route(&p.boxes, driver, sink)))
        });
    }
}

/// `cpu16` open in a headless app (threadless runtime: compiles inline),
/// with an extra unconnected AND gate whose kind the edits toggle.
fn cpu16_app() -> (App, Entity) {
    let mut app = perf::headless_app(SimRuntime::Threadless);
    perf::load(&mut app, perf::workspace("cpu16"));
    perf::send(
        &mut app,
        CreateGate {
            kind: GateKind::And { w: 1 },
            position: Vec2::new(-2000.0, -2000.0),
            label: Some("perf_probe".into()),
        },
    );
    let mut q = app
        .world_mut()
        .query::<(Entity, &gate_quest::sim::GateLabel)>();
    let gate = q
        .iter(app.world())
        .find(|(_, l)| l.value == "perf_probe")
        .map(|(e, _)| e)
        .expect("the probe gate");
    (app, gate)
}

fn edits(c: &mut Criterion) {
    let mut g = group(c, "bridge");
    g.sample_size(10);
    let (mut app, gate) = cpu16_app();
    let mut or = false;
    g.bench_function("edit/cpu16", |b| {
        b.iter(|| {
            or = !or;
            let kind = if or {
                GateKind::Or { w: 1 }
            } else {
                GateKind::And { w: 1 }
            };
            perf::send(&mut app, SetGateKind { entity: gate, kind });
        })
    });
    g.finish();
    let mut g = group(c, "history");
    g.sample_size(10);
    g.bench_function("undo_redo/cpu16", |b| {
        b.iter(|| {
            perf::send(&mut app, HistoryCommand::Undo);
            perf::send(&mut app, HistoryCommand::Redo);
        })
    });
}

/// The adder8 challenge's reference solution (as in
/// `src/sim/tests/challenges.rs`): eight full adders, carries chained.
const ADDER8: &str = r#"{"format": "gatequest-workspace", "version": 6, "root": "main",
  "circuits": [{"name": "main", "nodes": [
    {"name": "A", "type": "Input", "w": 8, "at": [0, 80]},
    {"name": "B", "type": "Input", "w": 8, "at": [0, 0]},
    {"name": "Cin", "type": "Input", "at": [0, -80]},
    {"name": "fa", "use": "full_adder/full_adder", "at": [320, 280], "count": 8, "step": [0, -80]},
    {"name": "Sum", "type": "Output", "w": 8, "at": [640, 40]},
    {"name": "Cout", "type": "Output", "at": [640, -40]}],
  "wires": [["A", "fa.A"], ["B", "fa.B"], ["Cin", "fa[0].Cin"],
    ["fa[0..7].Cout", "fa[1..8].Cin"], ["fa.Sum", "Sum"], ["fa[7].Cout", "Cout"]]}]}"#;

fn challenges(c: &mut Criterion) {
    let file = source::parse(ADDER8).unwrap();
    let ws = source::expand(&file, "perf/adder8", &perf::catalog().externals())
        .unwrap()
        .workspace;
    let challenge = perf::catalog().challenge("adder8").expect("adder8");
    assert!(challenge.check(&ws).passed(), "the solution passes");
    let mut g = group(c, "challenges/check");
    g.bench_function("adder8", |b| b.iter(|| black_box(challenge.check(&ws))));
}

fn chips(c: &mut Criterion) {
    let mut g = group(c, "chip/plan");
    for id in ids() {
        let ws = perf::workspace(id);
        let lib = library(&ws);
        let selection: HashSet<_> = ws
            .root
            .nodes
            .iter()
            .filter(|n| !n.kind.is_port())
            .map(|n| n.id)
            .collect();
        g.bench_function(BenchmarkId::from_parameter(id), |b| {
            b.iter(|| black_box(plan_chip(&ws.root, &selection, "Chip", &lib).unwrap()))
        });
    }
}

criterion_group!(
    benches,
    documents,
    route,
    rubber_band,
    edits,
    challenges,
    chips
);
criterion_main!(benches);
