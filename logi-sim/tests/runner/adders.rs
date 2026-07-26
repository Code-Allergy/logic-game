use super::common::{EventDrivenFactory, FlatFactory, NaiveFactory, RunnerFactory, SimdFactory};
use logi_sim::fixtures;
use logi_sim::runner::Runner;

pub fn run_adders_suite<F>()
where
    F: RunnerFactory,
{
    half_adder::<F>();
    full_adder::<F>();
}

fn half_adder<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::half_adder();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let sum = fixture.nodes["Sum"];
    let carry = fixture.nodes["Carry"];
    let mut runner = F::create(&fixture.circuit);

    // 0 + 0 = 0 (carry 0)
    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(sum, 0), false);
    assert_eq!(runner.output(carry, 0), false);

    // 1 + 0 = 1 (carry 0)
    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, false);
    runner.tick();
    assert_eq!(runner.output(sum, 0), true);
    assert_eq!(runner.output(carry, 0), false);

    // 0 + 1 = 1 (carry 0)
    runner.set_input(input_a, 0, false);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(sum, 0), true);
    assert_eq!(runner.output(carry, 0), false);

    // 1 + 1 = 0 (carry 1)
    runner.set_input(input_a, 0, true);
    runner.set_input(input_b, 0, true);
    runner.tick();
    assert_eq!(runner.output(sum, 0), false);
    assert_eq!(runner.output(carry, 0), true);
}

fn full_adder<F>()
where
    F: RunnerFactory,
{
    let fixture = fixtures::full_adder();
    let input_a = fixture.nodes["A"];
    let input_b = fixture.nodes["B"];
    let input_cin = fixture.nodes["Cin"];
    let sum = fixture.nodes["Sum"];
    let cout = fixture.nodes["Cout"];
    let mut runner = F::create(&fixture.circuit);

    let test_cases = [
        // A, B, Cin -> Sum, Cout
        (false, false, false, false, false),
        (false, false, true, true, false),
        (false, true, false, true, false),
        (false, true, true, false, true),
        (true, false, false, true, false),
        (true, false, true, false, true),
        (true, true, false, false, true),
        (true, true, true, true, true),
    ];

    for (a, b, cin, expected_sum, expected_cout) in test_cases {
        runner.set_input(input_a, 0, a);
        runner.set_input(input_b, 0, b);
        runner.set_input(input_cin, 0, cin);

        // Full adder may take a few ticks to propagate fully through two half adders
        // Let's tick a few times just to be safe
        runner.tick();
        runner.tick();
        runner.tick();

        assert_eq!(
            runner.output(sum, 0),
            expected_sum,
            "Sum failed for A={}, B={}, Cin={}",
            a,
            b,
            cin
        );
        assert_eq!(
            runner.output(cout, 0),
            expected_cout,
            "Cout failed for A={}, B={}, Cin={}",
            a,
            b,
            cin
        );
    }
}

#[test]
fn adders_suite_naive() {
    run_adders_suite::<NaiveFactory>();
}

#[test]
fn adders_suite_flat() {
    run_adders_suite::<FlatFactory>();
}

#[test]
fn adders_suite_event_driven() {
    run_adders_suite::<EventDrivenFactory>();
}

#[test]
fn adders_suite_simd() {
    run_adders_suite::<SimdFactory>();
}
