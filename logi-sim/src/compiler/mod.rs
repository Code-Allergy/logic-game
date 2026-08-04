pub mod combinational_cells;
pub mod common_cells;
pub mod dead_code;
pub mod sequential_cells;

use std::collections::HashMap;

use crate::circuit::{Circuit, NodeId, NodeKind, WireId};
pub use dead_code::cull_dead_nodes;

/// Optimization and compilation pipeline for logi-sim circuits.
pub struct Compiler;

impl Compiler {
    /// Runs optimization passes on a circuit before passing it to execution runners.
    pub fn optimize(circuit: &Circuit) -> Circuit {
        let optimized = Self::optimize_with_maps(circuit).0;

        // Callers that do not need raw-to-optimized identity maps can also
        // replace the circuit root itself. Mapping-preserving compilation
        // deliberately keeps that outer shell so game/editor observations
        // continue to address the user's visible nodes and wires.
        let lowered = combinational_cells::lower_handmade_adder(&optimized)
            .or_else(|| sequential_cells::lower_handmade_counter(&optimized));
        lowered
            .map(|circuit| Self::optimize_with_maps(&circuit).0)
            .unwrap_or(optimized)
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
                let lowered_before_children = combinational_cells::lower_handmade_adder(child)
                    .or_else(|| sequential_cells::lower_handmade_counter(child));
                if let Some(lowered) = lowered_before_children {
                    **child = Self::optimize(&lowered);
                } else {
                    let optimized_child = Self::optimize(child);
                    **child = combinational_cells::lower_handmade_adder(&optimized_child)
                        .or_else(|| sequential_cells::lower_handmade_counter(&optimized_child))
                        .unwrap_or(optimized_child);
                }
            }
        }

        let (mut current, mut raw_node_map, mut raw_wire_map) =
            dead_code::cull_dead_nodes_with_map(&recursively_optimized);

        // Local passes run to a fixed point. Keeping map composition here
        // makes future lowering/canonicalization passes safe to add without
        // weakening API wire observability.
        const MAX_OPTIMIZATION_ROUNDS: usize = 16;
        for _round in 0..MAX_OPTIMIZATION_ROUNDS {
            let old_shape = (current.nodes.len(), current.wires.len());
            let (next, node_map, wire_map) = common_cells::merge_common_cells_with_map(&current);

            for mapped in raw_node_map.values_mut() {
                *mapped = node_map[mapped];
            }
            for mapped in raw_wire_map.values_mut() {
                *mapped = wire_map[mapped];
            }

            current = next;
            if (current.nodes.len(), current.wires.len()) == old_shape {
                break;
            }
        }

        (current, raw_node_map, raw_wire_map)
    }
}
