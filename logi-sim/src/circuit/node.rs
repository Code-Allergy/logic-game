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
    Nand,
    Nor,
    Xnor,
    Buffer,
    Mux,
    Demux,
    TriStateBuffer,
    /// Falling-edge-triggered byte counter. Inputs are increment and reset;
    /// outputs are the eight little-endian state bits.
    Counter8,
    /// Falling-edge-triggered 64-bit counter with increment/reset inputs.
    Counter64,
    /// Native ripple-adder macro cell. Inputs are A bits, B bits, and optional
    /// carry-in; outputs are sum bits followed by carry-out.
    Adder {
        bits: u16,
        carry_in: bool,
    },
    Clock {
        period_ticks: u32,
        duty_ticks: u32,
        phase: u32,
    },

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
            (NodeKind::Nand, NodeKind::Nand) => true,
            (NodeKind::Nor, NodeKind::Nor) => true,
            (NodeKind::Xnor, NodeKind::Xnor) => true,
            (NodeKind::Buffer, NodeKind::Buffer) => true,
            (NodeKind::Mux, NodeKind::Mux) => true,
            (NodeKind::Demux, NodeKind::Demux) => true,
            (NodeKind::TriStateBuffer, NodeKind::TriStateBuffer) => true,
            (NodeKind::Counter8, NodeKind::Counter8) => true,
            (NodeKind::Counter64, NodeKind::Counter64) => true,
            (
                NodeKind::Adder {
                    bits: a_bits,
                    carry_in: a_carry,
                },
                NodeKind::Adder {
                    bits: b_bits,
                    carry_in: b_carry,
                },
            ) => a_bits == b_bits && a_carry == b_carry,
            (
                NodeKind::Clock {
                    period_ticks: p1,
                    duty_ticks: d1,
                    phase: ph1,
                },
                NodeKind::Clock {
                    period_ticks: p2,
                    duty_ticks: d2,
                    phase: ph2,
                },
            ) => p1 == p2 && d1 == d2 && ph1 == ph2,
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
