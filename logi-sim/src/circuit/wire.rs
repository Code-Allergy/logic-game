use slotmap::new_key_type;
use smallvec::SmallVec;

use super::node::NodeId;

new_key_type! {
    /// A key for a Wire in the circuit graph. This is used to uniquely identify wires in the circuit.
    pub struct WireId;
}

pub struct OutputRef {
    pub node: NodeId,
    pub pin: u8,
}

pub struct InputRef {
    pub node: NodeId,
    pub pin: u8,
}

pub struct Wire {
    pub source: OutputRef,

    pub sinks: SmallVec<[InputRef; 4]>,
}
