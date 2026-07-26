use super::common::{EventDrivenFactory, FlatFactory, NaiveFactory, RunnerFactory, SimdFactory};
use logi_sim::fixtures;
use logi_sim::runner::Runner;

pub fn run_sequential_suite<F>()
where
    F: RunnerFactory,
{
    sr_latch::<F>();
    d_flip_flop::<F>();
}

fn sr_latch<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::sr_latch();
    let r = fixture.nodes["R"];
    let s = fixture.nodes["S"];
    let q = fixture.nodes["Q"];
    let qn = fixture.nodes["Qn"];
    let mut runner = F::create(&fixture.circuit);

    // Initial state: Set
    runner.set_input(s, 0, true);
    runner.set_input(r, 0, false);
    for _ in 0..5 {
        runner.tick();
    } // Need multiple ticks for feedback loop to stabilize
    assert_eq!(runner.output(q, 0), true);
    assert_eq!(runner.output(qn, 0), false);

    // Hold state
    runner.set_input(s, 0, false);
    runner.set_input(r, 0, false);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), true);
    assert_eq!(runner.output(qn, 0), false);

    // Reset state
    runner.set_input(s, 0, false);
    runner.set_input(r, 0, true);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), false);
    assert_eq!(runner.output(qn, 0), true);

    // Hold state
    runner.set_input(s, 0, false);
    runner.set_input(r, 0, false);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), false);
    assert_eq!(runner.output(qn, 0), true);

    // Invalid state (both S and R true)
    runner.set_input(s, 0, true);
    runner.set_input(r, 0, true);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), false);
    assert_eq!(runner.output(qn, 0), false);
}

fn d_flip_flop<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::d_flip_flop();
    let d = fixture.nodes["D"];
    let e = fixture.nodes["E"];
    let q = fixture.nodes["Q"];
    let qn = fixture.nodes["Qn"];
    let mut runner = F::create(&fixture.circuit);

    // Initial value
    runner.set_input(d, 0, true);
    runner.set_input(e, 0, false);
    for _ in 0..10 {
        runner.tick();
    } // Stable output may initially be invalid if not initialized, but let's see.

    // Enable is high, D=1 -> Q=1
    runner.set_input(e, 0, true);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), true);
    assert_eq!(runner.output(qn, 0), false);

    // Latch value (Enable goes low)
    runner.set_input(e, 0, false);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), true);
    assert_eq!(runner.output(qn, 0), false);

    // Change D while Enable is low (should not change Q)
    runner.set_input(d, 0, false);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), true); // Still holding 1
    assert_eq!(runner.output(qn, 0), false);

    // Enable goes high, D=0 -> Q=0
    runner.set_input(e, 0, true);
    for _ in 0..5 {
        runner.tick();
    }
    assert_eq!(runner.output(q, 0), false);
    assert_eq!(runner.output(qn, 0), true);
}

#[test]
fn sequential_suite_naive() {
    run_sequential_suite::<NaiveFactory>();
}

#[test]
fn sequential_suite_flat() {
    run_sequential_suite::<FlatFactory>();
}

#[test]
fn sequential_suite_event_driven() {
    run_sequential_suite::<EventDrivenFactory>();
}

#[test]
fn sequential_suite_simd() {
    run_sequential_suite::<SimdFactory>();
}
