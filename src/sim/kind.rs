//! `GateKind`: the editor's single node-kind enum.
//!
//! It is used by the ECS authoring components, by the save format and by the
//! palette. Every kind except [`GateKind::SubCircuit`] maps to exactly one
//! `logi_sim::netlist::NodeKind` (see [`GateKind::to_netlist`]); a subcircuit
//! becomes a `NodeKind::Instance` of a definition built from the
//! [`CircuitLibrary`](super::CircuitLibrary).

use bevy::prelude::*;
use logi_sim::netlist::{NodeKind, PinWidths};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Largest width the editor exposes for a word.
pub const MAX_WIDTH: u8 = logi_sim::netlist::MAX_WIDTH;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Reflect)]
#[serde(tag = "type")]
pub enum GateKind {
    /// Circuit input port, driven from the editor.
    Input {
        w: u8,
    },
    /// Circuit output port.
    Output {
        w: u8,
    },
    /// A sink shown as a number. Behaves like an output in the root circuit
    /// and is omitted from subcircuit definitions (it is visual only).
    Display {
        w: u8,
    },
    Const {
        w: u8,
        value: u64,
    },
    Not {
        w: u8,
    },
    And {
        w: u8,
    },
    Or {
        w: u8,
    },
    Xor {
        w: u8,
    },
    Nand {
        w: u8,
    },
    Nor {
        w: u8,
    },
    Xnor {
        w: u8,
    },
    /// `sel ? b : a`.
    Mux {
        w: u8,
    },
    /// `en ? d : 0` (replaces the old tri-state buffer).
    Switch {
        w: u8,
    },
    Add {
        w: u8,
    },
    Eq {
        w: u8,
    },
    LtU {
        w: u8,
    },
    ReduceOr {
        w: u8,
    },
    ReduceAnd {
        w: u8,
    },
    ReduceXor {
        w: u8,
    },
    /// Bit slice `a[lo .. lo + len]`.
    Split {
        w: u8,
        lo: u8,
        len: u8,
    },
    /// Concatenation, input 0 in the lowest bits.
    Merge {
        widths: Vec<u8>,
    },
    Extend {
        from: u8,
        to: u8,
        signed: bool,
    },
    Clock {
        period: u32,
        duty: u32,
        phase: u32,
    },
    Rom {
        addr_bits: u8,
        w: u8,
        contents: Vec<u64>,
    },
    Delay {
        w: u8,
    },
    Register {
        w: u8,
    },
    DLatch {
        w: u8,
    },
    SrLatch,
    Ram {
        addr_bits: u8,
        w: u8,
    },
    /// An instance of a circuit definition in the [`super::CircuitLibrary`].
    SubCircuit {
        definition: Uuid,
    },
}

impl GateKind {
    /// The simulator primitive this kind maps to (`None` for subcircuits).
    pub fn to_netlist(&self) -> Option<NodeKind> {
        use GateKind as G;
        Some(match self {
            G::Input { w } => NodeKind::Input { w: *w },
            G::Output { w } | G::Display { w } => NodeKind::Output { w: *w },
            G::Const { w, value } => NodeKind::Const {
                w: *w,
                value: *value & logi_sim::netlist::mask(*w),
            },
            G::Not { w } => NodeKind::Not { w: *w },
            G::And { w } => NodeKind::And { w: *w },
            G::Or { w } => NodeKind::Or { w: *w },
            G::Xor { w } => NodeKind::Xor { w: *w },
            G::Nand { w } => NodeKind::Nand { w: *w },
            G::Nor { w } => NodeKind::Nor { w: *w },
            G::Xnor { w } => NodeKind::Xnor { w: *w },
            G::Mux { w } => NodeKind::Mux { w: *w },
            G::Switch { w } => NodeKind::Switch { w: *w },
            G::Add { w } => NodeKind::Add { w: *w },
            G::Eq { w } => NodeKind::Eq { w: *w },
            G::LtU { w } => NodeKind::LtU { w: *w },
            G::ReduceOr { w } => NodeKind::ReduceOr { w: *w },
            G::ReduceAnd { w } => NodeKind::ReduceAnd { w: *w },
            G::ReduceXor { w } => NodeKind::ReduceXor { w: *w },
            G::Split { w, lo, len } => NodeKind::Split {
                w: *w,
                lo: *lo,
                len: *len,
            },
            G::Merge { widths } => NodeKind::Merge {
                widths: widths.iter().copied().collect(),
            },
            G::Extend { from, to, signed } => NodeKind::Extend {
                from: *from,
                to: *to,
                signed: *signed,
            },
            G::Clock {
                period,
                duty,
                phase,
            } => NodeKind::Clock {
                period: *period,
                duty: *duty,
                phase: *phase,
            },
            G::Rom {
                addr_bits,
                w,
                contents,
            } => NodeKind::Rom {
                addr_bits: *addr_bits,
                w: *w,
                contents: contents.as_slice().into(),
            },
            G::Delay { w } => NodeKind::Delay { w: *w },
            G::Register { w } => NodeKind::Register { w: *w },
            G::DLatch { w } => NodeKind::DLatch { w: *w },
            G::SrLatch => NodeKind::SrLatch,
            G::Ram { addr_bits, w } => NodeKind::Ram {
                addr_bits: *addr_bits,
                w: *w,
                init: Vec::new().into(),
            },
            G::SubCircuit { .. } => return None,
        })
    }

    /// Check the kind's parameters. Subcircuits are checked when the bridge
    /// builds their definition.
    pub fn check(&self) -> Result<(), String> {
        match self.to_netlist() {
            Some(kind) => kind.check_params(),
            None => Ok(()),
        }
    }

    /// Pin widths `(inputs, outputs)` of a primitive kind. Subcircuit pins
    /// come from their definition (see [`super::CircuitLibrary::pins`]).
    pub fn primitive_pins(&self) -> Option<(PinWidths, PinWidths)> {
        self.to_netlist().and_then(|k| k.pins())
    }

    /// Display name shown in the editor.
    pub fn display_name(&self) -> &'static str {
        use GateKind as G;
        match self {
            G::Input { .. } => "INPUT",
            G::Output { .. } => "OUTPUT",
            G::Display { .. } => "DISPLAY",
            G::Const { .. } => "CONST",
            G::Not { .. } => "NOT",
            G::And { .. } => "AND",
            G::Or { .. } => "OR",
            G::Xor { .. } => "XOR",
            G::Nand { .. } => "NAND",
            G::Nor { .. } => "NOR",
            G::Xnor { .. } => "XNOR",
            G::Mux { .. } => "MUX",
            G::Switch { .. } => "SWITCH",
            G::Add { .. } => "ADD",
            G::Eq { .. } => "EQ",
            G::LtU { .. } => "LT",
            G::ReduceOr { .. } => "R-OR",
            G::ReduceAnd { .. } => "R-AND",
            G::ReduceXor { .. } => "R-XOR",
            G::Split { .. } => "SPLIT",
            G::Merge { .. } => "MERGE",
            G::Extend { .. } => "EXTEND",
            G::Clock { .. } => "CLOCK",
            G::Rom { .. } => "ROM",
            G::Delay { .. } => "DELAY",
            G::Register { .. } => "REGISTER",
            G::DLatch { .. } => "D-LATCH",
            G::SrLatch => "SR-LATCH",
            G::Ram { .. } => "RAM",
            G::SubCircuit { .. } => "SUBCIRCUIT",
        }
    }

    /// Short names for the input pins of a primitive.
    pub fn input_pin_names(&self) -> &'static [&'static str] {
        use GateKind as G;
        match self {
            G::Output { .. } | G::Display { .. } | G::Not { .. } | G::Delay { .. } => &["in"],
            G::ReduceOr { .. } | G::ReduceAnd { .. } | G::ReduceXor { .. } => &["a"],
            G::Split { .. } | G::Extend { .. } => &["a"],
            G::And { .. }
            | G::Or { .. }
            | G::Xor { .. }
            | G::Nand { .. }
            | G::Nor { .. }
            | G::Xnor { .. }
            | G::Eq { .. }
            | G::LtU { .. } => &["a", "b"],
            G::Mux { .. } => &["sel", "a", "b"],
            G::Switch { .. } => &["en", "d"],
            G::Add { .. } => &["a", "b", "cin"],
            G::Register { .. } | G::DLatch { .. } => &["en", "d"],
            G::SrLatch => &["s", "r"],
            G::Rom { .. } => &["addr"],
            G::Ram { .. } => &["addr", "we", "wdata"],
            _ => &[],
        }
    }

    /// Short names for the output pins of a primitive.
    pub fn output_pin_names(&self) -> &'static [&'static str] {
        use GateKind as G;
        match self {
            G::Add { .. } => &["sum", "cout"],
            G::Output { .. } | G::Display { .. } | G::SubCircuit { .. } => &[],
            G::Delay { .. } | G::Register { .. } | G::DLatch { .. } | G::SrLatch => &["q"],
            G::Rom { .. } | G::Ram { .. } => &["data"],
            _ => &["y"],
        }
    }

    /// The main width parameter, for kinds that have one.
    pub fn width(&self) -> Option<u8> {
        use GateKind as G;
        match self {
            G::Input { w }
            | G::Output { w }
            | G::Display { w }
            | G::Const { w, .. }
            | G::Not { w }
            | G::And { w }
            | G::Or { w }
            | G::Xor { w }
            | G::Nand { w }
            | G::Nor { w }
            | G::Xnor { w }
            | G::Mux { w }
            | G::Switch { w }
            | G::Add { w }
            | G::Eq { w }
            | G::LtU { w }
            | G::ReduceOr { w }
            | G::ReduceAnd { w }
            | G::ReduceXor { w }
            | G::Split { w, .. }
            | G::Rom { w, .. }
            | G::Delay { w }
            | G::Register { w }
            | G::DLatch { w }
            | G::Ram { w, .. } => Some(*w),
            G::Extend { to, .. } => Some(*to),
            G::Merge { .. } | G::Clock { .. } | G::SrLatch | G::SubCircuit { .. } => None,
        }
    }

    /// This kind with its main width parameter set to `w` (clamped to
    /// 1..=64). Kinds without one are returned unchanged.
    pub fn with_width(&self, w: u8) -> Self {
        use GateKind as G;
        let w = w.clamp(1, MAX_WIDTH);
        let mut k = self.clone();
        match &mut k {
            G::Input { w: x }
            | G::Output { w: x }
            | G::Display { w: x }
            | G::Not { w: x }
            | G::And { w: x }
            | G::Or { w: x }
            | G::Xor { w: x }
            | G::Nand { w: x }
            | G::Nor { w: x }
            | G::Xnor { w: x }
            | G::Mux { w: x }
            | G::Switch { w: x }
            | G::Add { w: x }
            | G::Eq { w: x }
            | G::LtU { w: x }
            | G::ReduceOr { w: x }
            | G::ReduceAnd { w: x }
            | G::ReduceXor { w: x }
            | G::Rom { w: x, .. }
            | G::Delay { w: x }
            | G::Register { w: x }
            | G::DLatch { w: x }
            | G::Ram { w: x, .. } => *x = w,
            G::Const { w: x, value } => {
                *x = w;
                *value &= logi_sim::netlist::mask(w);
            }
            G::Split { w: x, lo, len } => {
                *x = w;
                *lo = (*lo).min(w - 1);
                *len = (*len).clamp(1, w - *lo);
            }
            G::Extend { from, to, .. } => {
                *to = w;
                *from = (*from).min(w);
            }
            G::Merge { .. } | G::Clock { .. } | G::SrLatch | G::SubCircuit { .. } => {}
        }
        k
    }

    pub fn is_input(&self) -> bool {
        matches!(self, GateKind::Input { .. })
    }

    /// Ports that need a label in a saved workspace.
    pub fn is_port(&self) -> bool {
        matches!(
            self,
            GateKind::Input { .. } | GateKind::Output { .. } | GateKind::Display { .. }
        )
    }

    /// Kinds offered by the editor palette, with width-parametric kinds at
    /// width `w`.
    pub fn palette(w: u8) -> Vec<(&'static str, Vec<GateKind>)> {
        use GateKind as G;
        let w = w.clamp(1, MAX_WIDTH);
        vec![
            (
                "Ports",
                vec![
                    G::Input { w },
                    G::Output { w },
                    G::Display { w },
                    G::Const { w, value: 0 },
                    G::Clock {
                        period: 10,
                        duty: 5,
                        phase: 0,
                    },
                ],
            ),
            (
                "Gates",
                vec![
                    G::Not { w },
                    G::And { w },
                    G::Or { w },
                    G::Xor { w },
                    G::Nand { w },
                    G::Nor { w },
                    G::Xnor { w },
                ],
            ),
            (
                "Routing",
                vec![
                    G::Mux { w },
                    G::Switch { w },
                    G::Split { w, lo: 0, len: 1 },
                    G::Merge {
                        widths: vec![1; usize::from(w.max(2))],
                    },
                    G::Extend {
                        from: 1,
                        to: w,
                        signed: false,
                    },
                ],
            ),
            (
                "Arithmetic",
                vec![
                    G::Add { w },
                    G::Eq { w },
                    G::LtU { w },
                    G::ReduceOr { w },
                    G::ReduceAnd { w },
                    G::ReduceXor { w },
                ],
            ),
            (
                "State",
                vec![
                    G::Delay { w },
                    G::Register { w },
                    G::DLatch { w },
                    G::SrLatch,
                    G::Ram { addr_bits: 4, w },
                    G::Rom {
                        addr_bits: 4,
                        w,
                        contents: Vec::new(),
                    },
                ],
            ),
        ]
    }

    /// Tile size for this kind.
    pub fn size(&self) -> Vec2 {
        match self {
            GateKind::Display { w } => {
                let hex_digits = usize::from(*w).div_ceil(4);
                Vec2::new((hex_digits as f32 * 18.0 + 40.0).max(120.0), 70.0)
            }
            _ => Vec2::new(50.0, 50.0),
        }
    }
}
