use std::collections::HashMap;

use crate::circuit::{Circuit, InputRef, NodeId, NodeKind, WireId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CellOp {
    And,
    Or,
    Not,
    Xor,
    Nand,
    Nor,
    Xnor,
    Buffer,
    Mux,
    Demux,
    TriStateBuffer,
}

impl CellOp {
    fn from_kind(kind: &NodeKind) -> Option<Self> {
        Some(match kind {
            NodeKind::And => Self::And,
            NodeKind::Or => Self::Or,
            NodeKind::Not => Self::Not,
            NodeKind::Xor => Self::Xor,
            NodeKind::Nand => Self::Nand,
            NodeKind::Nor => Self::Nor,
            NodeKind::Xnor => Self::Xnor,
            NodeKind::Buffer => Self::Buffer,
            NodeKind::Mux => Self::Mux,
            NodeKind::Demux => Self::Demux,
            NodeKind::TriStateBuffer => Self::TriStateBuffer,
            NodeKind::Input
            | NodeKind::Output
            | NodeKind::Clock { .. }
            | NodeKind::Counter8
            | NodeKind::Counter64
            | NodeKind::Adder { .. }
            | NodeKind::SubCircuit(_) => return None,
        })
    }

    fn is_commutative(self) -> bool {
        matches!(
            self,
            Self::And | Self::Or | Self::Xor | Self::Nand | Self::Nor | Self::Xnor
        )
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct CellKey {
    op: CellOp,
    inputs: Vec<Option<(NodeId, u8)>>,
}

/// Merge stateless cells that compute the same function from the same drivers.
///
/// Every original wire is rebuilt and mapped independently. A surviving cell
/// therefore drives all output-wire aliases of the cells it replaces, keeping
/// wire inspection exact while reducing the number of executed gate instructions.
pub fn merge_common_cells_with_map(
    circuit: &Circuit,
) -> (Circuit, HashMap<NodeId, NodeId>, HashMap<WireId, WireId>) {
    let mut representatives = HashMap::<CellKey, NodeId>::new();
    let mut representative_by_node = HashMap::<NodeId, NodeId>::new();

    for (node_id, node) in &circuit.nodes {
        let Some(op) = CellOp::from_kind(&node.kind) else {
            representative_by_node.insert(node_id, node_id);
            continue;
        };

        let mut inputs: Vec<_> = node
            .inputs
            .iter()
            .map(|pin| {
                pin.wire.and_then(|wire_id| {
                    circuit.wires.get(wire_id).map(|wire| {
                        let source = representative_by_node
                            .get(&wire.source.node)
                            .copied()
                            .unwrap_or(wire.source.node);
                        (source, wire.source.pin)
                    })
                })
            })
            .collect();
        if op.is_commutative() {
            inputs.sort_unstable();
        }

        let key = CellKey { op, inputs };
        let representative = *representatives.entry(key).or_insert(node_id);
        representative_by_node.insert(node_id, representative);
    }

    let mut optimized = Circuit::new();
    let mut representative_to_new = HashMap::<NodeId, NodeId>::new();
    for (node_id, node) in &circuit.nodes {
        if representative_by_node[&node_id] == node_id {
            let new_node =
                optimized.add_node(node.kind.clone(), node.inputs.len(), node.outputs.len());
            representative_to_new.insert(node_id, new_node);
        }
    }

    let node_map: HashMap<_, _> = representative_by_node
        .iter()
        .map(|(&old, representative)| (old, representative_to_new[representative]))
        .collect();
    let mut wire_map = HashMap::new();
    let mut wire_by_source = HashMap::<(NodeId, u8), WireId>::new();

    for (wire_id, wire) in &circuit.wires {
        let new_source = node_map[&wire.source.node];
        let source = (new_source, wire.source.pin);
        for sink in &wire.sinks {
            let new_sink = node_map[&sink.node];
            let new_wire = if let Some(&existing) = wire_by_source.get(&source) {
                optimized.wires[existing].sinks.push(InputRef {
                    node: new_sink,
                    pin: sink.pin,
                });
                optimized.nodes[new_sink].inputs[sink.pin as usize].wire = Some(existing);
                existing
            } else {
                let created = optimized.connect(source, (new_sink, sink.pin));
                wire_by_source.insert(source, created);
                created
            };
            wire_map.insert(wire_id, new_wire);
        }
    }

    (optimized, node_map, wire_map)
}
