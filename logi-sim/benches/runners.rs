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
    type Runner<'a> = NaiveRunner;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit.clone())
    }
}

// 2. NaiveRunner - Optimized
pub struct NaiveOptimizedBenchFactory;
impl BenchmarkRunnerFactory for NaiveOptimizedBenchFactory {
    fn name() -> &'static str {
        "NaiveRunner-optimized"
    }
    type Runner<'a> = NaiveRunner;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(opt_circuit.clone())
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
    fn current_tick(&self) -> u64 {
        self.0.current_tick()
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

pub struct ThreadedFlatUnoptimizedBenchFactory;
impl BenchmarkRunnerFactory for ThreadedFlatUnoptimizedBenchFactory {
    fn name() -> &'static str {
        "ThreadedFlatRunner-unoptimized"
    }
    type Runner<'a> = ThreadedFlatRunner;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(circuit);
        ThreadedFlatRunner::new(flat_circuit)
    }
}

pub struct ThreadedFlatOptimizedBenchFactory;
impl BenchmarkRunnerFactory for ThreadedFlatOptimizedBenchFactory {
    fn name() -> &'static str {
        "ThreadedFlatRunner-optimized"
    }
    type Runner<'a> = ThreadedFlatRunner;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let flat_circuit = FlattenedCircuit::compile(opt_circuit);
        ThreadedFlatRunner::new(flat_circuit)
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

const BATCH_TICKS: u32 = 1_000;

fn bench_factory<F: BenchmarkRunnerFactory>(c: &mut Criterion) {
    let factory_name = F::name();

    // 1. AND gate micro-benchmark
    let and_fixture = fixtures::and_gate();
    let opt_and = Compiler::optimize(&and_fixture.circuit);
    let mut and_runner = F::create(&and_fixture.circuit, &opt_and);
    c.bench_function(&format!("{}/and_gate_tick", factory_name), |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..BATCH_TICKS {
                    and_runner.tick();
                }
            }
            start.elapsed() / BATCH_TICKS
        });
    });

    // 2. Full Adder benchmark
    let adder_fixture = fixtures::full_adder();
    let opt_adder = Compiler::optimize(&adder_fixture.circuit);
    let mut adder_runner = F::create(&adder_fixture.circuit, &opt_adder);
    c.bench_function(&format!("{}/full_adder_tick", factory_name), |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..BATCH_TICKS {
                    adder_runner.tick();
                }
            }
            start.elapsed() / BATCH_TICKS
        });
    });

    // 3. D Flip-Flop benchmark
    let dff_fixture = fixtures::d_flip_flop();
    let opt_dff = Compiler::optimize(&dff_fixture.circuit);
    let mut dff_runner = F::create(&dff_fixture.circuit, &opt_dff);
    c.bench_function(&format!("{}/d_flip_flop_tick", factory_name), |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..BATCH_TICKS {
                    dff_runner.tick();
                }
            }
            start.elapsed() / BATCH_TICKS
        });
    });

    // 4. Circuit with 50 dead gates (tests dead-code culling optimization speedup)
    let dead_raw = circuit_with_dead_code();
    let dead_opt = Compiler::optimize(&dead_raw);
    let mut dead_runner = F::create(&dead_raw, &dead_opt);
    c.bench_function(&format!("{}/dead_code_circuit_tick", factory_name), |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..BATCH_TICKS {
                    dead_runner.tick();
                }
            }
            start.elapsed() / BATCH_TICKS
        });
    });

    // 5. Large 8-bit Ripple-Carry Adder benchmark
    let adder8_fixture = fixtures::ripple_carry_adder_8bit();
    let opt_adder8 = Compiler::optimize(&adder8_fixture.circuit);
    let mut adder8_runner = F::create(&adder8_fixture.circuit, &opt_adder8);
    let a0_node = adder8_fixture.nodes["A_0"];
    let mut toggle = false;
    c.bench_function(
        &format!("{}/ripple_carry_adder_8bit_tick", factory_name),
        |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    for _ in 0..BATCH_TICKS {
                        toggle = !toggle;
                        adder8_runner.set_input(a0_node, 0, toggle);
                        adder8_runner.tick();
                    }
                }
                start.elapsed() / BATCH_TICKS
            });
        },
    );

    // 6. Large 500-gate Quiescent Grid benchmark (tests event-driven speedup on large sparse events)
    let grid500_fixture = fixtures::large_quiescent_grid_500();
    let opt_grid500 = Compiler::optimize(&grid500_fixture.circuit);
    let mut grid500_runner = F::create(&grid500_fixture.circuit, &opt_grid500);
    let in0_node = grid500_fixture.nodes["In_0"];
    let mut grid_toggle = false;
    c.bench_function(
        &format!("{}/large_quiescent_grid_500_tick", factory_name),
        |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    for _ in 0..BATCH_TICKS {
                        grid_toggle = !grid_toggle;
                        grid500_runner.set_input(in0_node, 0, grid_toggle);
                        grid500_runner.tick();
                    }
                }
                start.elapsed() / BATCH_TICKS
            });
        },
    );

    // 7. 4-Level Recursive 16-Bit Adder benchmark
    let adder16_fixture = fixtures::recursive_16bit_adder();
    let opt_adder16 = Compiler::optimize(&adder16_fixture.circuit);
    let mut adder16_runner = F::create(&adder16_fixture.circuit, &opt_adder16);
    let a0_16 = adder16_fixture.nodes["A_0"];
    let mut toggle16 = false;
    c.bench_function(
        &format!("{}/recursive_16bit_adder_tick", factory_name),
        |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    for _ in 0..BATCH_TICKS {
                        toggle16 = !toggle16;
                        adder16_runner.set_input(a0_16, 0, toggle16);
                        adder16_runner.tick();
                    }
                }
                start.elapsed() / BATCH_TICKS
            });
        },
    );

    // 8. 5-Level Deep 8-Bit CPU Core Subsystem benchmark
    let cpu_fixture = fixtures::cpu_subsystem_8bit();
    let opt_cpu = Compiler::optimize(&cpu_fixture.circuit);
    let mut cpu_runner = F::create(&cpu_fixture.circuit, &opt_cpu);
    let cpu_a0 = cpu_fixture.nodes["A_0"];
    let mut cpu_toggle = false;
    c.bench_function(&format!("{}/cpu_subsystem_8bit_tick", factory_name), |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..BATCH_TICKS {
                    cpu_toggle = !cpu_toggle;
                    cpu_runner.set_input(cpu_a0, 0, cpu_toggle);
                    cpu_runner.tick();
                }
            }
            start.elapsed() / BATCH_TICKS
        });
    });

    // 9. Large dense-throughput random-input benchmark for max-tick stress
    let large_fixture = fixtures::large_dense_fixture();
    let opt_large_fixture = Compiler::optimize(&large_fixture.circuit);
    let mut large_runner = F::create(&large_fixture.circuit, &opt_large_fixture);
    let large_inputs = large_fixture.input_nodes;
    const LARGE_BATCH_TICKS: u32 = 64;
    c.bench_function(
        &format!("{}/large_dense_throughput_tick", factory_name),
        |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                let mut lfsr = 0x9E3779B97F4A7C15u64;
                for _ in 0..iters {
                    for _ in 0..LARGE_BATCH_TICKS {
                        for chunk in large_inputs.chunks(64) {
                            lfsr ^= lfsr << 7;
                            lfsr ^= lfsr >> 9;
                            lfsr ^= lfsr << 8;
                            let chunk_bits = lfsr;
                            for (bit, node) in chunk.iter().enumerate() {
                                large_runner.set_input(*node, 0, (chunk_bits >> bit) & 1 != 0);
                            }
                        }
                        large_runner.tick();
                    }
                }
                start.elapsed() / LARGE_BATCH_TICKS
            });
        },
    );
}

fn bench_simd_fast_large_dense_throughput(c: &mut Criterion) {
    let large_fixture = fixtures::large_dense_fixture();
    let opt_large_fixture = Compiler::optimize(&large_fixture.circuit);
    let bit_packed = BitPackedCircuit::compile(&opt_large_fixture);
    let node_output_pins = bit_packed.node_output_pins.clone();
    let mut fast_runner = SimdRunner::new(bit_packed);

    let mut input_wires = Vec::with_capacity(large_fixture.input_nodes.len());
    for node in &large_fixture.input_nodes {
        if let Some(wires) = node_output_pins.get(&(*node, 0))
            && let Some(&wire) = wires.first()
        {
            input_wires.push(wire);
        }
    }

    let mut previous_chunks: Vec<u64> = vec![0; input_wires.len().div_ceil(64)];
    let mut changed_inputs: Vec<(usize, u64)> = Vec::with_capacity(input_wires.len() / 2);

    const LARGE_BATCH_TICKS: u32 = 64;
    c.bench_function(
        "SimdRunner-optimized/large_dense_fast_input_throughput_tick",
        |b| {
            b.iter_custom(|iters| {
                let start = std::time::Instant::now();
                let mut lfsr = 0x9E3779B97F4A7C15u64;
                for _ in 0..iters {
                    for _ in 0..LARGE_BATCH_TICKS {
                        for (chunk_index, chunk) in input_wires.chunks(64).enumerate() {
                            lfsr ^= lfsr << 7;
                            lfsr ^= lfsr >> 9;
                            lfsr ^= lfsr << 8;
                            let chunk_bits = lfsr;
                            let prev = previous_chunks[chunk_index];
                            let diff = chunk_bits ^ prev;
                            if diff != 0 {
                                let mut bits = diff;
                                while bits != 0 {
                                    let slot = bits.trailing_zeros() as usize;
                                    let wire = chunk[slot];
                                    changed_inputs.push((
                                        wire,
                                        if (chunk_bits & (1u64 << slot)) == 0 {
                                            0
                                        } else {
                                            u64::MAX
                                        },
                                    ));
                                    bits &= bits - 1;
                                }
                                previous_chunks[chunk_index] = chunk_bits;
                            }
                        }

                        fast_runner.set_input_scalar_wire_values(&changed_inputs);
                        changed_inputs.clear();
                        fast_runner.tick();
                    }
                }
                start.elapsed() / LARGE_BATCH_TICKS
            });
        },
    );
}

fn bench_simd_large_dense_tick_only(c: &mut Criterion) {
    let large_fixture = fixtures::large_dense_fixture();
    let opt_large_fixture = Compiler::optimize(&large_fixture.circuit);
    let bit_packed = BitPackedCircuit::compile(&opt_large_fixture);
    let mut fast_runner = SimdRunner::new(bit_packed);

    const LARGE_BATCH_TICKS: u32 = 64;
    c.bench_function("SimdRunner-optimized/large_dense_tick_only", |b| {
        b.iter_custom(|iters| {
            let start = std::time::Instant::now();
            for _ in 0..iters {
                for _ in 0..LARGE_BATCH_TICKS {
                    fast_runner.tick();
                }
            }
            start.elapsed() / LARGE_BATCH_TICKS
        });
    });
}

// ... runner factories ...

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
    fn current_tick(&self) -> u64 {
        self.0.current_tick()
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

pub struct SimdRunnerBenchWrapper(SimdRunner);

impl Runner for SimdRunnerBenchWrapper {
    fn set_input(&mut self, node: NodeId, pin: u8, value: bool) {
        self.0.set_input(node, pin, value);
    }
    fn tick(&mut self) {
        self.0.tick();
    }
    fn output(&self, node: NodeId, pin: u8) -> bool {
        self.0.output(node, pin)
    }
    fn current_tick(&self) -> u64 {
        self.0.current_tick()
    }
}

// 7. SimdRunner - Unoptimized
pub struct SimdUnoptimizedBenchFactory;
impl BenchmarkRunnerFactory for SimdUnoptimizedBenchFactory {
    fn name() -> &'static str {
        "SimdRunner-unoptimized"
    }
    type Runner<'a> = SimdRunnerBenchWrapper;
    fn create<'a>(circuit: &'a Circuit, _opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let bit_packed = BitPackedCircuit::compile(circuit);
        SimdRunnerBenchWrapper(SimdRunner::new(bit_packed))
    }
}

// 8. SimdRunner - Optimized
pub struct SimdOptimizedBenchFactory;
impl BenchmarkRunnerFactory for SimdOptimizedBenchFactory {
    fn name() -> &'static str {
        "SimdRunner-optimized"
    }
    type Runner<'a> = SimdRunnerBenchWrapper;
    fn create<'a>(_circuit: &'a Circuit, opt_circuit: &'a Circuit) -> Self::Runner<'a> {
        let bit_packed = BitPackedCircuit::compile(opt_circuit);
        SimdRunnerBenchWrapper(SimdRunner::new(bit_packed))
    }
}

fn runner_benchmarks(c: &mut Criterion) {
    bench_factory::<NaiveUnoptimizedBenchFactory>(c);
    bench_factory::<NaiveOptimizedBenchFactory>(c);
    bench_factory::<FlatUnoptimizedBenchFactory>(c);
    bench_factory::<FlatOptimizedBenchFactory>(c);
    bench_factory::<ThreadedFlatUnoptimizedBenchFactory>(c);
    bench_factory::<ThreadedFlatOptimizedBenchFactory>(c);
    bench_factory::<EventDrivenUnoptimizedBenchFactory>(c);
    bench_factory::<EventDrivenOptimizedBenchFactory>(c);
    bench_factory::<SimdUnoptimizedBenchFactory>(c);
    bench_factory::<SimdOptimizedBenchFactory>(c);
    bench_simd_fast_large_dense_throughput(c);
    bench_simd_large_dense_tick_only(c);
}

fn fast_criterion() -> Criterion {
    Criterion::default()
        .warm_up_time(std::time::Duration::from_millis(500))
        .measurement_time(std::time::Duration::from_secs(1))
        .sample_size(50)
}

criterion_group! {
    name = benches;
    config = fast_criterion();
    targets = runner_benchmarks
}
criterion_main!(benches);
