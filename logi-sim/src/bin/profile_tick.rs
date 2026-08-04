use std::{env, hint::black_box, process::ExitCode, thread, time::Duration, time::Instant};

use logi_sim::{engine::RunnerType, fixtures, prelude::*};

const DEFAULT_TICKS: u64 = 100_000_000;

#[derive(Clone, Copy)]
enum Representation {
    Naive,
    Flat,
    Event,
    Simd,
}

struct Options {
    representation: Representation,
    engine: bool,
    optimized: bool,
    fixture: &'static str,
    ticks: u64,
}

fn usage() -> &'static str {
    "usage: profile_tick [engine-][naive|flat|event|simd][-raw] [and|adder|adder8|quiescent500|recursive16|cpu] [ticks]\n\
     example: profile_tick engine-simd cpu 100000000"
}

fn parse_options() -> Result<Options, String> {
    let mut args = env::args().skip(1);
    let runner = args.next().unwrap_or_else(|| "simd".to_owned());
    let fixture_arg = args.next().unwrap_or_else(|| "cpu".to_owned());
    let ticks = args
        .next()
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| "ticks must be a positive integer".to_owned())?
        .unwrap_or(DEFAULT_TICKS);

    let optimized = !runner.ends_with("-raw");
    let runner = runner.strip_suffix("-raw").unwrap_or(&runner);
    let engine = runner.starts_with("engine-");
    let runner = runner.strip_prefix("engine-").unwrap_or(runner);
    let representation = match runner {
        "naive" => Representation::Naive,
        "flat" => Representation::Flat,
        "event" => Representation::Event,
        "simd" => Representation::Simd,
        _ => return Err(format!("unknown runner '{runner}'\n{}", usage())),
    };
    let fixture = match fixture_arg.as_str() {
        "and" => "and",
        "adder" => "adder",
        "adder8" => "adder8",
        "quiescent500" => "quiescent500",
        "recursive16" => "recursive16",
        "cpu" => "cpu",
        _ => return Err(format!("unknown fixture '{fixture_arg}'\n{}", usage())),
    };
    if ticks == 0 {
        return Err("ticks must be greater than zero".to_owned());
    }

    Ok(Options {
        representation,
        engine,
        optimized,
        fixture,
        ticks,
    })
}

fn load_fixture(name: &str) -> fixtures::Fixture {
    match name {
        "and" => fixtures::and_gate(),
        "adder" => fixtures::full_adder(),
        "adder8" => fixtures::ripple_carry_adder_8bit(),
        "quiescent500" => fixtures::large_quiescent_grid_500(),
        "recursive16" => fixtures::recursive_16bit_adder(),
        "cpu" => fixtures::cpu_subsystem_8bit(),
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
