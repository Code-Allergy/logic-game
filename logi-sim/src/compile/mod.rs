//! Compiler for simulation v2 (design doc section 5): `Design -> Program`.
//!
//! Passes: validate, elaborate (flatten with port unification by
//! union-find), loop check (iterative Tarjan), levelize, emit. The simplify
//! pass (section 5 step 3; issue #26) is not implemented yet, so every
//! authored net stays observable regardless of [`Options::debug`].
//!
//! All passes are iterative; nothing recurses on circuit depth or instance
//! nesting.

mod elaborate;
mod emit;
mod loops;
pub mod program;

#[cfg(test)]
mod exec;
#[cfg(test)]
mod tests;

pub use program::*;

use std::fmt;

use crate::netlist::{Design, ValidationError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Keep every authored net observable. Currently always the case, since
    /// no optimization pass exists yet.
    pub debug: bool,
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
pub fn compile(design: &Design, _options: &Options) -> Result<Program, Vec<CompileError>> {
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
    Ok(emit::emit(elab))
}
