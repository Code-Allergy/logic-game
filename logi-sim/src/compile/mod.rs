//! Compiler for simulation v2 (design doc section 5): `Design -> Program`.
//!
//! Passes: validate, elaborate (flatten with port unification by
//! union-find), loop check (iterative Tarjan), function matching of
//! definitions (see [`func`]), lower, simplify (constant
//! folding, alias elimination, CSE, dead-code elimination; see
//! `simplify.rs`), word lifting (bit-sliced logic back to word ops; see
//! `lift.rs`), word-level cleanup (see `words.rs`), levelize, emit.
//!
//! All passes are iterative; nothing recurses on circuit depth or instance
//! nesting.

mod elaborate;
mod emit;
pub mod func;
mod lift;
mod loops;
mod pack;
pub mod program;
mod simplify;
mod words;

#[cfg(test)]
pub(crate) mod tests;

pub use program::*;

use std::fmt;

/// Hash map with `foldhash`'s fast hasher and a fixed seed: deterministic
/// iteration for a given insertion history, and no SipHash on the
/// compiler's hot paths.
pub type FastMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::FixedState>;
/// Hash set counterpart of [`FastMap`].
pub type FastSet<K> = std::collections::HashSet<K, foldhash::fast::FixedState>;

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
    /// Bus-aligned word lifting (issue #117): groups of 1-bit bitwise
    /// instructions that read the same bit positions of words become word
    /// instructions, and 1-bit state elements forming a bus become one
    /// state word. Lifted nets stay observable through views in the debug
    /// map ([`Program::views`]).
    pub lift: bool,
    /// Function matching (issue #117, stage 2): a subcircuit definition
    /// proven equal to a catalogue word function (an adder, a mux, a
    /// register built from gate latches ...) compiles as that function.
    /// Its internal nets stay readable through shadow slots
    /// ([`Program::shadows`]). See [`func`].
    pub func_match: bool,
    /// Word-level cleanup (issue #117, stage 3): slice and concatenation
    /// round trips, ops merged across bit ranges, and state words merged
    /// where they are only read together. See `words.rs`.
    pub words: bool,
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
    /// Lay the program out for this many threads (issue #41): each rank's
    /// instructions are split into partitions of about equal estimated cost,
    /// an instruction preferring the partition that produced its operands,
    /// and each `(rank, op)` run becomes one block per partition. Use the
    /// same number as `SimOptions::threads`. 1 (the default) does nothing.
    pub partitions: u8,
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
            lift: true,
            func_match: true,
            words: true,
            probes: Vec::new(),
            slot_order: SlotOrder::default(),
            pack: PackMode::Auto,
            pack_min: DEFAULT_PACK_MIN,
            partitions: 1,
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
            lift: false,
            func_match: false,
            words: false,
            probes: Vec::new(),
            slot_order: SlotOrder::Production,
            pack: PackMode::Off,
            pack_min: DEFAULT_PACK_MIN,
            partitions: 1,
        }
    }

    /// These options laid out for `threads` threads
    /// ([`Options::partitions`]).
    pub fn for_threads(self, threads: usize) -> Self {
        Self {
            partitions: threads.clamp(1, u8::MAX as usize) as u8,
            ..self
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

/// A compiler pass, in the order the passes run. [`compile_observed`]
/// reports each one as it ends (a pass that is switched off still ends,
/// at once).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pass {
    /// [`Design::validate`].
    Validate,
    /// Flattening with port unification.
    Elaborate,
    /// The combinational-loop check.
    LoopCheck,
    /// Function matching of definitions (cached analysis).
    Match,
    /// Slot assignment and lowering to instructions.
    Lower,
    /// Constant folding, alias elimination, CSE, dead-code elimination.
    Simplify,
    /// Bus-aligned word lifting.
    Lift,
    /// Word-level cleanup.
    Words,
    /// Levelizing, partitioning and slot ordering.
    Levelize,
    /// Lane packing and block emission.
    Pack,
    /// Final slot renumbering.
    Renumber,
}

impl Pass {
    pub const ALL: [Pass; 11] = [
        Pass::Validate,
        Pass::Elaborate,
        Pass::LoopCheck,
        Pass::Match,
        Pass::Lower,
        Pass::Simplify,
        Pass::Lift,
        Pass::Words,
        Pass::Levelize,
        Pass::Pack,
        Pass::Renumber,
    ];

    /// Short lowercase name (benchmark ids).
    pub fn name(self) -> &'static str {
        match self {
            Pass::Validate => "validate",
            Pass::Elaborate => "elaborate",
            Pass::LoopCheck => "loop_check",
            Pass::Match => "match",
            Pass::Lower => "lower",
            Pass::Simplify => "simplify",
            Pass::Lift => "lift",
            Pass::Words => "words",
            Pass::Levelize => "levelize",
            Pass::Pack => "pack",
            Pass::Renumber => "renumber",
        }
    }
}

/// Compile a design. Returns every validation error, or every combinational
/// loop, found.
pub fn compile(design: &Design, options: &Options) -> Result<Program, Vec<CompileError>> {
    compile_observed(design, options, |_| {})
}

/// [`compile`], calling `pass_done` as each [`Pass`] ends (for timing the
/// passes; see `benches/sim.rs`). The observer is generic, so `compile`
/// itself pays nothing for it.
pub fn compile_observed(
    design: &Design,
    options: &Options,
    pass_done: impl FnMut(Pass),
) -> Result<Program, Vec<CompileError>> {
    compile_cached(design, options, &mut func::MatchCache::new(), pass_done)
}

/// [`compile_observed`] with a [`func::MatchCache`] kept across compiles:
/// definitions analysed for function matching by an earlier compile with
/// the same cache, and unchanged since, are not analysed again.
pub fn compile_cached(
    design: &Design,
    options: &Options,
    cache: &mut func::MatchCache,
    mut pass_done: impl FnMut(Pass),
) -> Result<Program, Vec<CompileError>> {
    design.validate().map_err(|errs| {
        errs.into_iter()
            .map(CompileError::Invalid)
            .collect::<Vec<_>>()
    })?;
    pass_done(Pass::Validate);
    let elab = elaborate::Elab::new(design);
    pass_done(Pass::Elaborate);
    let loops = loops::find_loops(&elab);
    if !loops.is_empty() {
        return Err(loops
            .into_iter()
            .map(|nodes| CompileError::CombinationalLoop { nodes })
            .collect());
    }
    pass_done(Pass::LoopCheck);
    let analysis = if options.func_match {
        func::analyze(design, cache)
    } else {
        Default::default()
    };
    pass_done(Pass::Match);
    Ok(emit::emit(elab, options, &analysis, pass_done))
}
