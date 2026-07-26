use logi_sim::prelude::*;
use std::thread::sleep;
use std::time::Duration;

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
    engine.set_tick_rate(10000.0);

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
fn test_flat_runner_equivalence() {
    let mut c = Circuit::new();
    let in_a = c.input();
    let in_b = c.input();
    let and = c.and();
    let out = c.output();

    c.connect((in_a, 0), (and, 0));
    c.connect((in_b, 0), (and, 1));
    c.connect((and, 0), (out, 0));

    let mut naive = NaiveRunner::new(&c);
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

