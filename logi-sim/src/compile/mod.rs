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
mod simplify;
pub mod program;

#[cfg(test)]
mod tests;

pub use program::*;

use std::fmt;

use crate::netlist::{Design, NetId, NodeId, ValidationError};

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
}

impl Default for Options {
    fn default() -> Self {
        Self { debug: false, const_fold: true, alias: true, cse: true, dce: true, probes: Vec::new() }
    }
}

impl Options {
    /// Every pass off: one instruction per primitive, every net observable.
    pub fn unoptimized() -> Self {
        Self { debug: true, const_fold: false, alias: false, cse: false, dce: false, probes: Vec::new() }
    }

    /// All passes except dead-code elimination; every net observable.
    pub fn debug() -> Self {
        Self { debug: true, ..Self::default() }
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
                write!(f, "combinational loop through {} node(s): {nodes:?}", nodes.len())
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile a design. Returns every validation error, or every combinational
/// loop, found.
pub fn compile(design: &Design, options: &Options) -> Result<Program, Vec<CompileError>> {
    design
        .validate()
        .map_err(|errs| errs.into_iter().map(CompileError::Invalid).collect::<Vec<_>>())?;
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
