use logi_sim::circuit::Circuit;
use logi_sim::runner::{NaiveRunner, Runner};

use logi_sim::fixtures;

struct Factory;

pub trait RunnerFactory {
    type Runner<'a>: Runner
    where
        Self: 'a;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a>;
}

impl RunnerFactory for Factory {
    type Runner<'a> = NaiveRunner<'a>;

    fn create<'a>(circuit: &'a Circuit) -> Self::Runner<'a> {
        NaiveRunner::new(circuit)
    }
}

// AND gate runner test

pub fn run_gate_suite<F>()
where
    F: RunnerFactory,
{
    and_gate::<F>();
    or_gate::<F>();
    not_gate::<F>();
}

fn and_gate<F>()
where
    F: RunnerFactory,
{
    let and_gate_fixture = fixtures::and_gate();
    let input_a = and_gate_fixture.nodes["A"];
    let input_b = and_gate_fixture.nodes["B"];
    let output = and_gate_fixture.nodes["Y"];
    let mut runner = F::create(&and_gate_fixture.circuit);

    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);

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

fn or_gate<F>()
where
    F: RunnerFactory,
{
    let or_gate_fixture = fixtures::or_gate();
    let input_a = or_gate_fixture.nodes["A"];
    let input_b = or_gate_fixture.nodes["B"];
    let output = or_gate_fixture.nodes["Y"];
    let mut runner = F::create(&or_gate_fixture.circuit);

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
    assert_eq!(runner.output(output, 0), true);
}

fn not_gate<F>()
where
    F: RunnerFactory,
{
    let not_gate_fixture = fixtures::not_gate();
    let input_a = not_gate_fixture.nodes["A"];
    let output = not_gate_fixture.nodes["Y"];
    let mut runner = F::create(&not_gate_fixture.circuit);

    runner.set_input(input_a, 0, false);
    runner.tick();
    assert_eq!(runner.output(output, 0), true);

    runner.set_input(input_a, 0, true);
    runner.tick();
    assert_eq!(runner.output(output, 0), false);
}

#[test]
fn gate_suite() {
    run_gate_suite::<Factory>();
}
