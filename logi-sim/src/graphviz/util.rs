use crate::circuit::{NodeId, NodeKind};
use slotmap::Key;

pub fn node_name(id: NodeId) -> String {
    format!("n{}", id.data().as_ffi())
}

pub fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('|', "\\|")
        .replace('<', "\\<")
        .replace('>', "\\>")
}

pub fn kind_label(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Input { .. } => "INPUT",
        NodeKind::Output { .. } => "OUTPUT",
        NodeKind::And => "AND",
        NodeKind::Or => "OR",
        NodeKind::Not => "NOT",
        NodeKind::Xor => "XOR",
        NodeKind::Nand => "NAND",
        NodeKind::Nor => "NOR",
        NodeKind::Xnor => "XNOR",
        NodeKind::Buffer => "BUFFER",
        NodeKind::Mux => "MUX",
        NodeKind::Demux => "DEMUX",
        NodeKind::TriStateBuffer => "TRISTATE",
        NodeKind::Clock { .. } => "CLOCK",
        NodeKind::SubCircuit(_) => "SUBCIRCUIT",
    }
}
