use std::{hint::black_box, thread, time::Duration};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::{
    engine::{RunnerType, SimulationEngine},
    fixtures,
    prelude::*,
};

const TICKS_PER_SAMPLE: u64 = 100_000;
const DIRECT_TICKS_PER_SAMPLE: u64 = 10_000;

fn unlimited_worker(c: &mut Criterion) {
    let fixture = fixtures::cpu_subsystem_8bit();
    let mut engine = SimulationEngine::new(fixture.circuit);
    engine.start_worker();
    engine.set_tick_rate(None);

    let mut group = c.benchmark_group("engine");
    group.throughput(Throughput::Elements(TICKS_PER_SAMPLE));
    group.bench_function("unlimited_cpu_subsystem_100k_ticks", |b| {
        b.iter(|| {
            let target = engine.tick_count() + TICKS_PER_SAMPLE;
            while engine.tick_count() < target {
                thread::sleep(Duration::from_micros(50));
            }
            black_box(engine.tick_count());
        });
    });
    group.finish();

    engine.stop_worker();
}

fn common_cell_hot_path(c: &mut Criterion) {
    let mut circuit = Circuit::new();
    let a = circuit.input();
    let b = circuit.input();
    for _ in 0..256 {
        let gate = circuit.and();
        let output = circuit.output();
        circuit.connect((a, 0), (gate, 0));
        circuit.connect((b, 0), (gate, 1));
        circuit.connect((gate, 0), (output, 0));
    }

    let optimized = Compiler::optimize(&circuit);
    assert_eq!(circuit.nodes.len() - optimized.nodes.len(), 255);

    let mut raw = FlatRunner::new(FlattenedCircuit::compile(&circuit));
    let mut optimized = FlatRunner::new(FlattenedCircuit::compile(&optimized));
    let mut group = c.benchmark_group("common_cells_256");
    group.bench_function("raw", |b| b.iter(|| raw.tick()));
    group.bench_function("optimized", |b| b.iter(|| optimized.tick()));
    group.finish();
}

fn direct_service_stepping(c: &mut Criterion) {
    let fixture = fixtures::cpu_subsystem_8bit();

    let mut group = c.benchmark_group("engine");
    group.throughput(Throughput::Elements(DIRECT_TICKS_PER_SAMPLE));
    group.sample_size(20);

    for &runner_type in RunnerType::ALL {
        group.bench_function(
            BenchmarkId::new("direct_service_step", format!("{runner_type}")),
            |b| {
                b.iter_batched(
                    || {
                        let mut engine = SimulationEngine::new(fixture.circuit.clone());
                        engine.set_runner(runner_type);
                        engine
                    },
                    |mut engine| {
                        for _ in 0..DIRECT_TICKS_PER_SAMPLE {
                            engine.step_sync();
                        }
                        black_box(engine.tick_count())
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn config() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1))
        .sample_size(10)
}

criterion_group! {
    name = benches;
    config = config();
    targets = unlimited_worker, common_cell_hot_path, direct_service_stepping
}
criterion_main!(benches);
