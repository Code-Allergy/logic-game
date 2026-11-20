//! Criterion benchmarks for simulation v2 (design doc section 9). Every
//! throughput benchmark changes the root inputs every tick or runs real
//! sequential logic. Throughput is reported in instructions (elements), so
//! criterion's elem/s is instructions per second.
//!
//! For the one-shot report with the acceptance gates, run
//! `cargo run -p logi-sim --release --bin sim_v2_report`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::circuit::BitPackedCircuit;
use logi_sim::compile::{Options, Program, compile};
use logi_sim::netlist::{Design, mask};
use logi_sim::prelude::{Runner, SimdRunner};
use logi_sim::sim::Sim;
use logi_sim::testkit::{GenConfig, bitblast::bitblast, fixtures, generate, legacy::to_legacy};

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
    bench_sim(c, "sim_v2/dag32", cases);
}

fn gates_vs_legacy(c: &mut Criterion) {
    let sizes = [10_000, 100_000, 1_000_000];
    let designs: Vec<_> = sizes
        .iter()
        .map(|&n| (n, generate(&GenConfig::gates(2, n, 1)).design))
        .collect();
    bench_sim(
        c,
        "sim_v2/gates1",
        designs
            .iter()
            .map(|(n, d)| (format!("{n}"), d.clone()))
            .collect(),
    );

    let mut g = c.benchmark_group("legacy_simd/gates1");
    g.sample_size(20).measurement_time(Duration::from_secs(4));
    for (n, d) in &designs {
        let legacy = to_legacy(d);
        let mut simd = SimdRunner::new(BitPackedCircuit::compile(&legacy.circuit));
        let mut lfsr = 0x1234_5678_9ABC_DEF1u64;
        g.throughput(Throughput::Elements(*n as u64));
        g.bench_function(BenchmarkId::from_parameter(n), |b| {
            b.iter(|| {
                for &node in &legacy.inputs {
                    lfsr ^= lfsr << 13;
                    lfsr ^= lfsr >> 7;
                    lfsr ^= lfsr << 17;
                    simd.set_input(node, 0, lfsr & 1 == 1);
                }
                simd.tick();
            });
            black_box(simd.output(legacy.outputs[0], 0));
        });
    }
    g.finish();
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
        "sim_v2/sequential",
        vec![
            ("cpu".into(), cpu.clone()),
            ("cpu_bitblasted".into(), bitblast(&cpu)),
            ("datapath32".into(), dp.clone()),
            ("datapath32_bitblasted".into(), bitblast(&dp)),
        ],
    );
}

fn compile_time(c: &mut Criterion) {
    let mut g = c.benchmark_group("sim_v2/compile");
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

criterion_group!(
    benches,
    dags,
    gates_vs_legacy,
    cpu_and_datapath,
    compile_time
);
criterion_main!(benches);
