use logi_sim::prelude::*;

#[test]
fn test_dead_code_culling() {
    let mut c = Circuit::new();

    // Live branch: A -> AND -> Output
    let in_a = c.input();
    let and1 = c.and();
    let out = c.output();
    c.connect((in_a, 0), (and1, 0));
    c.connect((and1, 0), (out, 0));

    // Dead branch: Unconnected AND gate + OR gate floating in space
    let dead_and = c.and();
    let dead_or = c.or();
    c.connect((dead_and, 0), (dead_or, 0));

    assert_eq!(c.nodes.len(), 5); // 5 nodes total initially

    // Run optimization pass
    let optimized = Compiler::optimize(&c);

    // Optimized circuit should only contain 3 live nodes (in_a, and1, out)
    assert_eq!(optimized.nodes.len(), 3);
}

#[test]
fn common_cells_are_merged_without_losing_wire_observability() {
    let mut circuit = Circuit::new();
    let a = circuit.input();
    let b = circuit.input();
    let and_a = circuit.and();
    let and_b = circuit.and();
    let out_a = circuit.output();
    let out_b = circuit.output();

    circuit.connect((a, 0), (and_a, 0));
    circuit.connect((b, 0), (and_a, 1));
    // Reversed inputs verify commutative-cell canonicalization.
    circuit.connect((b, 0), (and_b, 0));
    circuit.connect((a, 0), (and_b, 1));
    let raw_out_a = circuit.connect((and_a, 0), (out_a, 0));
    let raw_out_b = circuit.connect((and_b, 0), (out_b, 0));

    let (optimized, node_map, wire_map) = Compiler::optimize_with_maps(&circuit);
    assert_eq!(optimized.nodes.len(), circuit.nodes.len() - 1);
    assert_eq!(node_map[&and_a], node_map[&and_b]);
    assert_eq!(wire_map[&raw_out_a], wire_map[&raw_out_b]);

    let mut runner = FlatRunner::new(FlattenedCircuit::compile(&optimized));
    runner.set_input(node_map[&a], 0, true);
    runner.set_input(node_map[&b], 0, true);
    runner.tick();

    assert!(runner.wire_value(wire_map[&raw_out_a]));
    assert!(runner.wire_value(wire_map[&raw_out_b]));
    assert!(runner.output(node_map[&out_a], 0));
    assert!(runner.output(node_map[&out_b], 0));
}

#[test]
fn common_cell_pass_recurses_into_subcircuits() {
    let mut child = Circuit::new();
    let input = child.input();
    let not_a = child.not();
    let not_b = child.not();
    let out_a = child.output();
    let out_b = child.output();
    child.connect((input, 0), (not_a, 0));
    child.connect((input, 0), (not_b, 0));
    child.connect((not_a, 0), (out_a, 0));
    child.connect((not_b, 0), (out_b, 0));

    let mut root = Circuit::new();
    let root_input = root.input();
    let nested = root.add_subcircuit(child);
    let root_out_a = root.output();
    let root_out_b = root.output();
    root.connect((root_input, 0), (nested, 0));
    root.connect((nested, 0), (root_out_a, 0));
    root.connect((nested, 1), (root_out_b, 0));

    let optimized = Compiler::optimize(&root);
    let nested_child = optimized
        .nodes
        .values()
        .find_map(|node| match &node.kind {
            NodeKind::SubCircuit(child) => Some(child),
            _ => None,
        })
        .unwrap();

    assert_eq!(nested_child.nodes.len(), 4);
}

#[test]
fn handmade_counter_byte_is_lowered_inside_a_subcircuit() {
    let handmade = handmade_counter8();
    let mut root = Circuit::new();
    let increment = root.input();
    let reset = root.input();
    let byte = root.add_subcircuit(handmade);
    root.connect((increment, 0), (byte, 0));
    root.connect((reset, 0), (byte, 1));
    let mut root_outputs = Vec::new();
    for bit in 0..8 {
        let output = root.output();
        root.connect((byte, bit), (output, 0));
        root_outputs.push(output);
    }

    let (optimized, node_map, _) = Compiler::optimize_with_maps(&root);
    let child = optimized
        .nodes
        .values()
        .find_map(|node| match &node.kind {
            NodeKind::SubCircuit(child) => Some(child),
            _ => None,
        })
        .unwrap();

    assert_eq!(
        child
            .nodes
            .values()
            .filter(|node| matches!(node.kind, NodeKind::Counter8))
            .count(),
        1
    );
    assert_eq!(child.nodes.len(), 11, "2 inputs + Counter8 + 8 outputs");
    assert_eq!(child.wires.len(), 10);

    let mut runner = FlatRunner::new(FlattenedCircuit::compile(&optimized));
    runner.set_input(node_map[&reset], 0, true);
    runner.tick();
    runner.set_input(node_map[&reset], 0, false);
    for expected in 1..=3_u8 {
        runner.set_input(node_map[&increment], 0, true);
        runner.tick();
        runner.set_input(node_map[&increment], 0, false);
        runner.tick();
        let value = root_outputs
            .iter()
            .enumerate()
            .fold(0_u8, |value, (bit, output)| {
                value | (u8::from(runner.output(node_map[output], 0)) << bit)
            });
        assert_eq!(value, expected);
    }
}

#[test]
fn near_match_is_not_miscompiled_as_a_counter() {
    let mut malformed = handmade_counter8();
    let stage = malformed
        .nodes
        .values_mut()
        .find_map(|node| match &mut node.kind {
            NodeKind::SubCircuit(child) => Some(child),
            _ => None,
        })
        .unwrap();
    let gate = stage
        .nodes
        .values_mut()
        .find(|node| matches!(node.kind, NodeKind::And))
        .unwrap();
    gate.kind = NodeKind::Or;

    assert!(logi_sim::compiler::sequential_cells::lower_handmade_counter8(&malformed).is_none());
}

#[test]
fn half_full_and_ripple_adders_lower_to_one_native_instruction() {
    let half =
        logi_sim::compiler::combinational_cells::lower_handmade_adder(&handmade_half_adder())
            .expect("half adder should match");
    assert!(half.nodes.values().any(|node| matches!(
        node.kind,
        NodeKind::Adder {
            bits: 1,
            carry_in: false
        }
    )));

    let full =
        logi_sim::compiler::combinational_cells::lower_handmade_adder(&handmade_full_adder())
            .expect("full adder should match");
    assert!(full.nodes.values().any(|node| matches!(
        node.kind,
        NodeKind::Adder {
            bits: 1,
            carry_in: true
        }
    )));

    let composed =
        logi_sim::compiler::combinational_cells::lower_handmade_adder(&composed_full_adder())
            .expect("two half adders plus OR should match");
    assert!(composed.nodes.values().any(|node| matches!(
        node.kind,
        NodeKind::Adder {
            bits: 1,
            carry_in: true
        }
    )));

    let ripple = handmade_ripple_adder(8);
    let lowered = logi_sim::compiler::combinational_cells::lower_handmade_adder(&ripple)
        .expect("8-bit ripple adder should match");
    assert!(lowered.nodes.values().any(|node| matches!(
        node.kind,
        NodeKind::Adder {
            bits: 8,
            carry_in: true
        }
    )));
    let before = FlattenedCircuit::compile(&ripple).executable_nodes.len();
    let after = FlattenedCircuit::compile(&lowered).executable_nodes.len();
    assert_eq!(before, 40);
    assert_eq!(after, 1);
}

#[test]
fn chained_counter_bytes_lower_to_counter64() {
    let handmade = handmade_counter64();
    let lowered = logi_sim::compiler::sequential_cells::lower_handmade_counter(&handmade)
        .expect("64-bit counter should match");
    assert!(
        lowered
            .nodes
            .values()
            .any(|node| matches!(node.kind, NodeKind::Counter64))
    );
    assert_eq!(lowered.nodes.len(), 67);
    assert_eq!(lowered.wires.len(), 66);
}

#[test]
fn native_adder_and_counter64_execute_on_every_representation() {
    let (adder_circuit, adder_inputs, adder_outputs) = native_adder(8);
    exercise_adder(
        NaiveRunner::new(adder_circuit.clone()),
        &adder_inputs,
        &adder_outputs,
    );
    exercise_adder(
        FlatRunner::new(FlattenedCircuit::compile(&adder_circuit)),
        &adder_inputs,
        &adder_outputs,
    );
    exercise_adder(
        EventDrivenRunner::new(EventDrivenCircuit::compile(&adder_circuit)),
        &adder_inputs,
        &adder_outputs,
    );
    exercise_adder(
        SimdRunner::new(BitPackedCircuit::compile(&adder_circuit)),
        &adder_inputs,
        &adder_outputs,
    );

    let (counter_circuit, increment, reset, outputs) = native_counter64();
    exercise_counter(
        NaiveRunner::new(counter_circuit.clone()),
        increment,
        reset,
        &outputs,
    );
    exercise_counter(
        FlatRunner::new(FlattenedCircuit::compile(&counter_circuit)),
        increment,
        reset,
        &outputs,
    );
    exercise_counter(
        EventDrivenRunner::new(EventDrivenCircuit::compile(&counter_circuit)),
        increment,
        reset,
        &outputs,
    );
    exercise_counter(
        SimdRunner::new(BitPackedCircuit::compile(&counter_circuit)),
        increment,
        reset,
        &outputs,
    );
}

#[test]
fn recursive_adder_fixture_gets_a_material_optimized_hot_path() {
    let raw = logi_sim::fixtures::recursive_16bit_adder().circuit;
    let optimized = Compiler::optimize(&raw);
    let raw_flat = FlattenedCircuit::compile(&raw);
    let optimized_flat = FlattenedCircuit::compile(&optimized);
    assert!(
        optimized_flat.executable_nodes.len() * 10 < raw_flat.executable_nodes.len(),
        "expected macro lowering to materially reduce executable nodes: {} -> {}",
        raw_flat.executable_nodes.len(),
        optimized_flat.executable_nodes.len()
    );
    assert_eq!(
        optimized_flat
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Adder { bits: 16, .. }))
            .count(),
        1
    );
}

fn exercise_adder(mut runner: impl Runner, inputs: &[NodeId], outputs: &[NodeId]) {
    let a = 0xA5_u16;
    let b = 0x5C_u16;
    for bit in 0..8 {
        runner.set_input(inputs[bit], 0, (a >> bit) & 1 != 0);
        runner.set_input(inputs[8 + bit], 0, (b >> bit) & 1 != 0);
    }
    runner.set_input(inputs[16], 0, true);
    runner.tick();
    let result = outputs
        .iter()
        .enumerate()
        .fold(0_u16, |value, (bit, node)| {
            value | (u16::from(runner.output(*node, 0)) << bit)
        });
    assert_eq!(result, a + b + 1);
}

fn exercise_counter(mut runner: impl Runner, increment: NodeId, reset: NodeId, outputs: &[NodeId]) {
    runner.set_input(reset, 0, true);
    runner.tick();
    runner.set_input(reset, 0, false);
    for expected in 1..=3_u64 {
        runner.set_input(increment, 0, true);
        runner.tick();
        runner.set_input(increment, 0, false);
        runner.tick();
        let value = outputs
            .iter()
            .enumerate()
            .fold(0_u64, |value, (bit, node)| {
                value | (u64::from(runner.output(*node, 0)) << bit)
            });
        assert_eq!(value, expected);
    }
}

fn native_adder(bits: usize) -> (Circuit, Vec<NodeId>, Vec<NodeId>) {
    let mut c = Circuit::new();
    let inputs: Vec<_> = (0..bits * 2 + 1).map(|_| c.input()).collect();
    let adder = c.add_node(
        NodeKind::Adder {
            bits: bits as u16,
            carry_in: true,
        },
        bits * 2 + 1,
        bits + 1,
    );
    for (pin, input) in inputs.iter().copied().enumerate() {
        c.connect((input, 0), (adder, pin as u8));
    }
    let outputs: Vec<_> = (0..=bits)
        .map(|pin| {
            let output = c.output();
            c.connect((adder, pin as u8), (output, 0));
            output
        })
        .collect();
    (c, inputs, outputs)
}

fn native_counter64() -> (Circuit, NodeId, NodeId, Vec<NodeId>) {
    let mut c = Circuit::new();
    let increment = c.input();
    let reset = c.input();
    let counter = c.add_node(NodeKind::Counter64, 2, 64);
    c.connect((increment, 0), (counter, 0));
    c.connect((reset, 0), (counter, 1));
    let outputs = (0..64)
        .map(|pin| {
            let output = c.output();
            c.connect((counter, pin), (output, 0));
            output
        })
        .collect();
    (c, increment, reset, outputs)
}

fn handmade_half_adder() -> Circuit {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let xor = c.xor();
    let and = c.and();
    let sum = c.output();
    let carry = c.output();
    for (source, sink, pin) in [(a, xor, 0), (b, xor, 1), (a, and, 0), (b, and, 1)] {
        c.connect((source, 0), (sink, pin));
    }
    c.connect((xor, 0), (sum, 0));
    c.connect((and, 0), (carry, 0));
    c
}

fn handmade_full_adder() -> Circuit {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let carry_in = c.input();
    let xor_ab = c.xor();
    let sum_gate = c.xor();
    let and_ab = c.and();
    let and_carry = c.and();
    let carry_gate = c.or();
    let sum = c.output();
    let carry = c.output();
    for (source, source_pin, sink, sink_pin) in [
        (a, 0, xor_ab, 0),
        (b, 0, xor_ab, 1),
        (xor_ab, 0, sum_gate, 0),
        (carry_in, 0, sum_gate, 1),
        (a, 0, and_ab, 0),
        (b, 0, and_ab, 1),
        (xor_ab, 0, and_carry, 0),
        (carry_in, 0, and_carry, 1),
        (and_ab, 0, carry_gate, 0),
        (and_carry, 0, carry_gate, 1),
        (sum_gate, 0, sum, 0),
        (carry_gate, 0, carry, 0),
    ] {
        c.connect((source, source_pin), (sink, sink_pin));
    }
    c
}

fn composed_full_adder() -> Circuit {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let carry_in = c.input();
    let first = c.add_subcircuit(handmade_half_adder());
    let second = c.add_subcircuit(handmade_half_adder());
    let carry_gate = c.or();
    let sum = c.output();
    let carry = c.output();
    for (source, source_pin, sink, sink_pin) in [
        (a, 0, first, 0),
        (b, 0, first, 1),
        (first, 0, second, 0),
        (carry_in, 0, second, 1),
        (first, 1, carry_gate, 0),
        (second, 1, carry_gate, 1),
        (second, 0, sum, 0),
        (carry_gate, 0, carry, 0),
    ] {
        c.connect((source, source_pin), (sink, sink_pin));
    }
    c
}

fn handmade_ripple_adder(bits: usize) -> Circuit {
    let mut c = Circuit::new();
    let a: Vec<_> = (0..bits).map(|_| c.input()).collect();
    let b: Vec<_> = (0..bits).map(|_| c.input()).collect();
    let carry_in = c.input();
    let mut carry = (carry_in, 0);
    let mut sums = Vec::new();
    for bit in 0..bits {
        let stage = c.add_subcircuit(handmade_full_adder());
        c.connect((a[bit], 0), (stage, 0));
        c.connect((b[bit], 0), (stage, 1));
        c.connect(carry, (stage, 2));
        let sum = c.output();
        c.connect((stage, 0), (sum, 0));
        sums.push(sum);
        carry = (stage, 1);
    }
    let carry_out = c.output();
    c.connect(carry, (carry_out, 0));
    c
}

fn handmade_counter64() -> Circuit {
    let mut c = Circuit::new();
    let increment = c.input();
    let reset = c.input();
    let mut ripple = (increment, 0);
    for _ in 0..8 {
        let byte = c.add_subcircuit(handmade_counter8());
        c.connect(ripple, (byte, 0));
        c.connect((reset, 0), (byte, 1));
        for bit in 0..8 {
            let output = c.output();
            c.connect((byte, bit), (output, 0));
        }
        ripple = (byte, 7);
    }
    c
}

fn handmade_counter8() -> Circuit {
    let mut circuit = Circuit::new();
    let increment = circuit.input();
    let reset = circuit.input();
    let mut ripple = increment;
    for _ in 0..8 {
        let stage = circuit.add_subcircuit(handmade_tff());
        circuit.connect((ripple, 0), (stage, 0));
        circuit.connect((reset, 0), (stage, 1));
        let output = circuit.output();
        circuit.connect((stage, 0), (output, 0));
        ripple = stage;
    }
    circuit
}

fn handmade_tff() -> Circuit {
    let mut circuit = Circuit::new();
    let clock = circuit.input();
    let reset = circuit.input();
    let s_m = circuit.and();
    let r_m = circuit.and();
    let rm_rst = circuit.or();
    let q_m = circuit.nor();
    let qn_m = circuit.nor();
    let not_clock = circuit.not();
    let s_s = circuit.and();
    let r_s = circuit.and();
    let rs_rst = circuit.or();
    let q_s = circuit.nor();
    let qn_s = circuit.nor();
    let q = circuit.output();
    let qn = circuit.output();
    for (source, source_pin, sink, sink_pin) in [
        (clock, 0, not_clock, 0),
        (qn_s, 0, s_m, 0),
        (clock, 0, s_m, 1),
        (q_s, 0, r_m, 0),
        (clock, 0, r_m, 1),
        (r_m, 0, rm_rst, 0),
        (reset, 0, rm_rst, 1),
        (rm_rst, 0, q_m, 0),
        (qn_m, 0, q_m, 1),
        (s_m, 0, qn_m, 0),
        (q_m, 0, qn_m, 1),
        (q_m, 0, s_s, 0),
        (not_clock, 0, s_s, 1),
        (qn_m, 0, r_s, 0),
        (not_clock, 0, r_s, 1),
        (r_s, 0, rs_rst, 0),
        (reset, 0, rs_rst, 1),
        (rs_rst, 0, q_s, 0),
        (qn_s, 0, q_s, 1),
        (s_s, 0, qn_s, 0),
        (q_s, 0, qn_s, 1),
        (q_s, 0, q, 0),
        (qn_s, 0, qn, 0),
    ] {
        circuit.connect((source, source_pin), (sink, sink_pin));
    }
    circuit
}
