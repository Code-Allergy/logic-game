pub mod common_cells;
pub mod dead_code;

use std::collections::HashMap;

use crate::circuit::{Circuit, NodeId, NodeKind, WireId};
pub use dead_code::cull_dead_nodes;

/// Optimization and compilation pipeline for logi-sim circuits.
pub struct Compiler;

impl Compiler {
    /// Runs optimization passes on a circuit before passing it to execution runners.
    pub fn optimize(circuit: &Circuit) -> Circuit {
        Self::optimize_with_map(circuit).0
    }

    /// Runs optimization passes and returns a mapping from raw NodeIds to optimized NodeIds.
    pub fn optimize_with_map(
        circuit: &Circuit,
    ) -> (
        Circuit,
        std::collections::HashMap<crate::circuit::NodeId, crate::circuit::NodeId>,
    ) {
        let (circuit, node_map, _) = Self::optimize_with_maps(circuit);
        (circuit, node_map)
    }

    /// Run the recursive optimization pipeline while retaining raw-to-optimized
    /// mappings for both nodes and observable wires.
    pub fn optimize_with_maps(
        circuit: &Circuit,
    ) -> (Circuit, HashMap<NodeId, NodeId>, HashMap<WireId, WireId>) {
        let mut recursively_optimized = circuit.clone();
        for node in recursively_optimized.nodes.values_mut() {
            if let NodeKind::SubCircuit(child) = &mut node.kind {
                **child = Self::optimize(child);
            }
        }

        let (mut current, mut raw_node_map, mut raw_wire_map) =
            dead_code::cull_dead_nodes_with_map(&recursively_optimized);

        loop {
            let old_node_count = current.nodes.len();
            let (next, node_map, wire_map) = common_cells::merge_common_cells_with_map(&current);

            for mapped in raw_node_map.values_mut() {
                *mapped = node_map[mapped];
            }
            for mapped in raw_wire_map.values_mut() {
                *mapped = wire_map[mapped];
            }

            current = next;
            if current.nodes.len() == old_node_count {
                break;
            }
        }

        (current, raw_node_map, raw_wire_map)
    }
}
