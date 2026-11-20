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
//!    [`Op`] and one rank, and never read each other's results. A packed
//!    block runs [`Pack`]s instead: each evaluates up to 64 1-bit
//!    instructions of its op in the lanes of one `u64` (see below).
//!
//! Slots are numbered densely: the zero slot, root inputs, constants, state,
//! then instruction destinations. With [`super::SlotOrder::Production`] and
//! [`super::SlotOrder::FirstUse`] destinations are numbered in execution
//! order, so every block writes a contiguous run of slots; the runtime relies
//! on that only as a fast path. Within a block, instructions are ordered for
//! locality as [`super::Options::slot_order`] says.
//!
//! Construction is: zero all slots, load constants, apply inputs (all zero),
//! settle at tick 0.
//!
//! # Lane packing
//!
//! With [`super::Options::pack`], 1-bit instructions of a packable op that
//! share a rank are packed into the lanes of a 64-bit word slot. Each
//! [`Pack`] builds its operand words from in-place lanes of other words
//! (`base`), one bit shared by many lanes (`splat`) and single gathered
//! bits ([`Gather`]), stores the result word, and copies out ([`Scatter`])
//! the lanes something reads as a plain slot: a scalar instruction, a
//! commit or a root output. So commits, ports and state keys always name
//! real slots, and commits still read only slots no commit writes.
//!
//! A packed net nothing reads as a plain slot has no slot of its own. The
//! debug map holds a **lane slot** for it, numbered from `slot_count` up:
//! lane slot `slot_count + 64 * k + l` is bit `l` of real slot
//! `lane_words[k]` ([`Program::locate`]); `Sim::read` resolves both kinds.
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
/// `imm` is the instruction's entry in [`Program::imms`], described per op. Results are masked only where an op can set
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
}

/// A run of instructions with the same op and rank: `instrs[start..start+len]`,
/// or, when `packed`, `packs[start..start+len]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub op: Op,
    pub rank: u32,
    pub start: u32,
    pub len: u32,
    /// The block runs [`Pack`]s instead of [`Instr`]s.
    pub packed: bool,
}

/// Up to 64 independent 1-bit instructions of one op and rank, evaluated
/// as one `u64` operation (issue #41, `Options::pack`). Lane `l` of each
/// operand word holds lane `l`'s operand; the result word goes to `dst`,
/// and the lanes that something reads as a plain 1-bit slot are copied
/// out by `scatters`.
///
/// Ops and their lane semantics: `Copy` `a`, `Not` `!a`, `And`, `Or`,
/// `Xor` as usual, `Nand`, `Nor`, `Xnor` negated, `Mux` `a ? c : b`
/// (`(a & c) | (!a & b)`), `Switch` `a & b`. Lanes no instruction uses hold
/// unspecified bits and are never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pack {
    pub dst: u32,
    /// Lanes holding an instruction.
    pub lanes: u64,
    pub a: PackOperand,
    pub b: PackOperand,
    pub c: PackOperand,
    /// `scatters[scatter_start..scatter_start + scatter_len]`.
    pub scatter_start: u32,
    pub scatter_len: u32,
}

/// How a [`Pack`] assembles one operand word:
///
/// ```text
/// (slots[base] & keep)
///   | (if (slots[splat] >> splat_bit) & 1 == 1 { splat_mask } else { 0 })
///   | for g in gathers[gather_start..][..gather_len]:
///         ((slots[g.slot] >> g.bit) & 1) << g.lane
/// ```
///
/// `base` covers lanes already in place in another packed word, `splat`
/// one bit read by many lanes (a register enable), and gathers the rest.
/// Unused parts are `ZERO_SLOT` with a zero mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PackOperand {
    pub base: u32,
    pub keep: u64,
    pub splat: u32,
    pub splat_bit: u8,
    pub splat_mask: u64,
    pub gather_start: u32,
    pub gather_len: u32,
}

/// One gathered lane: bit `bit` of `slot` into lane `lane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gather {
    pub slot: u32,
    pub bit: u8,
    pub lane: u8,
}

/// One scattered lane: `slots[slot] = (word >> lane) & 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scatter {
    pub slot: u32,
    pub lane: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// `slots[state] = slots[next]`
    Copy { state: u32, next: u32 },
    /// `if slots[we] != 0 { mems[mem][slots[addr]] = slots[data] }`. The three
    /// slots are snapshots taken at the end of the settle.
    RamWrite {
        mem: u32,
        we: u32,
        addr: u32,
        data: u32,
    },
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
    /// The root definition's port node. Stable across edits, so callers can
    /// keep addressing a port after recompiling.
    pub node: NodeId,
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
        self.inputs
            .iter()
            .position(|p| p.name.as_deref() == Some(name))
    }
    pub fn output(&self, name: &str) -> Option<usize> {
        self.outputs
            .iter()
            .position(|p| p.name.as_deref() == Some(name))
    }
    pub fn input_by_node(&self, node: NodeId) -> Option<usize> {
        self.inputs.iter().position(|p| p.node == node)
    }
    pub fn output_by_node(&self, node: NodeId) -> Option<usize> {
        self.outputs.iter().position(|p| p.node == node)
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

/// `(instance path, NetId) <-> slot` for every authored net. A slot may be
/// a lane slot (at or above [`Program::slot_count`]; see
/// [`Program::locate`]).
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
    /// Per-instruction immediate (`imms[k]` belongs to `instrs[k]`), kept out
    /// of [`Instr`] so ops that do not need it stream 16 bytes per
    /// instruction.
    pub imms: Vec<u64>,
    pub commits: Vec<Commit>,
    pub mems: Vec<MemDesc>,
    pub ports: PortMap,
    pub debug: DebugMap,
    pub state_keys: Vec<StateKey>,
    /// Packed words run by packed blocks (empty unless `Options::pack`).
    pub packs: Vec<Pack>,
    pub gathers: Vec<Gather>,
    pub scatters: Vec<Scatter>,
    /// Real slots of the packed words that hold lane slots: lane slot
    /// `slot_count + 64 * k + l` is bit `l` of `lane_words[k]`.
    pub lane_words: Vec<u32>,
}

impl Program {
    /// Number of ranks (the settle's critical path length).
    pub fn depth(&self) -> u32 {
        self.blocks.last().map_or(0, |b| b.rank + 1)
    }

    /// Number of 1-bit instructions folded into packed words.
    pub fn packed_lanes(&self) -> usize {
        self.packs
            .iter()
            .map(|p| p.lanes.count_ones() as usize)
            .sum()
    }

    /// Total work items: scalar instructions plus packed lanes.
    pub fn work(&self) -> usize {
        self.instrs.len() + self.packed_lanes()
    }

    /// Real slot and bit holding `slot`: a lane slot (at or above
    /// `slot_count`) resolves to its packed word, any other slot to itself
    /// with bit 0 meaning the whole word. `None` if out of range.
    pub fn locate(&self, slot: u32) -> Option<(u32, Option<u8>)> {
        if slot < self.slot_count {
            return Some((slot, None));
        }
        let lane = (slot - self.slot_count) as usize;
        let word = *self.lane_words.get(lane / 64)?;
        Some((word, Some((lane % 64) as u8)))
    }
}
