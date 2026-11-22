//! The compiled form of a [`crate::netlist::Design`] (design doc section 5).
//!
//! # Execution model
//!
//! A program is a vector of `slot_count` `u64` slots. Every slot holds a value
//! masked to its width. Slot [`ZERO_SLOT`] is always 0 and is what unconnected
//! pins and undriven nets read. Constant slots are set once from
//! [`Program::consts`] and never written again.
//!
//! `tick()` is:
//! 1. **Commit.** Apply every [`Commit`] in order. A commit only reads
//!    `next`/snapshot slots, which no commit writes, so the order is
//!    irrelevant.
//! 2. **Advance** the tick count, then apply staged root-input writes to
//!    their port slots.
//! 3. **Settle.** Run the blocks in order. Instructions in a block share one
//!    [`Op`] and one rank, and never read each other's results.
//!
//! Construction is: zero all slots, load constants, apply inputs (all zero),
//! settle at tick 0.
//!
//! The settle also computes each state element's next value (and snapshots
//! RAM write ports) into slots that only commits read, so a root input
//! changed between ticks reaches state at the commit after the next settle.

use std::collections::HashMap;
use std::sync::Arc;

use slotmap::SecondaryMap;

use crate::netlist::{DefId, NetId, NodeId};

/// Slot that always reads 0.
pub const ZERO_SLOT: u32 = 0;

/// Opcode. Fields of [`Instr`] are slot indices unless noted as literals.
/// `imm` is described per op. Results are masked only where an op can set
/// bits above the width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Op {
    /// `a`
    Copy,
    /// `!a & imm`
    Not,
    /// `a & b`
    And,
    /// `a | b`
    Or,
    /// `a ^ b`
    Xor,
    /// `!(a & b) & imm`
    Nand,
    /// `!(a | b) & imm`
    Nor,
    /// `!(a ^ b) & imm`
    Xnor,
    /// `if a != 0 { c } else { b }`
    Mux,
    /// `if a != 0 { b } else { 0 }`
    Switch,
    /// `(a + b + c) & imm`
    AddSum,
    /// Carry out: `((a + b + c) >> imm) & 1`, computed in 128 bits; `imm` is
    /// the width.
    AddCarry,
    /// `(a == b) as u64`
    Eq,
    /// `(a < b) as u64`
    LtU,
    /// `(a != 0) as u64`
    ReduceOr,
    /// `(a == imm) as u64`
    ReduceAnd,
    /// `a.count_ones() & 1`
    ReduceXor,
    /// Bit slice: `(a >> b) & imm`; `b` is a literal shift.
    Shr,
    /// Concatenate: `a | (b << c)`; `c` is a literal shift.
    Concat,
    /// Sign-extend from `b` bits (literal) to the width with mask `imm`:
    /// `if a >> (b - 1) & 1 == 1 { (a | !mask(b)) & imm } else { a }`.
    Sext,
    /// Clock: `((tick + c) % a) < b`; `a` period, `b` duty, `c` phase are
    /// literals.
    Clock,
    /// Memory read: `mems[b][a]`, or 0 past the stored contents; `b` is a
    /// literal index into [`Program::mems`].
    MemRead,
    /// SR latch next state: `if b != 0 { 0 } else if a != 0 { 1 } else { c }`
    /// (`a` = s, `b` = r, `c` = state).
    SrNext,
}

impl Op {
    /// Which of `a`, `b`, `c` are slot operands.
    pub fn slot_operands(self) -> (bool, bool, bool) {
        use Op::*;
        match self {
            Copy | Not | ReduceOr | ReduceAnd | ReduceXor | Shr | Sext | MemRead => {
                (true, false, false)
            }
            And | Or | Xor | Nand | Nor | Xnor | Switch | Eq | LtU | Concat => (true, true, false),
            Mux | AddSum | AddCarry | SrNext => (true, true, true),
            Clock => (false, false, false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instr {
    pub dst: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub imm: u64,
}

/// A run of instructions with the same op and rank: `instrs[start..start+len]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub op: Op,
    pub rank: u32,
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// `slots[state] = slots[next]`
    Copy { state: u32, next: u32 },
    /// `if slots[we] != 0 { mems[mem][slots[addr]] = slots[data] }`. The three
    /// slots are snapshots taken at the end of the settle.
    RamWrite { mem: u32, we: u32, addr: u32, data: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemDesc {
    pub addr_bits: u8,
    pub width: u8,
    /// Initial contents, already masked to `width`. May be shorter than
    /// `2^addr_bits`; missing words are 0.
    pub init: Arc<[u64]>,
    /// RAM (true) or ROM (false).
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub slot: u32,
    pub width: u8,
    pub name: Option<String>,
}

/// Root ports in port order. Outputs whose net is undriven or unconnected
/// read [`ZERO_SLOT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortMap {
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}

impl PortMap {
    pub fn input(&self, name: &str) -> Option<usize> {
        self.inputs.iter().position(|p| p.name.as_deref() == Some(name))
    }
    pub fn output(&self, name: &str) -> Option<usize> {
        self.outputs.iter().position(|p| p.name.as_deref() == Some(name))
    }
}

/// A node in the hierarchy: the instance nodes from the root down, then the
/// node itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodePath {
    pub instances: Vec<NodeId>,
    pub node: NodeId,
}

/// One elaborated instance (the root is instance 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceInfo {
    pub def: DefId,
    /// Parent instance index and the instance node in it.
    pub parent: Option<(u32, NodeId)>,
}

/// `(instance path, NetId) <-> slot` for every authored net.
#[derive(Debug, Clone, Default)]
pub struct DebugMap {
    pub instances: Vec<InstanceInfo>,
    /// `(parent instance, instance node) -> child instance`.
    pub children: HashMap<(u32, NodeId), u32>,
    /// Per instance: `NetId -> slot`.
    pub net_slots: Vec<SecondaryMap<NetId, u32>>,
}

impl DebugMap {
    /// Instance node path of instance `inst`, from the root.
    pub fn path(&self, mut inst: u32) -> Vec<NodeId> {
        let mut path = Vec::new();
        while let Some((parent, node)) = self.instances[inst as usize].parent {
            path.push(node);
            inst = parent;
        }
        path.reverse();
        path
    }

    /// Instance index of the instance at `path` (empty = root = 0).
    pub fn instance(&self, path: &[NodeId]) -> Option<u32> {
        let mut inst = 0;
        for &node in path {
            inst = *self.children.get(&(inst, node))?;
        }
        Some(inst)
    }

    /// Slot holding `net` in the instance at `path`.
    pub fn slot(&self, path: &[NodeId], net: NetId) -> Option<u32> {
        let inst = self.instance(path)?;
        self.net_slots[inst as usize].get(net).copied()
    }

    /// Every authored net held in `slot` (linear scan; for diagnostics).
    pub fn nets_in_slot(&self, slot: u32) -> Vec<(Vec<NodeId>, NetId)> {
        let mut out = Vec::new();
        for (inst, map) in self.net_slots.iter().enumerate() {
            for (net, &s) in map {
                if s == slot {
                    out.push((self.path(inst as u32), net));
                }
            }
        }
        out
    }
}

/// Where a state element's state lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateLoc {
    Slot(u32),
    Mem(u32),
}

/// Identity of one state element, for carrying state across recompiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateKey {
    pub path: NodePath,
    pub loc: StateLoc,
    /// Word width (for `StateLoc::Slot`) or memory word width.
    pub width: u8,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub slot_count: u32,
    /// `(slot, value)`, loaded once before the first settle.
    pub consts: Vec<(u32, u64)>,
    pub blocks: Vec<Block>,
    pub instrs: Vec<Instr>,
    pub commits: Vec<Commit>,
    pub mems: Vec<MemDesc>,
    pub ports: PortMap,
    pub debug: DebugMap,
    pub state_keys: Vec<StateKey>,
}

impl Program {
    /// Number of ranks (the settle's critical path length).
    pub fn depth(&self) -> u32 {
        self.blocks.last().map_or(0, |b| b.rank + 1)
    }
}
