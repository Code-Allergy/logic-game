//! Engine overhead (`logi_sim::engine`): what the engine adds on top of
//! `Sim::tick`. Every case runs the same fixture, the 8-bit accumulator
//! CPU (`testkit::fixtures::cpu`), compiled with the editor's options
//! (`Options::debug`), so the numbers compare with `sim/sequential/cpu`.
//!
//! - `engine/batch`: 1000 ticks through `EngineCore::step` + `run` (chunked,
//!   with clock reads) against `Sim::run(1000)`.
//! - `engine/publish`: one probe frame with 0, 16 and every net probed.
//! - `engine/mem_probe`: a frame that copies the CPU's RAM (changed every
//!   time) against one where it did not change.
//! - `engine/reset` and `engine/install` (an edit: compile excluded, state
//!   carried over).
//! - `engine/threaded`: a round trip to the worker thread (`Engine::step`
//!   then `sync`), native only.
//!
//! Part of `cargo xtask perf bench` (see `docs/performance.md`).

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use logi_sim::compile::{Options, Program, compile};
use logi_sim::engine::{Engine, EngineCore, MemProbe, Probe, ProbeFrame};
use logi_sim::netlist::{Design, NodeId, NodeKind};
use logi_sim::sim::Sim;
use logi_sim::testkit::fixtures;

const TICKS: u64 = 1000;

fn cpu() -> (Design, Arc<Program>) {
    let d = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let p = Arc::new(compile(&d, &Options::debug()).unwrap());
    (d, p)
}

fn ram_node(d: &Design) -> NodeId {
    d.root_def()
        .nodes
        .iter()
        .find(|(_, n)| matches!(n.kind, NodeKind::Ram { .. }))
        .expect("the CPU has a RAM")
        .0
}

fn all_probes(d: &Design) -> Vec<Probe> {
    d.root_def()
        .nets
        .keys()
        .map(|net| Probe { path: vec![], net })
        .collect()
}

fn core(p: &Arc<Program>) -> EngineCore {
    let mut core = EngineCore::new(Options::debug());
    core.install(Arc::clone(p), 1);
    core
}

fn batch(c: &mut Criterion) {
    let (_, p) = cpu();
    let mut g = c.benchmark_group("engine/batch");
    g.sample_size(30).measurement_time(Duration::from_secs(3));
    let mut sim = Sim::new(Arc::clone(&p));
    g.bench_function("sim_run_1000", |b| b.iter(|| sim.run(TICKS)));
    let mut core = core(&p);
    let origin = Instant::now();
    let clock = move || origin.elapsed();
    g.bench_function("core_step_1000", |b| {
        b.iter(|| {
            core.step(TICKS);
            black_box(core.run(Duration::from_secs(1), &clock))
        })
    });
    g.finish();
}

fn publish(c: &mut Criterion) {
    let (d, p) = cpu();
    let probes = all_probes(&d);
    let mut g = c.benchmark_group("engine/publish");
    g.sample_size(30).measurement_time(Duration::from_secs(2));
    for (name, n) in [("0", 0), ("16", 16), ("all", probes.len())] {
        let mut core = core(&p);
        core.set_probes(probes.iter().take(n).cloned().collect());
        let mut frame = ProbeFrame::default();
        g.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                core.publish(&mut frame);
                black_box(frame.values.len())
            })
        });
    }
    g.finish();
}

fn mem_probe(c: &mut Criterion) {
    let (d, p) = cpu();
    let ram = ram_node(&d);
    let mut g = c.benchmark_group("engine/mem_probe");
    g.sample_size(30).measurement_time(Duration::from_secs(2));
    let mut core = core(&p);
    core.set_mem_probes(vec![MemProbe {
        path: vec![],
        node: ram,
    }]);
    let mut frame = ProbeFrame::default();
    let origin = Instant::now();
    let clock = move || origin.elapsed();
    // The loop program stores to RAM every few ticks; 16 ticks always
    // write it, so every frame copies the memory.
    g.bench_function("changed", |b| {
        b.iter(|| {
            core.step(16);
            core.run(Duration::from_secs(1), &clock);
            core.request_mems();
            core.publish(&mut frame);
        })
    });
    g.bench_function("unchanged", |b| {
        b.iter(|| {
            core.request_mems();
            core.publish(&mut frame);
        })
    });
    g.finish();
}

fn reset_and_install(c: &mut Criterion) {
    let (d, p) = cpu();
    let mut g = c.benchmark_group("engine");
    g.sample_size(30).measurement_time(Duration::from_secs(2));
    let mut core = core(&p);
    core.set_probes(all_probes(&d));
    g.bench_function("reset", |b| b.iter(|| core.reset()));
    g.bench_function("install", |b| {
        b.iter(|| black_box(core.install(Arc::clone(&p), 2)))
    });
    g.finish();
}

fn threaded(c: &mut Criterion) {
    let (d, _) = cpu();
    let mut g = c.benchmark_group("engine/threaded");
    g.sample_size(30).measurement_time(Duration::from_secs(3));
    let engine = Engine::spawn(Options::debug());
    engine.load(Arc::new(d.clone()), 1);
    engine
        .wait_event(Duration::from_secs(30))
        .expect("the design loads");
    engine.set_probes(all_probes(&d));
    engine.sync();
    g.bench_function("step_1_sync", |b| {
        b.iter(|| {
            engine.step(1);
            engine.sync();
        })
    });
    g.bench_function("step_1000_sync", |b| {
        b.iter(|| {
            engine.step(TICKS);
            engine.sync();
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    batch,
    publish,
    mem_probe,
    reset_and_install,
    threaded
);
criterion_main!(benches);
