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

struct FlattenCtx {
    is_root: bool,
    nodes: Vec<FlatNode>,
    wire_count: usize,
    node_to_idx: HashMap<NodeId, usize>,
    wire_to_idx: HashMap<WireId, usize>,
    idx_to_wire: Vec<WireId>,
    node_input_pins: HashMap<(NodeId, u8), usize>,
    node_output_pins: HashMap<(NodeId, u8), SmallVec<[usize; 2]>>,
}

impl FlattenCtx {
    fn new() -> Self {
        Self {
            is_root: true,
            nodes: Vec::new(),
            wire_count: 0,
            node_to_idx: HashMap::new(),
            wire_to_idx: HashMap::new(),
            idx_to_wire: Vec::new(),
            node_input_pins: HashMap::new(),
            node_output_pins: HashMap::new(),
        }
    }

    fn inline_circuit(
        &mut self,
        circuit: &Circuit,
        external_inputs: &[usize],
        external_outputs: &[SmallVec<[usize; 2]>],
    ) {
        let mut local_wire_map: HashMap<WireId, usize> = HashMap::new();

        for (wire_id, _) in &circuit.wires {
            let global_w_idx = self.wire_count;
            self.wire_count += 1;
            local_wire_map.insert(wire_id, global_w_idx);

            if self.is_root {
                self.wire_to_idx.insert(wire_id, global_w_idx);
                self.idx_to_wire.push(wire_id);
            }
        }

        let mut child_input_nodes: Vec<(NodeId, String)> = circuit
            .nodes
            .iter()
            .filter_map(|(id, n)| {
                if let NodeKind::Input { name } = &n.kind {
                    Some((id, name.clone()))
                } else {
                    None
                }
            })
            .collect();
        child_input_nodes.sort_by(|a, b| a.1.cmp(&b.1));
        let child_input_nodes: Vec<NodeId> = child_input_nodes.into_iter().map(|(id, _)| id).collect();

        let mut child_output_nodes: Vec<(NodeId, String)> = circuit
            .nodes
            .iter()
            .filter_map(|(id, n)| {
                if let NodeKind::Output { name } = &n.kind {
                    Some((id, name.clone()))
                } else {
                    None
                }
            })
            .collect();
        child_output_nodes.sort_by(|a, b| a.1.cmp(&b.1));
        let child_output_nodes: Vec<NodeId> = child_output_nodes.into_iter().map(|(id, _)| id).collect();

        // 1. Process Input nodes first to map external input wires
        for (node_id, node) in &circuit.nodes {
            if matches!(node.kind, NodeKind::Input { .. }) {
                if !self.is_root {
                    if let Some(input_idx) = child_input_nodes.iter().position(|&id| id == node_id)
                    {
                        if let Some(&ext_w_idx) = external_inputs.get(input_idx) {
                            for pin in &node.outputs {
                                for w in &pin.wires {
                                    local_wire_map.insert(*w, ext_w_idx);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 1.5 Process Output nodes first to map external output wires
        for (node_id, node) in &circuit.nodes {
            if matches!(node.kind, NodeKind::Output { .. }) {
                if !self.is_root {
                    if let Some(output_idx) =
                        child_output_nodes.iter().position(|&id| id == node_id)
                    {
                        if let Some(ext_out_wires) = external_outputs.get(output_idx) {
                            for pin in &node.inputs {
                                if let Some(w) = pin.wire {
                                    for &ext_w_idx in ext_out_wires {
                                        local_wire_map.insert(w, ext_w_idx);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Process all other nodes
        for (node_id, node) in &circuit.nodes {
            match &node.kind {
                NodeKind::Input { .. } => {
                    if self.is_root {
                        let n_idx = self.nodes.len();
                        self.node_to_idx.insert(node_id, n_idx);

                        let mut outputs = SmallVec::new();
                        for (pin_idx, pin) in node.outputs.iter().enumerate() {
                            let mut out_wires = SmallVec::new();
                            for w in &pin.wires {
                                if let Some(&w_idx) = local_wire_map.get(w) {
                                    outputs.push(w_idx);
                                    out_wires.push(w_idx);
                                }
                            }
                            self.node_output_pins
                                .insert((node_id, pin_idx as u8), out_wires);
                        }

                        self.nodes.push(FlatNode {
                            kind: node.kind.clone(),
                            inputs: SmallVec::new(),
                            outputs,
                        });
                    }
                }
                NodeKind::Output { .. } => {
                    if self.is_root {
                        let n_idx = self.nodes.len();
                        self.node_to_idx.insert(node_id, n_idx);

                        let mut inputs = SmallVec::new();
                        for (pin_idx, pin) in node.inputs.iter().enumerate() {
                            if let Some(w) = pin.wire {
                                if let Some(&w_idx) = local_wire_map.get(&w) {
                                    inputs.push(w_idx);
                                    self.node_input_pins.insert((node_id, pin_idx as u8), w_idx);
                                }
                            }
                        }

                        self.nodes.push(FlatNode {
                            kind: node.kind.clone(),
                            inputs,
                            outputs: SmallVec::new(),
                        });
                    }
                }

                NodeKind::SubCircuit(sub) => {
                    let mut sub_ext_inputs: SmallVec<[usize; 4]> = SmallVec::new();

                    for (pin_idx, pin) in node.inputs.iter().enumerate() {
                        if let Some(w) = pin.wire {
                            if let Some(&w_idx) = local_wire_map.get(&w) {
                                sub_ext_inputs.push(w_idx);
                                if self.is_root {
                                    self.node_input_pins.insert((node_id, pin_idx as u8), w_idx);
                                }
                            }
                        }
                    }

                    let mut sub_ext_outputs: SmallVec<[SmallVec<[usize; 2]>; 2]> = SmallVec::new();

                    for (pin_idx, pin) in node.outputs.iter().enumerate() {
                        let mut out_wires = SmallVec::new();
                        for w in &pin.wires {
                            if let Some(&w_idx) = local_wire_map.get(w) {
                                out_wires.push(w_idx);
                            }
                        }
                        if self.is_root {
                            self.node_output_pins
                                .insert((node_id, pin_idx as u8), out_wires.clone());
                        }
                        sub_ext_outputs.push(out_wires);
                    }

                    let prev_is_root = self.is_root;
                    self.is_root = false;
                    self.inline_circuit(sub, &sub_ext_inputs, &sub_ext_outputs);
                    self.is_root = prev_is_root;
                }
                _ => {
                    let n_idx = self.nodes.len();
                    if self.is_root {
                        self.node_to_idx.insert(node_id, n_idx);
                    }

                    let mut inputs = SmallVec::new();
                    for (pin_idx, pin) in node.inputs.iter().enumerate() {
                        if let Some(w) = pin.wire {
                            if let Some(&w_idx) = local_wire_map.get(&w) {
                                inputs.push(w_idx);
                                if self.is_root {
                                    self.node_input_pins.insert((node_id, pin_idx as u8), w_idx);
                                }
                            }
                        }
                    }

                    let mut outputs = SmallVec::new();
                    for (pin_idx, pin) in node.outputs.iter().enumerate() {
                        let mut out_wires = SmallVec::new();
                        for w in &pin.wires {
                            if let Some(&w_idx) = local_wire_map.get(w) {
                                outputs.push(w_idx);
                                out_wires.push(w_idx);
                            }
                        }
                        if self.is_root {
                            self.node_output_pins
                                .insert((node_id, pin_idx as u8), out_wires);
                        }
                    }

                    self.nodes.push(FlatNode {
                        kind: node.kind.clone(),
                        inputs,
                        outputs,
                    });
                }
            }
        }
    }
}

impl FlattenedCircuit {
    /// Compiles a Level 0 `Circuit` into a Level 1 `FlattenedCircuit`.
    pub fn compile(circuit: &Circuit) -> Self {
        let mut ctx = FlattenCtx::new();
        ctx.inline_circuit(circuit, &[], &[]);

        Self {
            nodes: ctx.nodes,
            wire_count: ctx.wire_count,
            node_to_idx: ctx.node_to_idx,
            wire_to_idx: ctx.wire_to_idx,
            idx_to_wire: ctx.idx_to_wire,
            node_input_pins: ctx.node_input_pins,
            node_output_pins: ctx.node_output_pins,
        }
    }
}
