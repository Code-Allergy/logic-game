use crate::circuit::{Circuit, NodeId, NodeKind, WireId};
use std::collections::{HashSet, VecDeque};

/// Optimization Pass: Prunes dead nodes and wires not transitively connected to any `Output` node.
pub fn cull_dead_nodes(circuit: &Circuit) -> Circuit {
    let mut live_nodes = HashSet::<NodeId>::new();
    let mut live_wires = HashSet::<WireId>::new();
    let mut queue = VecDeque::<NodeId>::new();

    // Find all Output nodes as starting points for backward reachability
    for (node_id, node) in &circuit.nodes {
        if matches!(node.kind, NodeKind::Output) {
            live_nodes.insert(node_id);
            queue.push_back(node_id);
        }
    }

    // Traverse backwards from Output nodes
    while let Some(node_id) = queue.pop_front() {
        let node = &circuit.nodes[node_id];
        for input_pin in &node.inputs {
            if let Some(wire_id) = input_pin.wire {
                if live_wires.insert(wire_id) {
                    if let Some(wire) = circuit.wires.get(wire_id) {
                        let source_node = wire.source.node;
                        if live_nodes.insert(source_node) {
                            queue.push_back(source_node);
                        }
                    }
                }
            }
        }
    }

    // Rebuild optimized circuit containing only live nodes and wires
    let mut optimized = Circuit::new();
    let mut node_map = std::collections::HashMap::new();

    for (node_id, node) in &circuit.nodes {
        if live_nodes.contains(&node_id) {
            let new_node = optimized.add_node(node.kind.clone(), node.inputs.len(), node.outputs.len());
            node_map.insert(node_id, new_node);
        }
    }

    for (wire_id, wire) in &circuit.wires {
        if live_wires.contains(&wire_id) {
            if let (Some(&new_source), Some(&new_sink)) = (
                node_map.get(&wire.source.node),
                wire.sinks.first().and_then(|s| node_map.get(&s.node)),
            ) {
                let sink_pin = wire.sinks[0].pin;
                optimized.connect((new_source, wire.source.pin), (new_sink, sink_pin));
            }
        }
    }

    optimized
}
