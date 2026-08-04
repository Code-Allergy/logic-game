use std::{hint::black_box, thread, time::Duration};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use logi_sim::{engine::SimulationEngine, fixtures};

const TICKS_PER_SAMPLE: u64 = 100_000;

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

fn config() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1))
        .sample_size(10)
}

criterion_group! {
    name = benches;
    config = config();
    targets = unlimited_worker
}
criterion_main!(benches);
