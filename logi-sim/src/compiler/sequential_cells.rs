use std::collections::{HashMap, HashSet};

use crate::circuit::{Circuit, Node, NodeId, NodeKind};

pub fn lower_handmade_counter(circuit: &Circuit) -> Option<Circuit> {
    lower_counter64(circuit).or_else(|| lower_handmade_counter8(circuit))
}

fn lower_counter64(circuit: &Circuit) -> Option<Circuit> {
    let inputs: Vec<_> = circuit
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Input))
        .map(|(id, _)| id)
        .collect();
    let outputs: Vec<_> = circuit
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Output))
        .map(|(id, _)| id)
        .collect();
    let stages: Vec<_> = circuit
        .nodes
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::SubCircuit(child) if is_counter8_interface(child) => Some(id),
            _ => None,
        })
        .collect();
    if inputs.len() != 2
        || outputs.len() != 64
        || stages.len() != 8
        || circuit.nodes.len() != 74
        || circuit.wires.len() != 80
    {
        return None;
    }
    let reset = inputs.iter().copied().find(|&input| {
        stages
            .iter()
            .all(|&stage| driver(circuit, stage, 1) == Some((input, 0)))
    })?;
    let increment = inputs.iter().copied().find(|&input| input != reset)?;
    let mut stage = stages
        .iter()
        .copied()
        .find(|&candidate| driver(circuit, candidate, 0) == Some((increment, 0)))?;
    let mut ordered_outputs = Vec::with_capacity(64);
    let mut visited = HashSet::new();
    for byte in 0..8 {
        if !visited.insert(stage) {
            return None;
        }
        for bit in 0..8_u8 {
            ordered_outputs.push(
                outputs
                    .iter()
                    .copied()
                    .find(|&output| driver(circuit, output, 0) == Some((stage, bit)))?,
            );
        }
        let next = stages
            .iter()
            .copied()
            .find(|&candidate| driver(circuit, candidate, 0) == Some((stage, 7)));
        if byte < 7 {
            stage = next?;
        } else if next.is_some() {
            return None;
        }
    }
    if visited.len() != 8 {
        return None;
    }

    let mut lowered = Circuit::new();
    let mut map = HashMap::new();
    for input in inputs {
        map.insert(input, lowered.add_node(NodeKind::Input, 0, 1));
    }
    let counter = lowered.add_node(NodeKind::Counter64, 2, 64);
    for output in outputs {
        map.insert(output, lowered.add_node(NodeKind::Output, 1, 0));
    }
    lowered.connect((map[&increment], 0), (counter, 0));
    lowered.connect((map[&reset], 0), (counter, 1));
    for (bit, output) in ordered_outputs.into_iter().enumerate() {
        lowered.connect((counter, bit as u8), (map[&output], 0));
    }
    Some(lowered)
}

fn is_counter8_interface(circuit: &Circuit) -> bool {
    lower_handmade_counter8(circuit).is_some()
        || (circuit.nodes.len() == 11
            && circuit
                .nodes
                .values()
                .filter(|node| matches!(node.kind, NodeKind::Input))
                .count()
                == 2
            && circuit
                .nodes
                .values()
                .filter(|node| matches!(node.kind, NodeKind::Output))
                .count()
                == 8
            && circuit
                .nodes
                .values()
                .filter(|node| matches!(node.kind, NodeKind::Counter8))
                .count()
                == 1)
}

/// Lower an exact hand-built eight-stage ripple counter to the native stateful
/// byte cell. The surrounding Input/Output interface is retained verbatim.
pub fn lower_handmade_counter8(circuit: &Circuit) -> Option<Circuit> {
    let inputs: Vec<_> = circuit
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Input))
        .map(|(id, _)| id)
        .collect();
    let outputs: Vec<_> = circuit
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Output))
        .map(|(id, _)| id)
        .collect();
    let stages: Vec<_> = circuit
        .nodes
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::SubCircuit(child) if is_handmade_tff(child) => Some(id),
            _ => None,
        })
        .collect();

    if inputs.len() != 2
        || outputs.len() != 8
        || stages.len() != 8
        || circuit.nodes.len() != 18
        || circuit.wires.len() != 24
    {
        return None;
    }

    let reset = inputs.iter().copied().find(|&input| {
        stages
            .iter()
            .all(|&stage| driver(circuit, stage, 1) == Some((input, 0)))
    })?;
    let increment = inputs.iter().copied().find(|&input| input != reset)?;
    let mut stage = stages
        .iter()
        .copied()
        .find(|&candidate| driver(circuit, candidate, 0) == Some((increment, 0)))?;

    let mut ordered_outputs = Vec::with_capacity(8);
    let mut visited = HashSet::new();
    for bit in 0..8 {
        if !visited.insert(stage) {
            return None;
        }
        let output = outputs
            .iter()
            .copied()
            .find(|&candidate| driver(circuit, candidate, 0) == Some((stage, 0)))?;
        ordered_outputs.push(output);

        let next = stages
            .iter()
            .copied()
            .find(|&candidate| driver(circuit, candidate, 0) == Some((stage, 0)));
        if bit < 7 {
            stage = next?;
        } else if next.is_some() {
            return None;
        }
    }
    if visited.len() != stages.len() {
        return None;
    }

    let mut lowered = Circuit::new();
    let mut interface_map = HashMap::new();
    for &input in &inputs {
        interface_map.insert(input, lowered.add_node(NodeKind::Input, 0, 1));
    }
    let counter = lowered.add_node(NodeKind::Counter8, 2, 8);
    for &output in &outputs {
        interface_map.insert(output, lowered.add_node(NodeKind::Output, 1, 0));
    }
    lowered.connect((interface_map[&increment], 0), (counter, 0));
    lowered.connect((interface_map[&reset], 0), (counter, 1));
    for (bit, old_output) in ordered_outputs.into_iter().enumerate() {
        lowered.connect((counter, bit as u8), (interface_map[&old_output], 0));
    }
    Some(lowered)
}

fn driver(circuit: &Circuit, node: NodeId, pin: usize) -> Option<(NodeId, u8)> {
    let wire = circuit.nodes.get(node)?.inputs.get(pin)?.wire?;
    let source = &circuit.wires.get(wire)?.source;
    Some((source.node, source.pin))
}

fn is_handmade_tff(candidate: &Circuit) -> bool {
    let template = tff_template();
    isomorphic(candidate, &template)
}

/// Exact colored, pin-aware graph isomorphism. This is deliberately stricter
/// than a gate-count heuristic: an almost-counter must never be miscompiled.
pub(crate) fn isomorphic(candidate: &Circuit, template: &Circuit) -> bool {
    if candidate.nodes.len() != template.nodes.len()
        || candidate
            .wires
            .values()
            .map(|wire| wire.sinks.len())
            .sum::<usize>()
            != template
                .wires
                .values()
                .map(|wire| wire.sinks.len())
                .sum::<usize>()
    {
        return false;
    }
    let actual: Vec<_> = candidate.nodes.iter().collect();
    let expected: Vec<_> = template.nodes.iter().collect();
    let options: Vec<Vec<usize>> = expected
        .iter()
        .map(|(_, node)| {
            actual
                .iter()
                .enumerate()
                .filter(|(_, (_, other))| same_node_shape(node, other))
                .map(|(index, _)| index)
                .collect()
        })
        .collect();
    if options.iter().any(Vec::is_empty) {
        return false;
    }

    let mut order: Vec<_> = (0..expected.len()).collect();
    order.sort_by_key(|&index| options[index].len());
    let mut mapping = vec![usize::MAX; expected.len()];
    let mut used = vec![false; actual.len()];
    let mut context = SearchContext {
        order: &order,
        options: &options,
        mapping: &mut mapping,
        used: &mut used,
        actual: &actual,
        expected: &expected,
        candidate,
        template,
    };
    search_mapping(0, &mut context)
}

struct SearchContext<'a> {
    order: &'a [usize],
    options: &'a [Vec<usize>],
    mapping: &'a mut [usize],
    used: &'a mut [bool],
    actual: &'a [(NodeId, &'a Node)],
    expected: &'a [(NodeId, &'a Node)],
    candidate: &'a Circuit,
    template: &'a Circuit,
}

fn search_mapping(depth: usize, ctx: &mut SearchContext<'_>) -> bool {
    if depth == ctx.order.len() {
        let inverse: HashMap<_, _> = ctx
            .mapping
            .iter()
            .enumerate()
            .map(|(expected_index, &actual_index)| (ctx.actual[actual_index].0, expected_index))
            .collect();
        return edge_set(ctx.candidate, |node| inverse[&node])
            == edge_set(ctx.template, |node| {
                ctx.expected.iter().position(|(id, _)| *id == node).unwrap()
            });
    }
    let expected_index = ctx.order[depth];
    for &actual_index in &ctx.options[expected_index] {
        if ctx.used[actual_index] {
            continue;
        }
        ctx.mapping[expected_index] = actual_index;
        ctx.used[actual_index] = true;
        if search_mapping(depth + 1, ctx) {
            return true;
        }
        ctx.used[actual_index] = false;
    }
    false
}

fn edge_set(circuit: &Circuit, index: impl Fn(NodeId) -> usize) -> Vec<(usize, u8, usize, u8)> {
    let mut edges = Vec::new();
    for wire in circuit.wires.values() {
        for sink in &wire.sinks {
            edges.push((
                index(wire.source.node),
                wire.source.pin,
                index(sink.node),
                sink.pin,
            ));
        }
    }
    edges.sort_unstable();
    edges
}

fn same_node_shape(left: &Node, right: &Node) -> bool {
    std::mem::discriminant(&left.kind) == std::mem::discriminant(&right.kind)
        && left.inputs.len() == right.inputs.len()
        && left.outputs.len() == right.outputs.len()
}

fn tff_template() -> Circuit {
    let mut circuit = Circuit::new();
    let clock = circuit.add_node(NodeKind::Input, 0, 1);
    let reset = circuit.add_node(NodeKind::Input, 0, 1);
    let s_m = circuit.add_node(NodeKind::And, 2, 1);
    let r_m = circuit.add_node(NodeKind::And, 2, 1);
    let rm_rst = circuit.add_node(NodeKind::Or, 2, 1);
    let q_m = circuit.add_node(NodeKind::Nor, 2, 1);
    let qn_m = circuit.add_node(NodeKind::Nor, 2, 1);
    let not_clock = circuit.add_node(NodeKind::Not, 1, 1);
    let s_s = circuit.add_node(NodeKind::And, 2, 1);
    let r_s = circuit.add_node(NodeKind::And, 2, 1);
    let rs_rst = circuit.add_node(NodeKind::Or, 2, 1);
    let q_s = circuit.add_node(NodeKind::Nor, 2, 1);
    let qn_s = circuit.add_node(NodeKind::Nor, 2, 1);
    let q = circuit.add_node(NodeKind::Output, 1, 0);
    let qn = circuit.add_node(NodeKind::Output, 1, 0);

    let edges = [
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
    ];
    for (source, source_pin, sink, sink_pin) in edges {
        circuit.connect((source, source_pin), (sink, sink_pin));
    }
    circuit
}
