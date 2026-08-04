#![allow(clippy::bool_assert_comparison)]

use logi_sim::circuit::{Circuit, NodeId};
use logi_sim::prelude::*;

fn build_half_adder_subcircuit() -> (Circuit, NodeId, NodeId, NodeId, NodeId) {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();

    let sum_xor = c.xor();
    let carry_and = c.and();

    c.connect((a, 0), (sum_xor, 0));
    c.connect((b, 0), (sum_xor, 1));

    c.connect((a, 0), (carry_and, 0));
    c.connect((b, 0), (carry_and, 1));

    let out_sum = c.output();
    let out_carry = c.output();

    c.connect((sum_xor, 0), (out_sum, 0));
    c.connect((carry_and, 0), (out_carry, 0));

    (c, a, b, out_sum, out_carry)
}

fn build_parent_with_subcircuit() -> (Circuit, NodeId, NodeId, NodeId, NodeId) {
    let (half_adder, _, _, _, _) = build_half_adder_subcircuit();

    let mut parent = Circuit::new();
    let in0 = parent.input();
    let in1 = parent.input();

    let sub_node = parent.add_subcircuit(half_adder);

    parent.connect((in0, 0), (sub_node, 0));
    parent.connect((in1, 0), (sub_node, 1));

    let out_sum = parent.output();
    let out_carry = parent.output();

    parent.connect((sub_node, 0), (out_sum, 0));
    parent.connect((sub_node, 1), (out_carry, 0));

    (parent, in0, in1, out_sum, out_carry)
}

fn verify_subcircuit_runner<R: Runner>(
    mut runner: R,
    in0: NodeId,
    in1: NodeId,
    sum: NodeId,
    carry: NodeId,
) {
    let truth_table = [
        (false, false, false, false),
        (false, true, true, false),
        (true, false, true, false),
        (true, true, false, true),
    ];

    for (a, b, expected_sum, expected_carry) in truth_table {
        runner.set_input(in0, 0, a);
        runner.set_input(in1, 0, b);
        runner.tick();
        runner.tick();

        assert_eq!(
            runner.output(sum, 0),
            expected_sum,
            "Sum mismatch for A={}, B={}",
            a,
            b
        );
        assert_eq!(
            runner.output(carry, 0),
            expected_carry,
            "Carry mismatch for A={}, B={}",
            a,
            b
        );
    }
}

#[test]
fn test_subcircuit_naive_runner() {
    let (parent, in0, in1, sum, carry) = build_parent_with_subcircuit();
    let runner = NaiveRunner::new(parent.clone());
    verify_subcircuit_runner(runner, in0, in1, sum, carry);
}

#[test]
fn test_subcircuit_flat_runner() {
    let (parent, in0, in1, sum, carry) = build_parent_with_subcircuit();
    let flat = FlattenedCircuit::compile(&parent);
    let runner = FlatRunner::new(flat);
    verify_subcircuit_runner(runner, in0, in1, sum, carry);
}

#[test]
fn test_subcircuit_event_driven_runner() {
    let (parent, in0, in1, sum, carry) = build_parent_with_subcircuit();
    let event_c = EventDrivenCircuit::compile(&parent);
    let runner = EventDrivenRunner::new(event_c);
    verify_subcircuit_runner(runner, in0, in1, sum, carry);
}

#[test]
fn test_subcircuit_simd_runner() {
    let (parent, in0, in1, sum, carry) = build_parent_with_subcircuit();
    let bit_packed = BitPackedCircuit::compile(&parent);
    let runner = SimdRunner::new(bit_packed);
    verify_subcircuit_runner(runner, in0, in1, sum, carry);
}

fn build_nested_buffer_with_fanout() -> (Circuit, NodeId, NodeId, NodeId) {
    let mut leaf = Circuit::new();
    let leaf_in = leaf.input();
    let buffer = leaf.buffer();
    let leaf_out = leaf.output();
    leaf.connect((leaf_in, 0), (buffer, 0));
    leaf.connect((buffer, 0), (leaf_out, 0));

    let mut wrapper = Circuit::new();
    let wrapper_in = wrapper.input();
    let leaf_node = wrapper.add_subcircuit(leaf);
    let wrapper_out = wrapper.output();
    wrapper.connect((wrapper_in, 0), (leaf_node, 0));
    wrapper.connect((leaf_node, 0), (wrapper_out, 0));

    let mut root = Circuit::new();
    let input = root.input();
    let nested = root.add_subcircuit(wrapper);
    let output_a = root.output();
    let output_b = root.output();
    root.connect((input, 0), (nested, 0));
    root.connect((nested, 0), (output_a, 0));
    root.connect((nested, 0), (output_b, 0));

    (root, input, output_a, output_b)
}

#[test]
fn optimized_flat_runner_preserves_nested_output_fanout() {
    let (circuit, input, output_a, output_b) = build_nested_buffer_with_fanout();
    let optimized = Compiler::optimize(&circuit);
    let mut runner = FlatRunner::new(FlattenedCircuit::compile(&optimized));

    runner.set_input(input, 0, true);
    runner.tick();

    assert!(runner.output(output_a, 0));
    assert!(runner.output(output_b, 0));
}

#[test]
fn test_recursive_16bit_adder_all_runners() {
    let fixture = logi_sim::fixtures::recursive_16bit_adder();
    let opt_circuit = Compiler::optimize(&fixture.circuit);

    // Test NaiveRunner, FlatRunner, EventDrivenRunner, and SimdRunner (512-bit vector)
    let mut flat_runner = FlatRunner::new(FlattenedCircuit::compile(&opt_circuit));
    let mut event_runner = EventDrivenRunner::new(EventDrivenCircuit::compile(&opt_circuit));
    let mut simd_runner = SimdRunner::new(BitPackedCircuit::compile(&opt_circuit));

    // Set A_0 = true, B_0 = true -> Expected Sum_0 = false, Carry = true
    let a0 = fixture.nodes["A_0"];
    let b0 = fixture.nodes["B_0"];
    let s0 = fixture.nodes["Sum_0"];

    for runner in [
        &mut flat_runner as &mut dyn Runner,
        &mut event_runner,
        &mut simd_runner,
    ] {
        runner.set_input(a0, 0, true);
        runner.set_input(b0, 0, true);

        for _ in 0..10 {
            runner.tick();
        }

        assert_eq!(runner.output(s0, 0), false);
    }
}
