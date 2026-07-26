use slotmap::new_key_type;
use smallvec::SmallVec;

use super::pin::{InputPin, OutputPin};

new_key_type! {
    /// A key for a Node in the circuit graph. This is used to uniquely identify nodes in the circuit.
    pub struct NodeId;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Input,
    Output,

    And,
    Or,
    Not,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,

    pub inputs: SmallVec<[InputPin; 4]>,
    pub outputs: SmallVec<[OutputPin; 2]>,
}

