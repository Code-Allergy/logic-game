use crate::circuit::{Circuit, NodeId, NodeKind, WireId};
use std::collections::{HashSet, VecDeque};

/// Optimization Pass: Prunes dead nodes and wires not transitively connected to any `Output` node.
pub fn cull_dead_nodes_with_map(
    circuit: &Circuit,
) -> (
    Circuit,
    std::collections::HashMap<NodeId, NodeId>,
    std::collections::HashMap<WireId, WireId>,
) {
    let mut live_nodes = HashSet::<NodeId>::new();
    let mut live_wires = HashSet::<WireId>::new();
    let mut queue = VecDeque::<NodeId>::new();

    // Find all output/input nodes as starting points for reachability and interface
    // preservation, including bus versions so multi-bit wrappers stay alive.
    for (node_id, node) in &circuit.nodes {
        if matches!(
            node.kind,
            NodeKind::Input
                | NodeKind::BusInput { .. }
                | NodeKind::Output
                | NodeKind::BusOutput { .. }
        ) {
            live_nodes.insert(node_id);
            if matches!(node.kind, NodeKind::Output | NodeKind::BusOutput { .. }) {
                queue.push_back(node_id);
            }
        }
    }

    // Traverse backwards from Output nodes
    while let Some(node_id) = queue.pop_front() {
        let node = &circuit.nodes[node_id];
        for input_pin in &node.inputs {
            if let Some(wire_id) = input_pin.wire
                && live_wires.insert(wire_id)
                && let Some(wire) = circuit.wires.get(wire_id)
            {
                let source_node = wire.source.node;
                if live_nodes.insert(source_node) {
                    queue.push_back(source_node);
                }
            }
        }
    }

    // Rebuild optimized circuit containing only live nodes and wires
    let mut optimized = Circuit::new();
    let mut node_map = std::collections::HashMap::new();
    let mut wire_map = std::collections::HashMap::new();

    for (node_id, node) in &circuit.nodes {
        if live_nodes.contains(&node_id) {
            let new_node =
                optimized.add_node(node.kind.clone(), node.inputs.len(), node.outputs.len());
            node_map.insert(node_id, new_node);
        }
    }

    for (wire_id, wire) in &circuit.wires {
        if live_wires.contains(&wire_id)
            && let Some(&new_source) = node_map.get(&wire.source.node)
        {
            let mut rebuilt_wire = None;
            for sink in &wire.sinks {
                if let Some(&new_sink) = node_map.get(&sink.node) {
                    if let Some(new_wire_id) = rebuilt_wire {
                        optimized.wires[new_wire_id]
                            .sinks
                            .push(crate::circuit::InputRef {
                                node: new_sink,
                                pin: sink.pin,
                            });
                        optimized.nodes[new_sink].inputs[sink.pin as usize].wire =
                            Some(new_wire_id);
                    } else {
                        rebuilt_wire = Some(
                            optimized.connect((new_source, wire.source.pin), (new_sink, sink.pin)),
                        );
                    }
                }
            }
            if let Some(new_wire_id) = rebuilt_wire {
                wire_map.insert(wire_id, new_wire_id);
            }
        }
    }

    (optimized, node_map, wire_map)
}

pub fn cull_dead_nodes(circuit: &Circuit) -> Circuit {
    cull_dead_nodes_with_map(circuit).0
}
