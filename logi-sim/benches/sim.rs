//! Criterion benchmarks for the simulator (design doc section 9). Every
//! throughput benchmark changes the root inputs every tick or runs real
//! sequential logic. Throughput is reported in instructions (elements), so
//! criterion's elem/s is instructions per second.
//!
//! For the one-shot report with the acceptance gates, run
//! `cargo run -p logi-sim --release --bin sim_report`.
//!
//! Part of the performance framework (`docs/performance.md`): run every
//! suite with `cargo xtask perf bench`. With `PERF_QUICK=1` (set by
//! `perf bench --quick`) the 1M-node cases are left out. The gallery CPUs
//! (`cpu16`, `cpu16_rtl`) are built by the app, so their tick benchmarks
//! live in `perf/benches/gallery.rs`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::compile::{Options, Pass, Program, compile, compile_observed};
use logi_sim::netlist::{Design, mask};
use logi_sim::sim::{Sim, SimOptions};
use logi_sim::testkit::{GenConfig, bitblast::bitblast, fixtures, generate};

/// `PERF_QUICK` is set: leave out the largest cases.
fn quick() -> bool {
    std::env::var_os("PERF_QUICK").is_some_and(|v| v != "0")
}

/// Node counts of the random designs: 10k, 100k and (unless quick) 1M.
fn sizes() -> Vec<usize> {
    if quick() {
        vec![10_000, 100_000]
    } else {
        vec![10_000, 100_000, 1_000_000]
    }
}

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
    let cases = sizes()
        .into_iter()
        .map(|n| (format!("{n}"), generate(&GenConfig::dag(1, n, 32)).design))
        .collect();
    bench_sim(c, "sim/dag32", cases);
}

fn gates(c: &mut Criterion) {
    let cases = sizes()
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
    let sizes: &[usize] = if quick() {
        &[100_000]
    } else {
        &[100_000, 1_000_000]
    };
    for &n in sizes {
        let d = generate(&GenConfig::gates(4, n, 1)).design;
        g.throughput(Throughput::Elements(n as u64));
        g.bench_function(BenchmarkId::new("gates1", n), |b| {
            b.iter(|| black_box(compile(&d, &Options::default()).unwrap()))
        });
    }
    g.finish();
}

/// Time of each compiler pass ([`Pass`]), measured through
/// [`compile_observed`]: every iteration compiles the whole design and
/// only the named pass's time is counted.
fn compile_passes(c: &mut Criterion) {
    let mut g = c.benchmark_group("sim/compile_pass");
    g.sample_size(10).measurement_time(Duration::from_secs(3));
    let cases = [
        (
            "gates1_100k",
            generate(&GenConfig::gates(4, 100_000, 1)).design,
        ),
        (
            "dag32_100k",
            generate(&GenConfig::dag(5, 100_000, 32)).design,
        ),
    ];
    for (name, d) in &cases {
        for pass in Pass::ALL {
            g.bench_function(BenchmarkId::new(*name, pass.name()), |b| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let mut last = Instant::now();
                        let p = compile_observed(d, &Options::default(), |done| {
                            let now = Instant::now();
                            if done == pass {
                                total += now - last;
                            }
                            last = now;
                        });
                        black_box(p.unwrap());
                    }
                    total
                })
            });
        }
    }
    g.finish();
}

/// One settle on 1, 2 and 4 threads, with the program laid out for that
/// many (`Options::for_threads`). Results are identical; only the time
/// differs.
fn threads(c: &mut Criterion) {
    let mut g = c.benchmark_group("sim/threads");
    g.sample_size(20).measurement_time(Duration::from_secs(4));
    let n = if quick() { 100_000 } else { 1_000_000 };
    let cases = [
        (
            format!("gates1_{n}"),
            generate(&GenConfig::gates(2, n, 1)).design,
        ),
        (
            format!("dag32_{n}"),
            generate(&GenConfig::dag(1, n, 32)).design,
        ),
    ];
    for (name, d) in &cases {
        for t in [1usize, 2, 4] {
            let p = Arc::new(compile(d, &Options::default().for_threads(t)).unwrap());
            let widths: Vec<u8> = p.ports.inputs.iter().map(|x| x.width).collect();
            g.throughput(Throughput::Elements(p.work() as u64));
            let mut sim = Sim::with_options(Arc::clone(&p), SimOptions::threaded(t));
            let mut lfsr = 0x9E37_79B9_7F4A_7C15u64;
            g.bench_function(BenchmarkId::new(name.as_str(), format!("{t}t")), |b| {
                b.iter(|| tick_with_inputs(&mut sim, &widths, &mut lfsr));
            });
        }
    }
    g.finish();
}

criterion_group!(
    benches,
    dags,
    gates,
    cpu_and_datapath,
    compile_time,
    compile_passes,
    threads
);
criterion_main!(benches);
