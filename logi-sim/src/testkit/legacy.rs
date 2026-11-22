//! Convert a 1-bit, gates-only [`Design`] into the legacy
//! [`crate::circuit::Circuit`], so benchmarks can run the same logic on the
//! legacy runners for a baseline.

use slotmap::SecondaryMap;

use crate::circuit::{self, Circuit};
use crate::netlist::{Design, NodeId, NodeKind};

pub struct LegacyCircuit {
    pub circuit: Circuit,
    /// Legacy input nodes in root input-port order.
    pub inputs: Vec<circuit::NodeId>,
    /// Legacy output nodes in root output-port order.
    pub outputs: Vec<circuit::NodeId>,
}

/// Convert a flat design using only 1-bit `Input`, `Output`, `Not` and the
/// 2-input bitwise gates. Panics on anything else.
pub fn to_legacy(design: &Design) -> LegacyCircuit {
    let d = design.root_def();
    let mut c = Circuit::new();
    let mut map: SecondaryMap<NodeId, circuit::NodeId> = SecondaryMap::new();
    for (id, node) in &d.nodes {
        let n = match node.kind {
            NodeKind::Input { w: 1 } => c.input(),
            NodeKind::Output { w: 1 } => c.output(),
            NodeKind::Not { w: 1 } => c.not(),
            NodeKind::And { w: 1 } => c.and(),
            NodeKind::Or { w: 1 } => c.or(),
            NodeKind::Xor { w: 1 } => c.xor(),
            NodeKind::Nand { w: 1 } => c.nand(),
            NodeKind::Nor { w: 1 } => c.nor(),
            NodeKind::Xnor { w: 1 } => c.xnor(),
            ref k => panic!("to_legacy: unsupported node {k:?}"),
        };
        map.insert(id, n);
    }
    for (id, node) in &d.nodes {
        for (pin, net) in node.inputs.iter().enumerate() {
            if let Some(net) = net {
                let (src, src_pin) = d.nets[*net].driver;
                c.connect((map[src], src_pin), (map[id], pin as u8));
            }
        }
    }
    LegacyCircuit {
        inputs: d.inputs.iter().map(|&n| map[n]).collect(),
        outputs: d.outputs.iter().map(|&n| map[n]).collect(),
        circuit: c,
    }
}
