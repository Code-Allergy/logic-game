use crate::circuit::{Circuit, NodeId, NodeKind, WireId};
use smallvec::SmallVec;
use std::collections::HashMap;

/// A flattened node using direct 0-based array index lookups instead of SlotMap keys.
#[derive(Clone, Debug)]
pub struct FlatNode {
    pub kind: NodeKind,
    pub inputs: SmallVec<[usize; 4]>,  // Direct wire array indices
    pub outputs: SmallVec<[usize; 2]>, // Direct wire array indices
}

/// Level 1 Circuit representation: Flattened contiguous array layout.
#[derive(Clone, Debug)]
pub struct FlattenedCircuit {
    pub nodes: Vec<FlatNode>,
    pub wire_count: usize,
    pub node_to_idx: HashMap<NodeId, usize>,
    pub wire_to_idx: HashMap<WireId, usize>,
    pub idx_to_wire: Vec<WireId>,
    pub node_input_pins: HashMap<(NodeId, u8), usize>,
    pub node_output_pins: HashMap<(NodeId, u8), SmallVec<[usize; 2]>>,
}

impl FlattenedCircuit {
    /// Compiles a Level 0 `Circuit` into a Level 1 `FlattenedCircuit`.
    pub fn compile(circuit: &Circuit) -> Self {
        let mut wire_to_idx = HashMap::new();
        let mut idx_to_wire = Vec::new();

        for (wire_id, _) in &circuit.wires {
            let idx = idx_to_wire.len();
            wire_to_idx.insert(wire_id, idx);
            idx_to_wire.push(wire_id);
        }

        let mut node_to_idx = HashMap::new();
        let mut nodes = Vec::new();
        let mut node_input_pins = HashMap::new();
        let mut node_output_pins = HashMap::new();

        for (node_id, node) in &circuit.nodes {
            let n_idx = nodes.len();
            node_to_idx.insert(node_id, n_idx);

            let mut inputs = SmallVec::new();
            for (pin_idx, pin) in node.inputs.iter().enumerate() {
                if let Some(w) = pin.wire {
                    if let Some(&w_idx) = wire_to_idx.get(&w) {
                        inputs.push(w_idx);
                        node_input_pins.insert((node_id, pin_idx as u8), w_idx);
                    }
                }
            }

            let mut outputs = SmallVec::new();
            for (pin_idx, pin) in node.outputs.iter().enumerate() {
                let mut out_wires = SmallVec::new();
                for w in &pin.wires {
                    if let Some(&w_idx) = wire_to_idx.get(w) {
                        outputs.push(w_idx);
                        out_wires.push(w_idx);
                    }
                }
                node_output_pins.insert((node_id, pin_idx as u8), out_wires);
            }

            nodes.push(FlatNode {
                kind: node.kind.clone(),
                inputs,
                outputs,
            });
        }

        Self {
            nodes,
            wire_count: idx_to_wire.len(),
            node_to_idx,
            wire_to_idx,
            idx_to_wire,
            node_input_pins,
            node_output_pins,
        }
    }
}
