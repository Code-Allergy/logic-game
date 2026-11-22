//! The MCP server: an AI assistant builds, views and tests circuits
//! (`docs/mcp-server.md`, #104).
//!
//! - [`batch`] – the batch editor: one pure function that applies a list
//!   of operations to a workspace document.
//! - [`view`] – text views of a circuit.
//! - [`session`] – the tools as plain functions on one open workspace.
//! - [`server`] – the tools over the Model Context Protocol (feature
//!   `mcp`; the `gatequest-mcp` program serves them on stdio).
//!
//! Everything but [`server`] works on documents only, with no `World`
//! and no protocol, and is always built so that the ordinary tests cover
//! it.

pub mod batch;
#[cfg(feature = "mcp")]
pub mod server;
pub mod session;
pub mod view;

#[cfg(test)]
mod tests;
