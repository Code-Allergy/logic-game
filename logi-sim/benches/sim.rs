//! Criterion benchmarks for the simulator (design doc section 9). Every
//! throughput benchmark changes the root inputs every tick or runs real
//! sequential logic. Throughput is reported in instructions (elements), so
//! criterion's elem/s is instructions per second.
//!
//! For the one-shot report with the acceptance gates, run
//! `cargo run -p logi-sim --release --bin sim_report`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::compile::{Options, Program, compile};
use logi_sim::netlist::{Design, mask};
use logi_sim::sim::Sim;
use logi_sim::testkit::{GenConfig, bitblast::bitblast, fixtures, generate};

fn program(d: &Design) -> Arc<Program> {
    Arc::new(compile(d, &Options::unoptimized()).unwrap())
}

/// One tick with every input randomized.
fn tick_with_inputs(sim: &mut Sim, widths: &[u8], lfsr: &mut u64) {
    for (port, &w) in widths.iter().enumerate() {
        *lfsr ^= *lfsr << 13;
        *lfsr ^= *lfsr >> 7;
        *lfsr ^= *lfsr << 17;
        sim.set_input(port, *lfsr & mask(w));
    }
    sim.tick();
}

fn bench_sim(c: &mut Criterion, group: &str, cases: Vec<(String, Design)>) {
    let mut g = c.benchmark_group(group);
    g.sample_size(20).measurement_time(Duration::from_secs(4));
    for (name, d) in cases {
        let p = program(&d);
        let widths: Vec<u8> = p.ports.inputs.iter().map(|x| x.width).collect();
        g.throughput(Throughput::Elements(p.instrs.len() as u64));
        let mut sim = Sim::new(Arc::clone(&p));
        let mut lfsr = 0x9E37_79B9_7F4A_7C15u64;
        g.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| tick_with_inputs(&mut sim, &widths, &mut lfsr));
            black_box(sim.output(0));
        });
    }
    g.finish();
}

fn dags(c: &mut Criterion) {
    let cases = [10_000, 100_000, 1_000_000]
        .into_iter()
        .map(|n| (format!("{n}"), generate(&GenConfig::dag(1, n, 32)).design))
        .collect();
    bench_sim(c, "sim/dag32", cases);
}

fn gates(c: &mut Criterion) {
    let cases = [10_000, 100_000, 1_000_000]
        .into_iter()
        .map(|n| (format!("{n}"), generate(&GenConfig::gates(2, n, 1)).design))
        .collect();
    bench_sim(c, "sim/gates1", cases);
}

fn cpu_and_datapath(c: &mut Criterion) {
    let cpu = fixtures::cpu(&fixtures::LOOP_PROGRAM);
    let dp = generate(&GenConfig {
        seed: 3,
        nodes: 20_000,
        child_defs: 0,
        max_depth: 1,
        max_width: 32,
        state_percent: 20,
        unconnected_percent: 0,
        inputs: 16,
        outputs: 16,
        ..Default::default()
    })
    .design;
    bench_sim(
        c,
        "sim/sequential",
        vec![
            ("cpu".into(), cpu.clone()),
            ("cpu_bitblasted".into(), bitblast(&cpu)),
            ("datapath32".into(), dp.clone()),
            ("datapath32_bitblasted".into(), bitblast(&dp)),
        ],
    );
}

fn compile_time(c: &mut Criterion) {
    let mut g = c.benchmark_group("sim/compile");
    g.sample_size(10).measurement_time(Duration::from_secs(10));
    for n in [100_000, 1_000_000] {
        let d = generate(&GenConfig::gates(4, n, 1)).design;
        g.throughput(Throughput::Elements(n as u64));
        g.bench_function(BenchmarkId::new("gates1", n), |b| {
            b.iter(|| black_box(compile(&d, &Options::default()).unwrap()))
        });
    }
    g.finish();
}

criterion_group!(benches, dags, gates, cpu_and_datapath, compile_time);
criterion_main!(benches);
