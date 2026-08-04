use std::collections::{HashMap, HashSet};

use crate::circuit::{Circuit, NodeId, NodeKind};

use super::sequential_cells::isomorphic;

pub fn lower_handmade_adder(circuit: &Circuit) -> Option<Circuit> {
    lower_ripple_adder(circuit)
        .or_else(|| lower_full_adder(circuit))
        .or_else(|| lower_half_adder(circuit))
}

fn lower_half_adder(circuit: &Circuit) -> Option<Circuit> {
    if !isomorphic(circuit, &half_adder_template()) {
        return None;
    }
    lower_single_adder(circuit, false, NodeKind::Xor, NodeKind::And)
}

fn lower_full_adder(circuit: &Circuit) -> Option<Circuit> {
    let ports = full_adder_ports(circuit)?;
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    rebuild_adder(
        circuit,
        &inputs,
        &outputs,
        &[outputs[ports.sum_output as usize]],
        outputs[ports.carry_output as usize],
        true,
    )
}

fn lower_single_adder(
    circuit: &Circuit,
    carry_in: bool,
    sum_driver: NodeKind,
    carry_driver: NodeKind,
) -> Option<Circuit> {
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    let sum = outputs.iter().copied().find(|&output| {
        driver(circuit, output, 0)
            .is_some_and(|(source, _)| same_kind(&circuit.nodes[source].kind, &sum_driver))
    })?;
    let carry = outputs.iter().copied().find(|&output| {
        driver(circuit, output, 0)
            .is_some_and(|(source, _)| same_kind(&circuit.nodes[source].kind, &carry_driver))
    })?;
    rebuild_adder(circuit, &inputs, &outputs, &[sum], carry, carry_in)
}

fn lower_ripple_adder(circuit: &Circuit) -> Option<Circuit> {
    if let Some(lowered) = lower_native_block_chain(circuit) {
        return Some(lowered);
    }
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    if inputs.len() < 3 || inputs.len() % 2 == 0 || outputs.len() * 2 != inputs.len() + 1 {
        return None;
    }
    let bits = outputs.len() - 1;
    let stages: Vec<_> = circuit
        .nodes
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::SubCircuit(child) => full_adder_ports(child).map(|ports| (id, ports)),
            _ => None,
        })
        .collect();
    if stages.len() != bits
        || circuit.nodes.len() != inputs.len() + outputs.len() + bits
        || circuit.wires.len() != bits * 4 + 1
    {
        return None;
    }

    let stage_ids: HashSet<_> = stages.iter().map(|(id, _)| *id).collect();
    let mut current = stages.iter().find_map(|(stage, ports)| {
        let source = driver(circuit, *stage, ports.carry_input)?;
        inputs
            .contains(&source.0)
            .then_some((*stage, *ports, source.0))
    })?;
    let carry_input = current.2;
    let mut sums = Vec::with_capacity(bits);
    let mut data_inputs = Vec::with_capacity(bits * 2);
    let mut visited = HashSet::new();
    let mut final_carry = None;

    for bit in 0..bits {
        let (stage, ports, _) = current;
        if !visited.insert(stage) {
            return None;
        }
        for pin in ports.data_inputs {
            let source = driver(circuit, stage, pin)?;
            if !inputs.contains(&source.0) {
                return None;
            }
            data_inputs.push(source.0);
        }
        let sum_output = outputs
            .iter()
            .copied()
            .find(|&output| driver(circuit, output, 0) == Some((stage, ports.sum_output)))?;
        sums.push(sum_output);

        let next = stages.iter().find_map(|(candidate, candidate_ports)| {
            (driver(circuit, *candidate, candidate_ports.carry_input)
                == Some((stage, ports.carry_output)))
            .then_some((*candidate, *candidate_ports, carry_input))
        });
        if bit + 1 < bits {
            current = next?;
        } else {
            if next.is_some() {
                return None;
            }
            final_carry = outputs
                .iter()
                .copied()
                .find(|&output| driver(circuit, output, 0) == Some((stage, ports.carry_output)));
        }
    }
    if visited != stage_ids {
        return None;
    }

    // Native input convention is all A bits, then all B bits, then carry-in.
    let mut native_inputs = Vec::with_capacity(bits * 2 + 1);
    native_inputs.extend(data_inputs.iter().step_by(2).copied());
    native_inputs.extend(data_inputs.iter().skip(1).step_by(2).copied());
    native_inputs.push(carry_input);
    rebuild_adder(circuit, &native_inputs, &outputs, &sums, final_carry?, true)
}

fn lower_native_block_chain(circuit: &Circuit) -> Option<Circuit> {
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    let stages: Vec<_> = circuit
        .nodes
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::SubCircuit(child) => native_adder_any_ports(child).map(|ports| (id, ports)),
            _ => None,
        })
        .collect();
    if stages.len() < 2 {
        return None;
    }
    let total_bits: usize = stages.iter().map(|(_, ports)| ports.bits).sum();
    if inputs.len() != total_bits * 2 + 1
        || outputs.len() != total_bits + 1
        || circuit.nodes.len() != inputs.len() + outputs.len() + stages.len()
        || circuit.wires.len() != total_bits * 3 + stages.len() + 1
    {
        return None;
    }
    let mut current = stages.iter().find_map(|(stage, ports)| {
        let source = driver(circuit, *stage, ports.carry_input)?;
        inputs
            .contains(&source.0)
            .then_some((*stage, ports, source.0))
    })?;
    let carry_input = current.2;
    let mut visited = HashSet::new();
    let mut a_inputs = Vec::with_capacity(total_bits);
    let mut b_inputs = Vec::with_capacity(total_bits);
    let mut sums = Vec::with_capacity(total_bits);
    let final_carry = loop {
        let (stage, ports, _) = current;
        if !visited.insert(stage) {
            return None;
        }
        for &pin in &ports.a_inputs {
            let source = driver(circuit, stage, pin)?;
            inputs.contains(&source.0).then_some(())?;
            a_inputs.push(source.0);
        }
        for &pin in &ports.b_inputs {
            let source = driver(circuit, stage, pin)?;
            inputs.contains(&source.0).then_some(())?;
            b_inputs.push(source.0);
        }
        for &sum_pin in &ports.sum_outputs {
            sums.push(
                outputs
                    .iter()
                    .copied()
                    .find(|&output| driver(circuit, output, 0) == Some((stage, sum_pin)))?,
            );
        }
        let next = stages.iter().find_map(|(candidate, candidate_ports)| {
            (driver(circuit, *candidate, candidate_ports.carry_input)
                == Some((stage, ports.carry_output)))
            .then_some((*candidate, candidate_ports, carry_input))
        });
        if let Some(next) = next {
            current = next;
        } else {
            break outputs
                .iter()
                .copied()
                .find(|&output| driver(circuit, output, 0) == Some((stage, ports.carry_output)));
        }
    };
    if visited.len() != stages.len() {
        return None;
    }
    let mut native_inputs = a_inputs;
    native_inputs.extend(b_inputs);
    native_inputs.push(carry_input);
    rebuild_adder(circuit, &native_inputs, &outputs, &sums, final_carry?, true)
}

#[derive(Clone, Copy)]
struct FullAdderPorts {
    carry_input: usize,
    data_inputs: [usize; 2],
    sum_output: u8,
    carry_output: u8,
}

fn full_adder_ports(circuit: &Circuit) -> Option<FullAdderPorts> {
    if let Some(native) = native_adder_ports(circuit, 1, true) {
        return Some(FullAdderPorts {
            carry_input: native.carry_input,
            data_inputs: [native.a_inputs[0], native.b_inputs[0]],
            sum_output: native.sum_outputs[0],
            carry_output: native.carry_output,
        });
    }
    if isomorphic(circuit, &full_adder_template()) {
        return gate_full_adder_ports(circuit);
    }
    composed_full_adder_ports(circuit)
}

fn gate_full_adder_ports(circuit: &Circuit) -> Option<FullAdderPorts> {
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    let carry_input = inputs.iter().position(|&input| {
        circuit.nodes[input].outputs[0].wires.iter().any(|&wire| {
            let sinks = &circuit.wires[wire].sinks;
            sinks.iter().any(|sink| {
                matches!(circuit.nodes[sink.node].kind, NodeKind::Xor)
                    && driver(circuit, sink.node, 0)
                        .or_else(|| driver(circuit, sink.node, 1))
                        .is_some_and(|(source, _)| {
                            matches!(circuit.nodes[source].kind, NodeKind::Xor)
                        })
            })
        })
    })?;
    let mut data = (0..3).filter(|&pin| pin != carry_input);
    let sum_output = outputs.iter().position(|&output| {
        driver(circuit, output, 0)
            .is_some_and(|(source, _)| matches!(circuit.nodes[source].kind, NodeKind::Xor))
    })? as u8;
    let carry_output = outputs.iter().position(|&output| {
        driver(circuit, output, 0)
            .is_some_and(|(source, _)| matches!(circuit.nodes[source].kind, NodeKind::Or))
    })? as u8;
    Some(FullAdderPorts {
        carry_input,
        data_inputs: [data.next()?, data.next()?],
        sum_output,
        carry_output,
    })
}

fn composed_full_adder_ports(circuit: &Circuit) -> Option<FullAdderPorts> {
    if !isomorphic(circuit, &composed_full_adder_template()) {
        return None;
    }
    let half_adders: Vec<_> = circuit
        .nodes
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::SubCircuit(child) => half_adder_ports(child).map(|ports| (id, ports)),
            _ => None,
        })
        .collect();
    if half_adders.len() != 2 {
        return None;
    }
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    let carry_output = outputs.iter().position(|&output| {
        driver(circuit, output, 0)
            .is_some_and(|(source, _)| matches!(circuit.nodes[source].kind, NodeKind::Or))
    })? as u8;
    let sum_output = outputs.iter().position(|&output| {
        driver(circuit, output, 0).is_some_and(|(source, pin)| {
            half_adders
                .iter()
                .any(|(stage, ports)| *stage == source && ports.sum == pin)
        })
    })? as u8;
    let (sum_stage, _) = driver(circuit, outputs[sum_output as usize], 0)?;
    let carry_input_node = inputs.iter().copied().find(|&input| {
        circuit.nodes[sum_stage].inputs.iter().any(|pin| {
            pin.wire
                .and_then(|wire| circuit.wires.get(wire))
                .is_some_and(|wire| wire.source.node == input)
        })
    })?;
    let carry_input = inputs.iter().position(|&input| input == carry_input_node)?;
    let mut data = (0..3).filter(|&pin| pin != carry_input);
    Some(FullAdderPorts {
        carry_input,
        data_inputs: [data.next()?, data.next()?],
        sum_output,
        carry_output,
    })
}

#[derive(Clone, Copy)]
struct HalfAdderPorts {
    sum: u8,
}

fn half_adder_ports(circuit: &Circuit) -> Option<HalfAdderPorts> {
    if let Some(native) = native_adder_ports(circuit, 1, false) {
        return Some(HalfAdderPorts {
            sum: native.sum_outputs[0],
        });
    }
    if !isomorphic(circuit, &half_adder_template()) {
        return None;
    }
    let outputs = interface_nodes(circuit, false);
    Some(HalfAdderPorts {
        sum: outputs.iter().position(|&output| {
            driver(circuit, output, 0)
                .is_some_and(|(source, _)| matches!(circuit.nodes[source].kind, NodeKind::Xor))
        })? as u8,
    })
}

struct NativeAdderPorts {
    bits: usize,
    carry_input: usize,
    a_inputs: Vec<usize>,
    b_inputs: Vec<usize>,
    sum_outputs: Vec<u8>,
    carry_output: u8,
}

fn native_adder_ports(
    circuit: &Circuit,
    expected_bits: usize,
    expected_carry: bool,
) -> Option<NativeAdderPorts> {
    let inputs = interface_nodes(circuit, true);
    let outputs = interface_nodes(circuit, false);
    let (adder, bits, carry_in) = circuit
        .nodes
        .iter()
        .find_map(|(id, node)| match node.kind {
            NodeKind::Adder { bits, carry_in } => Some((id, usize::from(bits), carry_in)),
            _ => None,
        })?;
    if bits != expected_bits
        || carry_in != expected_carry
        || inputs.len() != bits * 2 + usize::from(carry_in)
        || outputs.len() != bits + 1
    {
        return None;
    }
    let input_port = |native_pin: u8| {
        inputs.iter().position(|&input| {
            circuit.nodes[input].outputs[0].wires.iter().any(|&wire| {
                circuit.wires[wire]
                    .sinks
                    .iter()
                    .any(|sink| sink.node == adder && sink.pin == native_pin)
            })
        })
    };
    let output_port = |native_pin: u8| {
        outputs
            .iter()
            .position(|&output| driver(circuit, output, 0) == Some((adder, native_pin)))
    };
    Some(NativeAdderPorts {
        bits,
        carry_input: if carry_in {
            input_port((bits * 2) as u8)?
        } else {
            usize::MAX
        },
        a_inputs: (0..bits)
            .map(|bit| input_port(bit as u8))
            .collect::<Option<_>>()?,
        b_inputs: (0..bits)
            .map(|bit| input_port((bits + bit) as u8))
            .collect::<Option<_>>()?,
        sum_outputs: (0..bits)
            .map(|bit| output_port(bit as u8).map(|pin| pin as u8))
            .collect::<Option<_>>()?,
        carry_output: output_port(bits as u8)? as u8,
    })
}

fn native_adder_any_ports(circuit: &Circuit) -> Option<NativeAdderPorts> {
    let (bits, carry_in) = circuit.nodes.values().find_map(|node| match node.kind {
        NodeKind::Adder { bits, carry_in } => Some((usize::from(bits), carry_in)),
        _ => None,
    })?;
    carry_in.then_some(())?;
    native_adder_ports(circuit, bits, true)
}

fn rebuild_adder(
    circuit: &Circuit,
    native_inputs: &[NodeId],
    outputs: &[NodeId],
    sums: &[NodeId],
    carry: NodeId,
    carry_in: bool,
) -> Option<Circuit> {
    let interface_inputs = interface_nodes(circuit, true);
    let mut lowered = Circuit::new();
    let mut map = HashMap::new();
    for input in interface_inputs {
        map.insert(input, lowered.add_node(NodeKind::Input, 0, 1));
    }
    let bits = sums.len();
    let adder = lowered.add_node(
        NodeKind::Adder {
            bits: bits.try_into().ok()?,
            carry_in,
        },
        bits * 2 + usize::from(carry_in),
        bits + 1,
    );
    for &output in outputs {
        map.insert(output, lowered.add_node(NodeKind::Output, 1, 0));
    }
    for (pin, input) in native_inputs.iter().copied().enumerate() {
        lowered.connect((map[&input], 0), (adder, pin as u8));
    }
    for (bit, output) in sums.iter().copied().enumerate() {
        lowered.connect((adder, bit as u8), (map[&output], 0));
    }
    lowered.connect((adder, bits as u8), (map[&carry], 0));
    Some(lowered)
}

fn interface_nodes(circuit: &Circuit, inputs: bool) -> Vec<NodeId> {
    circuit
        .nodes
        .iter()
        .filter(|(_, node)| {
            matches!(node.kind, NodeKind::Input) == inputs
                && matches!(node.kind, NodeKind::Input | NodeKind::Output)
        })
        .map(|(id, _)| id)
        .collect()
}

fn driver(circuit: &Circuit, node: NodeId, pin: usize) -> Option<(NodeId, u8)> {
    let wire = circuit.nodes.get(node)?.inputs.get(pin)?.wire?;
    let source = &circuit.wires.get(wire)?.source;
    Some((source.node, source.pin))
}

fn same_kind(left: &NodeKind, right: &NodeKind) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

fn half_adder_template() -> Circuit {
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

fn full_adder_template() -> Circuit {
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

fn composed_full_adder_template() -> Circuit {
    let mut c = Circuit::new();
    let a = c.input();
    let b = c.input();
    let carry_in = c.input();
    let first = c.add_subcircuit(half_adder_template());
    let second = c.add_subcircuit(half_adder_template());
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
