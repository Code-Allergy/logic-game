//! Tick throughput and compile time of the gallery CPUs, `cpu16` (gates)
//! and `cpu16_rtl` (word primitives), built by the app (`design_of`), so
//! they live here rather than in `logi-sim/benches`. Throughput is in
//! simulator work units (instructions plus packed words), as in
//! `logi-sim/benches/sim.rs`.
//!
//! - `gallery/tick/<cpu>/{default,editor}`: one tick, single-threaded,
//!   with the compiler's defaults and with the editor's options
//!   (`Options::debug`, what the game runs). The CPU restarts from reset
//!   when it halts, so it is always running its program.
//! - `gallery/compile/<cpu>/{default,editor}`: a full compile.
//!
//! The one-shot report with ticks per CPU instruction is
//! `cargo run --release --example cpu_bench`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::compile::{Options, compile};
use logi_sim::sim::Sim;

fn options() -> [(&'static str, Options); 2] {
    [
        ("default", Options::default()),
        ("editor", Options::debug()),
    ]
}

fn tick(c: &mut Criterion) {
    let mut g = c.benchmark_group("gallery/tick");
    g.sample_size(30).measurement_time(Duration::from_secs(4));
    for id in perf::CPUS {
        let d = perf::design(id);
        for (name, opts) in options() {
            let p = Arc::new(compile(&d, &opts).unwrap());
            g.throughput(Throughput::Elements(p.work() as u64));
            let mut sim = Sim::new(Arc::clone(&p));
            g.bench_function(BenchmarkId::new(id, name), |b| {
                b.iter(|| {
                    if sim.get("Halted") != 0 {
                        sim.reset();
                    }
                    sim.tick();
                });
                black_box(sim.output(0));
            });
        }
    }
    g.finish();
}

fn compile_cpus(c: &mut Criterion) {
    let mut g = c.benchmark_group("gallery/compile");
    g.sample_size(10).measurement_time(Duration::from_secs(4));
    for id in perf::CPUS {
        let d = perf::design(id);
        for (name, opts) in options() {
            g.bench_function(BenchmarkId::new(id, name), |b| {
                b.iter(|| black_box(compile(&d, &opts).unwrap()))
            });
        }
    }
    g.finish();
}

criterion_group!(benches, tick, compile_cpus);
criterion_main!(benches);
