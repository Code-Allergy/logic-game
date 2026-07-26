use criterion::{Criterion, criterion_group, criterion_main};
use logi_sim::circuit::{Circuit, NodeId};
use logi_sim::fixtures;
use logi_sim::prelude::*;

pub trait BenchmarkRunnerFactory {
    fn name() -> &'static str;
    type Runner<'a>: Runner
    where
        Self: 'a;
    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a>;
}

pub struct NaiveBenchFactory;
impl BenchmarkRunnerFactory for NaiveBenchFactory {
    fn name() -> &'static str {
        "NaiveRunner"
    }
    type Runner<'a> = NaiveRunner<'a>;
    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit)
    }
}

pub struct FlatRunnerBenchWrapper(FlatRunner);

impl Runner for FlatRunnerBenchWrapper {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        self.0.set_input(node, pin, value);
    }
    fn tick(&mut self) {
        self.0.tick();
    }
    fn output(&self, node: NodeId, pin: u8) -> bool {
        self.0.output(node, pin)
    }
}

pub struct FlatBenchFactory;
impl BenchmarkRunnerFactory for FlatBenchFactory {
    fn name() -> &'static str {
        "FlatRunner"
    }
    type Runner<'a> = FlatRunnerBenchWrapper;
    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(circuit);
        FlatRunnerBenchWrapper(FlatRunner::new(flat_circuit))
    }
}

fn bench_factory<F: BenchmarkRunnerFactory>(c: &mut Criterion) {
    let factory_name = F::name();

    // 1. Benchmark AND gate ticks
    let and_fixture = fixtures::and_gate();
    let mut and_runner = F::create(&and_fixture.circuit);
    c.bench_function(&format!("{}/and_gate_tick", factory_name), |b| {
        b.iter(|| {
            and_runner.tick();
        });
    });

    // 2. Benchmark Full Adder ticks
    let adder_fixture = fixtures::full_adder();
    let mut adder_runner = F::create(&adder_fixture.circuit);
    c.bench_function(&format!("{}/full_adder_tick", factory_name), |b| {
        b.iter(|| {
            adder_runner.tick();
        });
    });

    // 3. Benchmark D Flip-Flop ticks
    let dff_fixture = fixtures::d_flip_flop();
    let mut dff_runner = F::create(&dff_fixture.circuit);
    c.bench_function(&format!("{}/d_flip_flop_tick", factory_name), |b| {
        b.iter(|| {
            dff_runner.tick();
        });
    });
}

fn runner_benchmarks(c: &mut Criterion) {
    bench_factory::<NaiveBenchFactory>(c);
    bench_factory::<FlatBenchFactory>(c);
}

criterion_group!(benches, runner_benchmarks);
criterion_main!(benches);
