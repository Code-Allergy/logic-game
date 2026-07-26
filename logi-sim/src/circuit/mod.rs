pub mod bit_packed;
pub mod event_driven;
pub mod flattened;
pub mod node;
pub mod pin;
pub mod utils;
pub mod wire;

pub use {
    bit_packed::{BitPackedCircuit, SimdGateInstruction, SimdOp},
    event_driven::EventDrivenCircuit,
    flattened::FlattenedCircuit,
    node::{Node, NodeId, NodeKind},
    pin::{InputPin, OutputPin},
    wire::{InputRef, OutputRef, Wire, WireId},
};




use utils::{get_in_pin_name, get_out_pin_name};

use slotmap::SlotMap;
use smallvec::{SmallVec, smallvec};

#[derive(Clone, Debug, Default)]
pub struct Circuit {
    pub nodes: SlotMap<NodeId, Node>,
    pub wires: SlotMap<WireId, Wire>,
}


impl Circuit {
    pub fn new() -> Self {
        Self {
            nodes: SlotMap::with_key(),
            wires: SlotMap::with_key(),
        }
    }

    pub fn add_node(&mut self, kind: NodeKind, inputs: usize, outputs: usize) -> NodeId {
        self.nodes.insert(Node {
            kind,
            inputs: (0..inputs)
                .map(|idx| InputPin {
                    name: get_in_pin_name(idx),
                    wire: None,
                })
                .collect(),
            outputs: (0..outputs)
                .map(|idx| OutputPin {
                    name: get_out_pin_name(idx),
                    wires: SmallVec::new(),
                })
                .collect(),
        })
    }

    pub fn input(&mut self) -> NodeId {
        self.add_node(NodeKind::Input, 0, 1)
    }

    pub fn output(&mut self) -> NodeId {
        self.add_node(NodeKind::Output, 1, 0)
    }

    pub fn and(&mut self) -> NodeId {
        self.add_node(NodeKind::And, 2, 1)
    }

    pub fn or(&mut self) -> NodeId {
        self.add_node(NodeKind::Or, 2, 1)
    }

    pub fn not(&mut self) -> NodeId {
        self.add_node(NodeKind::Not, 1, 1)
    }

    pub fn xor(&mut self) -> NodeId {
        self.add_node(NodeKind::Xor, 2, 1)
    }
}


// connections
impl Circuit {
    pub fn connect(&mut self, source: (NodeId, u8), sink: (NodeId, u8)) -> WireId {
        let wire = self.wires.insert(Wire {
            source: OutputRef {
                node: source.0,
                pin: source.1,
            },
            sinks: smallvec![InputRef {
                node: sink.0,
                pin: sink.1,
            }],
        });

        self.nodes[source.0].outputs[source.1 as usize]
            .wires
            .push(wire);

        self.nodes[sink.0].inputs[sink.1 as usize].wire = Some(wire);

        wire
    }
}
