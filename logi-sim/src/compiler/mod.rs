pub mod dead_code;

use crate::circuit::Circuit;
pub use dead_code::cull_dead_nodes;

/// Optimization and compilation pipeline for logi-sim circuits.
pub struct Compiler;

impl Compiler {
    /// Runs optimization passes on a circuit before passing it to execution runners.
    pub fn optimize(circuit: &Circuit) -> Circuit {
        Self::optimize_with_map(circuit).0
    }

    /// Runs optimization passes and returns a mapping from raw NodeIds to optimized NodeIds.
    pub fn optimize_with_map(circuit: &Circuit) -> (Circuit, std::collections::HashMap<crate::circuit::NodeId, crate::circuit::NodeId>) {
        dead_code::cull_dead_nodes_with_map(circuit)
    }
}
