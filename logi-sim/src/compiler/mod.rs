pub mod dead_code;

use crate::circuit::Circuit;
pub use dead_code::cull_dead_nodes;

/// Optimization and compilation pipeline for logi-sim circuits.
pub struct Compiler;

impl Compiler {
    /// Runs optimization passes on a circuit before passing it to execution runners.
    pub fn optimize(circuit: &Circuit) -> Circuit {
        // Pass 1: Cull dead nodes and unconnected wires
        let culled = cull_dead_nodes(circuit);

        // Future optimization passes:
        // Pass 2: Constant folding & logic simplification
        // Pass 3: Pattern matching & macro-gate substitution (e.g. 5-gate XOR -> NodeKind::Xor)

        culled
    }
}
