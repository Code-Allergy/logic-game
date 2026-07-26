use super::common::{EventDrivenFactory, FlatFactory, NaiveFactory, RunnerFactory, SimdFactory};
use logi_sim::fixtures;
use logi_sim::runner::Runner;

pub fn run_complex_suite<F>()
where
    F: RunnerFactory,
{
    nand_gate::<F>();
    nor_gate::<F>();
    xor_gate::<F>();
    xnor_gate::<F>();
    multiplexer::<F>();
}

fn nand_gate<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::nand_gate();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let output = fixture.nodes["Y"];
    let mut runner = F::create(&fixture.circuit);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);
}

fn nor_gate<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::nor_gate();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let output = fixture.nodes["Y"];
    let mut runner = F::create(&fixture.circuit);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);
}

fn xor_gate<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::xor_gate();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let output = fixture.nodes["Y"];
    let mut runner = F::create(&fixture.circuit);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);
}

fn xnor_gate<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::xnor_gate();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let output = fixture.nodes["Y"];
    let mut runner = F::create(&fixture.circuit);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);
}

fn multiplexer<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::multiplexer();
    let d0 = fixture.nodes["D0"];
    let d1 = fixture.nodes["D1"];
    let sel = fixture.nodes["Sel"];
    let output = fixture.nodes["Y"];
    let mut runner = F::create(&fixture.circuit);

    // Sel = 0 -> Y = D0
    runner.set_input(sel, 0, false);

    runner.set_input(d0, 0, false);
    runner.set_input(d1, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(d0, 0, true);
    runner.set_input(d1, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    // Sel = 1 -> Y = D1
    runner.set_input(sel, 0, true);

    runner.set_input(d0, 0, true);
    runner.set_input(d1, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

    runner.set_input(d0, 0, false);
    runner.set_input(d1, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);
}

#[test]
fn complex_suite_naive() {
    run_complex_suite::<NaiveFactory>();
}

#[test]
fn complex_suite_flat() {
    run_complex_suite::<FlatFactory>();
}

#[test]
fn complex_suite_event_driven() {
    run_complex_suite::<EventDrivenFactory>();
}

#[test]
fn complex_suite_simd() {
    run_complex_suite::<SimdFactory>();
}
