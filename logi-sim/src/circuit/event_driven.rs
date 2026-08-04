use crate::circuit::{Circuit, FlattenedCircuit, NodeId, NodeKind, WireId};
use smallvec::SmallVec;
use std::collections::HashMap;

/// A node in the event-driven netlist.
#[derive(Clone, Debug)]
pub struct EventNode {
    pub kind: NodeKind,
    pub inputs: SmallVec<[usize; 4]>,  // Direct wire indices
    pub outputs: SmallVec<[usize; 2]>, // Direct wire indices
    pub output_pins: SmallVec<[SmallVec<[usize; 2]>; 2]>,
}

/// Level 2 Event-Driven Netlist Representation.
#[derive(Clone, Debug)]
pub struct EventDrivenCircuit {
    pub nodes: Vec<EventNode>,
    pub wire_count: usize,
    /// Wire listeners: `wire_listeners[wire_idx]` -> list of `node_idx` that consume this wire.
    pub wire_listeners: Vec<SmallVec<[usize; 4]>>,
    pub node_to_idx: HashMap<NodeId, usize>,
    pub wire_to_idx: HashMap<WireId, usize>,
    pub idx_to_wire: Vec<WireId>,
    pub node_input_pins: HashMap<(NodeId, u8), usize>,
    pub node_output_pins: HashMap<(NodeId, u8), SmallVec<[usize; 2]>>,
}

impl EventDrivenCircuit {
    /// Compiles a Level 1 `FlattenedCircuit` into a Level 2 `EventDrivenCircuit`.
    pub fn compile_from_flat(flat: &FlattenedCircuit) -> Self {
        let wire_count = flat.wire_count;
        let mut wire_listeners: Vec<SmallVec<[usize; 4]>> = vec![SmallVec::new(); wire_count];

        let mut nodes = Vec::with_capacity(flat.nodes.len());
        for (n_idx, f_node) in flat.nodes.iter().enumerate() {
            nodes.push(EventNode {
                kind: f_node.kind.clone(),
                inputs: f_node.inputs.clone(),
                outputs: f_node.outputs.clone(),
                output_pins: f_node.output_pins.clone(),
            });

            // Register this node as a listener for each of its input wires
            for &w_idx in &f_node.inputs {
                if w_idx < wire_count {
                    wire_listeners[w_idx].push(n_idx);
                }
            }
        }

        Self {
            nodes,
            wire_count,
            wire_listeners,
            node_to_idx: flat.node_to_idx.clone(),
            wire_to_idx: flat.wire_to_idx.clone(),
            idx_to_wire: flat.idx_to_wire.clone(),
            node_input_pins: flat.node_input_pins.clone(),
            node_output_pins: flat.node_output_pins.clone(),
        }
    }

    /// Convenience compiler directly from Level 0 `Circuit`.
    pub fn compile(circuit: &Circuit) -> Self {
        let flat = FlattenedCircuit::compile(circuit);
        Self::compile_from_flat(&flat)
    }
}
