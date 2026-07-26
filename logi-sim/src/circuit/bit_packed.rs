use crate::circuit::{Circuit, FlattenedCircuit, NodeId, NodeKind, WireId};
use smallvec::SmallVec;
use std::collections::{HashMap, VecDeque};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdOp {
    And = 0,
    Or = 1,
    Not = 2,
    Xor = 3,
}

#[derive(Clone, Debug)]
pub struct SimdGateInstruction {
    pub op: SimdOp,
    pub in0: u32,
    pub in1: u32,
    pub outputs: SmallVec<[u32; 2]>,
}


/// Level 3 Bit-Packed SIMD Circuit Representation.
#[derive(Clone, Debug)]
pub struct BitPackedCircuit {
    pub instructions: Vec<SimdGateInstruction>,
    pub wire_count: usize,
    pub node_to_idx: HashMap<NodeId, usize>,
    pub wire_to_idx: HashMap<WireId, usize>,
    pub idx_to_wire: Vec<WireId>,
    pub node_input_pins: HashMap<(NodeId, u8), usize>,
    pub node_output_pins: HashMap<(NodeId, u8), SmallVec<[usize; 2]>>,
}

impl BitPackedCircuit {
    /// Compiles a Level 1 `FlattenedCircuit` into a Level 3 `BitPackedCircuit` with topological instruction ordering.
    pub fn compile_from_flat(flat: &FlattenedCircuit) -> Self {
        let node_count = flat.nodes.len();
        let wire_count = flat.wire_count;

        // Compute wire listeners and in-degrees for topological sort
        let mut wire_listeners: Vec<Vec<usize>> = vec![Vec::new(); wire_count];
        let mut in_degree = vec![0usize; node_count];

        for (n_idx, f_node) in flat.nodes.iter().enumerate() {
            in_degree[n_idx] = f_node.inputs.len();
            for &w_idx in &f_node.inputs {
                if w_idx < wire_count {
                    wire_listeners[w_idx].push(n_idx);
                }
            }
        }

        // Topological queue: start with Input nodes and 0-input nodes
        let mut queue = VecDeque::new();
        let mut visited = vec![false; node_count];

        for (n_idx, f_node) in flat.nodes.iter().enumerate() {
            if matches!(f_node.kind, NodeKind::Input) || f_node.inputs.is_empty() {
                queue.push_back(n_idx);
                visited[n_idx] = true;
            }
        }

        let mut topo_order = Vec::with_capacity(node_count);

        while let Some(n_idx) = queue.pop_front() {
            topo_order.push(n_idx);

            let f_node = &flat.nodes[n_idx];
            for &out_w_idx in &f_node.outputs {
                for &listener_idx in &wire_listeners[out_w_idx] {
                    if in_degree[listener_idx] > 0 {
                        in_degree[listener_idx] -= 1;
                        if in_degree[listener_idx] == 0 && !visited[listener_idx] {
                            visited[listener_idx] = true;
                            queue.push_back(listener_idx);
                        }
                    }
                }
            }
        }

        // Append any remaining unvisited nodes (e.g. feedback loops in sequential latches)
        for n_idx in 0..node_count {
            if !visited[n_idx] {
                topo_order.push(n_idx);
                visited[n_idx] = true;
            }
        }

        // Convert topological nodes to compact SIMD instructions
        let mut instructions = Vec::with_capacity(node_count);

        for &n_idx in &topo_order {
            let f_node = &flat.nodes[n_idx];
            let op = match f_node.kind {
                NodeKind::Input | NodeKind::Output => continue,
                NodeKind::And => SimdOp::And,
                NodeKind::Or => SimdOp::Or,
                NodeKind::Not => SimdOp::Not,
                NodeKind::Xor => SimdOp::Xor,
            };

            let in0 = f_node.inputs.first().copied().unwrap_or(0) as u32;
            let in1 = f_node.inputs.get(1).copied().unwrap_or(0) as u32;
            let outputs = f_node.outputs.iter().map(|&w| w as u32).collect();

            instructions.push(SimdGateInstruction {
                op,
                in0,
                in1,
                outputs,
            });
        }


        Self {
            instructions,
            wire_count,
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
