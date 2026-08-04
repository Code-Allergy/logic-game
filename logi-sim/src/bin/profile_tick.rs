use std::{env, hint::black_box, process::ExitCode, thread, time::Duration, time::Instant};

use logi_sim::{engine::RunnerType, fixtures, prelude::*};

const DEFAULT_TICKS: u64 = 100_000_000;
const FAST_INPUT_DENSE_DEFAULT_TICKS: u64 = 2_000_000;

#[derive(Clone, Copy)]
enum Representation {
    Naive,
    Flat,
    Event,
    ThreadedFlat,
    Simd,
}

struct Options {
    representation: Representation,
    engine: bool,
    optimized: bool,
    fixture: &'static str,
    fast_input: bool,
    ticks: u64,
}

fn usage() -> &'static str {
    "usage: profile_tick [engine-][naive|flat|event|threadedflat|simd][-raw] [and|adder|adder8|quiescent500|recursive16|cpu|dense|dense-fast-input] [ticks]\n\
     example: profile_tick engine-threadedflat cpu 100000000"
}

fn parse_options() -> Result<Options, String> {
    let mut args = env::args().skip(1);
    let runner = args.next().unwrap_or_else(|| "simd".to_owned());
    let fixture_arg = args.next().unwrap_or_else(|| "cpu".to_owned());
    let ticks_arg = args.next();
    let ticks = match &ticks_arg {
        Some(raw) => Some(
            raw.parse::<u64>()
                .map_err(|_| "ticks must be a positive integer".to_owned())?,
        ),
        None => None,
    };

    let optimized = !runner.ends_with("-raw");
    let runner = runner.strip_suffix("-raw").unwrap_or(&runner);
    let engine = runner.starts_with("engine-");
    let runner = runner.strip_prefix("engine-").unwrap_or(runner);
    let representation = match runner {
        "naive" => Representation::Naive,
        "flat" => Representation::Flat,
        "event" => Representation::Event,
        "threadedflat" => Representation::ThreadedFlat,
        "simd" => Representation::Simd,
        _ => return Err(format!("unknown runner '{runner}'\n{}", usage())),
    };
    let (fixture, fast_input) = match fixture_arg.as_str() {
        "and" => ("and", false),
        "adder" => ("adder", false),
        "adder8" => ("adder8", false),
        "quiescent500" => ("quiescent500", false),
        "recursive16" => ("recursive16", false),
        "cpu" => ("cpu", false),
        "dense" => ("dense", false),
        "dense-fast-input" => ("dense", true),
        _ => return Err(format!("unknown fixture '{fixture_arg}'\n{}", usage())),
    };
    let dense_fast_default_ticks = if fast_input {
        FAST_INPUT_DENSE_DEFAULT_TICKS
    } else {
        DEFAULT_TICKS
    };
    let ticks = ticks.unwrap_or(dense_fast_default_ticks);
    if ticks == 0 {
        return Err("ticks must be greater than zero".to_owned());
    }

    Ok(Options {
        representation,
        engine,
        optimized,
        fixture,
        fast_input,
        ticks,
    })
}

fn load_fixture(name: &str) -> fixtures::Fixture {
    use std::collections::HashMap;

    match name {
        "and" => fixtures::and_gate(),
        "adder" => fixtures::full_adder(),
        "adder8" => fixtures::ripple_carry_adder_8bit(),
        "quiescent500" => fixtures::large_quiescent_grid_500(),
        "recursive16" => fixtures::recursive_16bit_adder(),
        "cpu" => fixtures::cpu_subsystem_8bit(),
        "dense" => {
            let dense = fixtures::large_dense_fixture();
            let mut nodes = HashMap::new();
            nodes.insert("Output", dense.output);
            fixtures::Fixture {
                circuit: dense.circuit,
                nodes,
            }
        }
        _ => unreachable!(),
    }
}

#[inline(never)]
fn profile_ticks<R: Runner>(mut runner: R, output: NodeId, ticks: u64) -> (u64, bool) {
    for _ in 0..ticks {
        runner.tick();
    }
    (
        black_box(runner.current_tick()),
        black_box(runner.output(output, 0)),
    )
}

#[inline(never)]
fn profile_slow_input_dense_simd(
    circuit: logi_sim::circuit::Circuit,
    output: NodeId,
    optimized: bool,
    ticks: u64,
) -> (u64, bool) {
    let (circuit, output) = if optimized {
        let (optimized, node_map) = Compiler::optimize_with_map(&circuit);
        (optimized, node_map[&output])
    } else {
        (circuit, output)
    };

    let fixture = fixtures::large_dense_fixture();
    let bit_packed = BitPackedCircuit::compile(&circuit);
    let node_output_pins = bit_packed.node_output_pins.clone();
    let mut runner = SimdRunner::new(bit_packed);

    let mut input_wires = Vec::with_capacity(fixture.input_nodes.len());
    for node in &fixture.input_nodes {
        if let Some(wires) = node_output_pins.get(&(*node, 0))
            && let Some(&wire) = wires.first()
        {
            input_wires.push(wire);
        }
    }

    let mut previous_chunks: Vec<u64> = vec![0; input_wires.len().div_ceil(64)];
    let mut changed_inputs: Vec<(usize, u64)> = Vec::with_capacity(input_wires.len() / 2);
    let mut lfsr = 0x9E3779B97F4A7C15u64;
    runner.set_lane_mode_vector();

    for _ in 0..ticks {
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
        runner.set_input_scalar_wire_values(&changed_inputs);
        changed_inputs.clear();
        runner.tick();
    }
    let output_value = runner.output(output, 0);
    (runner.current_tick(), output_value)
}

#[inline(never)]
fn profile_engine(mut engine: SimulationEngine, runner_type: RunnerType, ticks: u64) -> u64 {
    engine.set_runner(runner_type);
    engine.start_worker();
    engine.set_tick_rate(None);
    while engine.tick_count() < ticks {
        thread::sleep(Duration::from_micros(50));
    }
    engine.stop_worker();
    black_box(engine.tick_count())
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let fixture = load_fixture(options.fixture);
    let raw_output = fixture
        .circuit
        .nodes
        .iter()
        .find_map(|(id, node)| matches!(node.kind, NodeKind::Output).then_some(id))
        .expect("profiling fixture must contain an output");
    let raw_circuit = fixture.circuit;
    let start = Instant::now();

    if options.engine {
        let runner_type = match (options.representation, options.optimized) {
            (Representation::Naive, false) => RunnerType::Naive,
            (Representation::Naive, true) => RunnerType::NaiveOptimized,
            (Representation::Flat, false) => RunnerType::Flat,
            (Representation::Flat, true) => RunnerType::FlatOptimized,
            (Representation::Event, false) => RunnerType::EventDriven,
            (Representation::Event, true) => RunnerType::EventDrivenOptimized,
            (Representation::ThreadedFlat, false) => RunnerType::ThreadedFlat,
            (Representation::ThreadedFlat, true) => RunnerType::ThreadedFlatOptimized,
            (Representation::Simd, false) => RunnerType::Simd,
            (Representation::Simd, true) => RunnerType::SimdOptimized,
        };
        let completed = profile_engine(
            SimulationEngine::new(raw_circuit),
            runner_type,
            options.ticks,
        );
        let elapsed = start.elapsed();
        eprintln!(
            "completed {completed} engine ticks in {:.3}s ({:.3} MHz)",
            elapsed.as_secs_f64(),
            completed as f64 / elapsed.as_secs_f64() / 1_000_000.0
        );
        return ExitCode::SUCCESS;
    }

    if options.fast_input {
        if !matches!(options.representation, Representation::Simd) {
            eprintln!("dense-fast-input fixture requires simd representation");
            return ExitCode::FAILURE;
        }
        let start = Instant::now();
        let (completed, output_value) = profile_slow_input_dense_simd(
            raw_circuit,
            raw_output,
            options.optimized,
            options.ticks,
        );
        let elapsed = start.elapsed();
        let rate = completed as f64 / elapsed.as_secs_f64();
        black_box(output_value);
        eprintln!(
            "completed {completed} ticks in {:.3}s ({:.3} MHz)",
            elapsed.as_secs_f64(),
            rate / 1_000_000.0
        );
        return ExitCode::SUCCESS;
    }

    let (circuit, output) = if options.optimized {
        let (optimized, node_map) = Compiler::optimize_with_map(&raw_circuit);
        let output = node_map[&raw_output];
        (optimized, output)
    } else {
        (raw_circuit, raw_output)
    };

    let (completed, output_value) = match options.representation {
        Representation::Naive => profile_ticks(NaiveRunner::new(circuit), output, options.ticks),
        Representation::Flat => profile_ticks(
            FlatRunner::new(FlattenedCircuit::compile(&circuit)),
            output,
            options.ticks,
        ),
        Representation::Event => profile_ticks(
            EventDrivenRunner::new(EventDrivenCircuit::compile(&circuit)),
            output,
            options.ticks,
        ),
        Representation::ThreadedFlat => profile_ticks(
            ThreadedFlatRunner::new(FlattenedCircuit::compile(&circuit)),
            output,
            options.ticks,
        ),
        Representation::Simd => profile_ticks(
            SimdRunner::new(BitPackedCircuit::compile(&circuit)),
            output,
            options.ticks,
        ),
    };
    let elapsed = start.elapsed();
    let rate = completed as f64 / elapsed.as_secs_f64();
    black_box(output_value);
    eprintln!(
        "completed {completed} ticks in {:.3}s ({:.3} MHz)",
        elapsed.as_secs_f64(),
        rate / 1_000_000.0
    );
    ExitCode::SUCCESS
}
