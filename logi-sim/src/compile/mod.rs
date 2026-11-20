//! Compiler for simulation v2 (design doc section 5): `Design -> Program`.
//!
//! Passes: validate, elaborate (flatten with port unification by
//! union-find), loop check (iterative Tarjan), lower, simplify (constant
//! folding, alias elimination, CSE, dead-code elimination; see
//! `simplify.rs`), levelize, emit.
//!
//! All passes are iterative; nothing recurses on circuit depth or instance
//! nesting.

mod elaborate;
mod emit;
mod loops;
mod pack;
pub mod program;
mod simplify;

#[cfg(test)]
pub(crate) mod tests;

pub use program::*;

use std::fmt;

use crate::netlist::{Design, NetId, NodeId, ValidationError};

/// Default [`Options::pack_min`].
pub const DEFAULT_PACK_MIN: u8 = 8;

/// Compiler options. [`Options::default`] enables every simplify pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Keep every authored net observable: disables dead-code elimination.
    /// The other passes stay on; folded and aliased nets remain readable
    /// through the debug map.
    pub debug: bool,
    /// Constant propagation and folding, including constant identities.
    pub const_fold: bool,
    /// Copy, identity and alias elimination.
    pub alias: bool,
    /// Common-subexpression merging.
    pub cse: bool,
    /// Dead-code elimination (ignored when `debug` is set). The live set is
    /// the root outputs, state, and `probes`.
    pub dce: bool,
    /// Nets to keep observable under dead-code elimination:
    /// `(instance path, NetId)`.
    pub probes: Vec<(Vec<NodeId>, NetId)>,
    /// How instructions are ordered within a block and how slots are
    /// numbered (issue #41). Changes memory layout only, never results.
    pub slot_order: SlotOrder,
    /// Lane packing (issue #41): evaluate up to 64 independent 1-bit
    /// instructions of one op and rank as one `u64` operation. Nets that
    /// end up only in a lane are read through lane slots (see
    /// [`Program::locate`]).
    pub pack: PackMode,
    /// With `pack`, the fewest lanes a packed word may hold; smaller groups
    /// stay scalar.
    pub pack_min: u8,
}

/// Which 1-bit instructions [`Options::pack`] packs into lanes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PackMode {
    /// No packing.
    Off,
    /// Pack a word only where a cost model says it is cheaper than its
    /// scalar instructions even with everything in cache: most lanes must
    /// read operands already in place in another packed word, or one bit
    /// shared by many lanes.
    Aligned,
    /// Pack every packable instruction (in words of at least
    /// [`Options::pack_min`] lanes). Pays on large, memory-bound 1-bit
    /// logic, where the slot array shrinks by up to 64 times.
    All,
    /// `All` when the slots would take more than [`AUTO_PACK_BYTES`] and at
    /// least half the instructions are packable; otherwise `Off`.
    #[default]
    Auto,
}

/// Slot footprint above which [`PackMode::Auto`] packs (see the design
/// doc, section 13). Below it the slots stay in a core's cache and
/// gathering lanes costs more than it saves.
pub const AUTO_PACK_BYTES: usize = 4 << 20;

/// Slot numbering and in-block instruction order (issue #41). Every order
/// keeps blocks sorted by `(rank, op)`; they differ in where values sit in
/// memory relative to their readers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SlotOrder {
    /// Instructions in a block keep their lowering order, and slots are
    /// numbered in execution order (the M3 layout).
    Production,
    /// Ranks are laid out last to first: each block's instructions are
    /// sorted by the position of their first reader, and slots are numbered
    /// in execution order. Every block still writes a contiguous run of
    /// slots, and values read close together sit close together.
    #[default]
    FirstUse,
    /// Slots are numbered in depth-first post-order from the sinks (root
    /// outputs, then commits), so a value sits next to the other operands
    /// of its reader; each block executes in that order.
    Dfs,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            debug: false,
            const_fold: true,
            alias: true,
            cse: true,
            dce: true,
            probes: Vec::new(),
            slot_order: SlotOrder::default(),
            pack: PackMode::Auto,
            pack_min: DEFAULT_PACK_MIN,
        }
    }
}

impl Options {
    /// Every pass off: one instruction per primitive, every net observable,
    /// and the M3 memory layout ([`SlotOrder::Production`]).
    pub fn unoptimized() -> Self {
        Self {
            debug: true,
            const_fold: false,
            alias: false,
            cse: false,
            dce: false,
            probes: Vec::new(),
            slot_order: SlotOrder::Production,
            pack: PackMode::Off,
            pack_min: DEFAULT_PACK_MIN,
        }
    }

    /// All passes except dead-code elimination; every net observable.
    pub fn debug() -> Self {
        Self {
            debug: true,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The design failed [`Design::validate`].
    Invalid(ValidationError),
    /// A combinational cycle. `nodes` are the primitives on it (sorted), or,
    /// for a cycle made only of wires through instance ports, the instance
    /// nodes it crosses.
    CombinationalLoop { nodes: Vec<NodePath> },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Invalid(e) => write!(f, "invalid design: {e:?}"),
            CompileError::CombinationalLoop { nodes } => {
                write!(
                    f,
                    "combinational loop through {} node(s): {nodes:?}",
                    nodes.len()
                )
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile a design. Returns every validation error, or every combinational
/// loop, found.
pub fn compile(design: &Design, options: &Options) -> Result<Program, Vec<CompileError>> {
    design.validate().map_err(|errs| {
        errs.into_iter()
            .map(CompileError::Invalid)
            .collect::<Vec<_>>()
    })?;
    let elab = elaborate::Elab::new(design);
    let loops = loops::find_loops(&elab);
    if !loops.is_empty() {
        return Err(loops
            .into_iter()
            .map(|nodes| CompileError::CombinationalLoop { nodes })
            .collect());
    }
    Ok(emit::emit(elab, options))
}
