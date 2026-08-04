#![allow(clippy::bool_assert_comparison, clippy::needless_range_loop)]

use logi_sim::{engine::RunnerType, fixtures, prelude::*};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn run_cpu_subsystem_fixture_once(
    engine: &mut SimulationEngine,
    tick_batch: impl Fn(&mut SimulationEngine, usize),
) {
    let fixture = fixtures::cpu_subsystem_8bit();
    let bus_enable = fixture.nodes["Bus_Enable"];
    let alu_op = fixture.nodes["ALU_OpSel"];
    let zero_flag = fixture.nodes["ZeroFlag"];
    let a_bits = [false, false, true, true, false, false, true, true];
    let b_bits = [false, true, false, true, false, true, false, true];

    engine.set_input(bus_enable, 0, true);
    engine.set_input(alu_op, 0, false);

    for bit in 0..8 {
        let a_node = *fixture.nodes.get(format!("A_{bit}").as_str()).unwrap();
        let b_node = *fixture.nodes.get(format!("B_{bit}").as_str()).unwrap();
        engine.set_input(a_node, 0, a_bits[bit]);
        engine.set_input(b_node, 0, b_bits[bit]);
    }
    tick_batch(engine, 20);

    let expected_and = [false, false, false, true, false, false, false, true];
    for bit in 0..8 {
        let bus_node = *fixture.nodes.get(format!("Bus_{bit}").as_str()).unwrap();
        assert_eq!(
            engine.get_input(bus_node, 0),
            expected_and[bit],
            "Bus output mismatch for A != B"
        );
    }
    assert_eq!(
        engine.get_input(zero_flag, 0),
        false,
        "ZeroFlag mismatch for A != B"
    );

    for bit in 0..8 {
        let a_node = *fixture.nodes.get(format!("A_{bit}").as_str()).unwrap();
        let b_node = *fixture.nodes.get(format!("B_{bit}").as_str()).unwrap();
        engine.set_input(a_node, 0, b_bits[bit]);
        engine.set_input(b_node, 0, b_bits[bit]);
    }
    tick_batch(engine, 20);
    assert_eq!(
        engine.get_input(zero_flag, 0),
        true,
        "ZeroFlag mismatch for A == B"
    );
}

#[test]
fn test_simulation_engine_synchronous() {
    let mut c = Circuit::new();
    let in_a = c.input();
    let in_b = c.input();
    let and = c.and();
    let out = c.output();

    let wire_a = c.connect((in_a, 0), (and, 0));
    let wire_b = c.connect((in_b, 0), (and, 1));
    let wire_out = c.connect((and, 0), (out, 0));

    let mut engine = SimulationEngine::new(c);

    // Initial state check
    assert_eq!(engine.get_wire(wire_a), false);
    assert_eq!(engine.get_wire(wire_b), false);
    assert_eq!(engine.get_wire(wire_out), false);

    // Step synchronously
    engine.step_sync();
    assert_eq!(engine.tick_count(), 1);
}

#[test]
fn test_threaded_simulation_engine_atomic_queries() {
    let mut c = Circuit::new();
    let in_a = c.input();
    let in_b = c.input();
    let and = c.and();
    let out = c.output();

    c.connect((in_a, 0), (and, 0));
    c.connect((in_b, 0), (and, 1));
    c.connect((and, 0), (out, 0));

    let mut engine = SimulationEngine::new(c);

    // Start uncapped worker thread
    engine.start_worker();

    // Set tick rate high enough to exceed 100 in 20ms (e.g. 10000 Hz)
    engine.set_tick_rate(Some(10000.0));

    // Give worker time to run a few thousand ticks
    sleep(Duration::from_millis(20));

    assert!(engine.tick_count() > 100);

    // Set input A to true
    engine.set_input(in_a, 0, true);
    sleep(Duration::from_millis(10));
    // Output should still be false because B is false
    assert_eq!(engine.get_input(out, 0), false);

    // Set input B to true
    engine.set_input(in_b, 0, true);
    sleep(Duration::from_millis(10));
    // Now Output should be true (AND gate: true AND true = true)
    assert_eq!(engine.get_input(out, 0), true);

    // Turn A back to false
    engine.set_input(in_a, 0, false);
    sleep(Duration::from_millis(10));
    assert_eq!(engine.get_input(out, 0), false);

    // Stop worker cleanly
    engine.stop_worker();
}

#[test]
fn rate_limited_scheduler_batches_ticks_to_recover_from_os_jitter() {
    let mut circuit = Circuit::new();
    let input = circuit.input();
    let output = circuit.output();
    circuit.connect((input, 0), (output, 0));

    let mut engine = SimulationEngine::new(circuit);
    engine.start_worker();
    engine.set_tick_rate(Some(100_000.0));
    sleep(Duration::from_millis(50));

    let completed = engine.tick_count();
    engine.stop_worker();

    // A one-tick-per-scheduler-wakeup loop typically completes fewer than 100
    // ticks here. Leave ample headroom for loaded CI while verifying catch-up.
    assert!(
        completed >= 1_000,
        "rate-limited scheduler completed only {completed} ticks"
    );
}

#[test]
fn every_runner_publishes_results_across_the_supported_rate_range() {
    const RATES: [f64; 5] = [0.1, 10.0, 1_000.0, 100_000.0, 10_000_000.0];

    for &runner_type in RunnerType::ALL {
        for rate in RATES {
            let mut circuit = Circuit::new();
            let input = circuit.input();
            let buffer = circuit.buffer();
            let output = circuit.output();
            circuit.connect((input, 0), (buffer, 0));
            circuit.connect((buffer, 0), (output, 0));

            let mut engine = SimulationEngine::new(circuit);
            engine.set_runner(runner_type);
            engine.start_worker();
            // Queue a deterministic configuration boundary. Resume schedules
            // an immediate tick even at 0.1 Hz, so this test does not need to
            // wait for a full ten-second period.
            engine.pause();
            engine.set_tick_rate(Some(rate));
            engine.set_input(input, 0, true);
            engine.resume();

            let deadline = Instant::now() + Duration::from_millis(500);
            while !engine.get_input(output, 0) && Instant::now() < deadline {
                sleep(Duration::from_millis(1));
            }
            let output_value = engine.get_input(output, 0);
            engine.stop_worker();

            assert!(
                output_value,
                "{runner_type} did not publish its output at {rate} Hz"
            );
        }
    }
}

#[test]
fn each_runner_direct_mode_matches_cpu_fixture() {
    for &runner_type in RunnerType::ALL {
        let mut engine = SimulationEngine::new(fixtures::cpu_subsystem_8bit().circuit);
        engine.set_runner(runner_type);

        run_cpu_subsystem_fixture_once(&mut engine, |engine, ticks| {
            for _ in 0..ticks {
                engine.step_sync();
            }
        });
    }
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn each_runner_threaded_mode_matches_cpu_fixture() {
    for &runner_type in RunnerType::ALL {
        let mut engine = SimulationEngine::new(fixtures::cpu_subsystem_8bit().circuit);
        engine.set_runner(runner_type);
        engine.start_worker();
        engine.pause();
        engine.set_tick_rate(Some(10.0));

        run_cpu_subsystem_fixture_once(&mut engine, |engine, ticks| {
            let start = engine.tick_count();
            let target = start + ticks as u64;
            engine.step_many(ticks as u64);
            let deadline = Instant::now() + Duration::from_secs(1);
            while engine.tick_count() < target {
                sleep(Duration::from_millis(1));
                assert!(
                    Instant::now() < deadline,
                    "timeout waiting for worker ticks for {runner_type}"
                );
            }
        });

        engine.stop_worker();
    }
}

#[test]
fn test_flat_runner_equivalence() {
    let mut c = Circuit::new();
    let in_a = c.input();
    let in_b = c.input();
    let and = c.and();
    let out = c.output();

    c.connect((in_a, 0), (and, 0));
    c.connect((in_b, 0), (and, 1));
    c.connect((and, 0), (out, 0));

    let mut naive = NaiveRunner::new(c.clone());
    let flat_circuit = FlattenedCircuit::compile(&c);
    let mut flat = FlatRunner::new(flat_circuit);

    naive.set_input(in_a, 0, true);
    flat.set_input(in_a, 0, true);

    naive.set_input(in_b, 0, true);
    flat.set_input(in_b, 0, true);

    naive.tick();
    flat.tick();

    assert_eq!(naive.output(out, 0), flat.output(out, 0));
    assert_eq!(flat.output(out, 0), true);
}
