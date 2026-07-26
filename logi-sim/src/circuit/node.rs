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
    Input,
    Output,

    And,
    Or,
    Not,
    Xor,

    SubCircuit(Box<Circuit>),
}

impl PartialEq for NodeKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NodeKind::Input, NodeKind::Input) => true,
            (NodeKind::Output, NodeKind::Output) => true,
            (NodeKind::And, NodeKind::And) => true,
            (NodeKind::Or, NodeKind::Or) => true,
            (NodeKind::Not, NodeKind::Not) => true,
            (NodeKind::Xor, NodeKind::Xor) => true,
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

