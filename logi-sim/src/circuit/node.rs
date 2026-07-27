use slotmap::new_key_type;
use smallvec::SmallVec;

use super::pin::{InputPin, OutputPin};

new_key_type! {
    /// A key for a Node in the circuit graph. This is used to uniquely identify nodes in the circuit.
    pub struct NodeId;
}

use crate::circuit::Circuit;

#[derive(Clone, Debug)]
pub enum NodeKind {
    Input { name: String },
    Output { name: String },

    And,
    Or,
    Not,
    Xor,
    Nand,
    Nor,
    Xnor,
    Buffer,
    Mux,
    Demux,
    TriStateBuffer,
    Clock { period_ticks: u32, phase: u32 },

    SubCircuit(Box<Circuit>),
}

impl PartialEq for NodeKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NodeKind::Input { name: a }, NodeKind::Input { name: b }) => a == b,
            (NodeKind::Output { name: a }, NodeKind::Output { name: b }) => a == b,
            (NodeKind::And, NodeKind::And) => true,
            (NodeKind::Or, NodeKind::Or) => true,
            (NodeKind::Not, NodeKind::Not) => true,
            (NodeKind::Xor, NodeKind::Xor) => true,
            (NodeKind::Nand, NodeKind::Nand) => true,
            (NodeKind::Nor, NodeKind::Nor) => true,
            (NodeKind::Xnor, NodeKind::Xnor) => true,
            (NodeKind::Buffer, NodeKind::Buffer) => true,
            (NodeKind::Mux, NodeKind::Mux) => true,
            (NodeKind::Demux, NodeKind::Demux) => true,
            (NodeKind::TriStateBuffer, NodeKind::TriStateBuffer) => true,
            (
                NodeKind::Clock {
                    period_ticks: p1,
                    phase: ph1,
                },
                NodeKind::Clock {
                    period_ticks: p2,
                    phase: ph2,
                },
            ) => p1 == p2 && ph1 == ph2,
            (NodeKind::SubCircuit(a), NodeKind::SubCircuit(b)) => {
                std::ptr::eq(a.as_ref(), b.as_ref())
            }
            _ => false,
        }
    }
}

impl Eq for NodeKind {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,

    pub inputs: SmallVec<[InputPin; 4]>,
    pub outputs: SmallVec<[OutputPin; 2]>,
}
