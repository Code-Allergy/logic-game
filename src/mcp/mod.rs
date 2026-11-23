//! The MCP server: an AI assistant builds, views and tests circuits
//! (`docs/mcp-server.md`, #104).
//!
//! - [`batch`] – the batch editor: one pure function that applies a list
//!   of operations to a workspace document.
//! - [`view`] – text views of a circuit.
//! - [`testing`] – stored tests of chips, run as a suite.
//! - [`compiled`] – what the compiler makes of a circuit, build errors
//!   with parts named, and the tests that pin it.
//! - [`rig`] – a private simulation of one circuit, with signals by name.
//! - [`run`] – experiment scripts on a rig: stimulus, expectations,
//!   recordings, comparison with a trace, snapshots.
//! - [`memfile`] – memory contents from files (ROM images).
//! - [`suite`] – the native suite runner: large CPU test suites (Tom
//!   Harte's single-step tests, nestest, Klaus Dormann's, blargg's) run
//!   on every core, outside the server.
//! - [`app`] – the app side: the open workspace of a running app served
//!   to a session, edits written back as undo steps, the transport.
//! - [`session`] – the tools as plain functions on one open workspace.
//! - [`server`] – the tools over the Model Context Protocol (feature
//!   `mcp`; the `gatequest-mcp` program serves them on stdio).
//! - [`http`] – the same over HTTP on this machine, with a token: how the
//!   running game (`--mcp`) is reached.
//!
//! Everything but [`server`] works on documents only, with no `World`
//! and no protocol, and is always built so that the ordinary tests cover
//! it.

pub mod app;
pub mod batch;
pub mod chips;
pub mod compiled;
#[cfg(feature = "mcp")]
pub mod http;
pub mod import;
pub mod memfile;
pub mod rig;
pub mod run;
#[cfg(feature = "mcp")]
pub mod server;
pub mod session;
pub mod suite;
pub mod testing;
pub mod view;

#[cfg(test)]
mod tests;
