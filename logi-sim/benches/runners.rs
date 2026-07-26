use criterion::{Criterion, criterion_group, criterion_main};
use logi_sim::circuit::{Circuit, NodeId};
use logi_sim::fixtures;
use logi_sim::prelude::*;

pub trait BenchmarkRunnerFactory {
    fn name() -> &'static str;
    type Runner<'a>: Runner
    where
        Self: 'a;
    fn create<'a>(circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a>;
}

// 1. NaiveRunner - Unoptimized
pub struct NaiveUnoptimizedBenchFactory;
impl BenchmarkRunnerFactory for NaiveUnoptimizedBenchFactory {
    fn name() -> &'static str {
        "NaiveRunner-unoptimized"
    }
    type Runner<'a> = NaiveRunner<'a>;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit)
    }
}

// 2. NaiveRunner - Optimized
pub struct NaiveOptimizedBenchFactory;
impl BenchmarkRunnerFactory for NaiveOptimizedBenchFactory {
    fn name() -> &'static str {
        "NaiveRunner-optimized"
    }
    type Runner<'a> = NaiveRunner<'a>;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(opt_circuit)
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

// 3. FlatRunner - Unoptimized
pub struct FlatUnoptimizedBenchFactory;
impl BenchmarkRunnerFactory for FlatUnoptimizedBenchFactory {
    fn name() -> &'static str {
        "FlatRunner-unoptimized"
    }
    type Runner<'a> = FlatRunnerBenchWrapper;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(circuit);
        FlatRunnerBenchWrapper(FlatRunner::new(flat_circuit))
    }
}

// 4. FlatRunner - Optimized
pub struct FlatOptimizedBenchFactory;
impl BenchmarkRunnerFactory for FlatOptimizedBenchFactory {
    fn name() -> &'static str {
        "FlatRunner-optimized"
    }
    type Runner<'a> = FlatRunnerBenchWrapper;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(opt_circuit);
        FlatRunnerBenchWrapper(FlatRunner::new(flat_circuit))
    }
}

fn circuit_with_dead_code() -> Circuit {
    let mut c = fixtures::full_adder().circuit;
    // Insert 50 dead floating gates
    for _ in 0..50 {
        let a = c.and();
        let b = c.or();
        c.connect((a, 0), (b, 0));
    }
    c
}

fn bench_factory<F: BenchmarkRunnerFactory>(c: &mut Criterion) {
    let factory_name = F::name();

    // 1. AND gate micro-benchmark
    let and_fixture = fixtures::and_gate();
    let opt_and = Compiler::optimize(&and_fixture.circuit);
    let mut and_runner = F::create(&and_fixture.circuit, &opt_and);
    c.bench_function(&format!("{}/and_gate_tick", factory_name), |b| {
        b.iter(|| {
            and_runner.tick();
        });
    });

    // 2. Full Adder benchmark
    let adder_fixture = fixtures::full_adder();
    let opt_adder = Compiler::optimize(&adder_fixture.circuit);
    let mut adder_runner = F::create(&adder_fixture.circuit, &opt_adder);
    c.bench_function(&format!("{}/full_adder_tick", factory_name), |b| {
        b.iter(|| {
            adder_runner.tick();
        });
    });

    // 3. D Flip-Flop benchmark
    let dff_fixture = fixtures::d_flip_flop();
    let opt_dff = Compiler::optimize(&dff_fixture.circuit);
    let mut dff_runner = F::create(&dff_fixture.circuit, &opt_dff);
    c.bench_function(&format!("{}/d_flip_flop_tick", factory_name), |b| {
        b.iter(|| {
            dff_runner.tick();
        });
    });

    // 4. Circuit with 50 dead gates (tests dead-code culling optimization speedup)
    let dead_raw = circuit_with_dead_code();
    let dead_opt = Compiler::optimize(&dead_raw);
    let mut dead_runner = F::create(&dead_raw, &dead_opt);
    c.bench_function(&format!("{}/dead_code_circuit_tick", factory_name), |b| {
        b.iter(|| {
            dead_runner.tick();
        });
    });
}

pub struct EventDrivenRunnerBenchWrapper(EventDrivenRunner);

impl Runner for EventDrivenRunnerBenchWrapper {
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

// 5. EventDrivenRunner - Unoptimized
pub struct EventDrivenUnoptimizedBenchFactory;
impl BenchmarkRunnerFactory for EventDrivenUnoptimizedBenchFactory {
    fn name() -> &'static str {
        "EventDrivenRunner-unoptimized"
    }
    type Runner<'a> = EventDrivenRunnerBenchWrapper;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let event_circuit = EventDrivenCircuit::compile(circuit);
        EventDrivenRunnerBenchWrapper(EventDrivenRunner::new(event_circuit))
    }
}

// 6. EventDrivenRunner - Optimized
pub struct EventDrivenOptimizedBenchFactory;
impl BenchmarkRunnerFactory for EventDrivenOptimizedBenchFactory {
    fn name() -> &'static str {
        "EventDrivenRunner-optimized"
    }
    type Runner<'a> = EventDrivenRunnerBenchWrapper;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let event_circuit = EventDrivenCircuit::compile(opt_circuit);
        EventDrivenRunnerBenchWrapper(EventDrivenRunner::new(event_circuit))
    }
}

fn runner_benchmarks(c: &mut Criterion) {
    bench_factory::<NaiveUnoptimizedBenchFactory>(c);
    bench_factory::<NaiveOptimizedBenchFactory>(c);
    bench_factory::<FlatUnoptimizedBenchFactory>(c);
    bench_factory::<FlatOptimizedBenchFactory>(c);
    bench_factory::<EventDrivenUnoptimizedBenchFactory>(c);
    bench_factory::<EventDrivenOptimizedBenchFactory>(c);
}

criterion_group!(benches, runner_benchmarks);
criterion_main!(benches);


