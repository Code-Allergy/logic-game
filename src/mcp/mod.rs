//! The MCP server: an AI assistant builds, views and tests circuits
//! (`docs/mcp-server.md`, #104).
//!
//! - [`batch`] – the batch editor: one pure function that applies a list
//!   of operations to a workspace document.
//! - [`view`] – text views of a circuit.
//!
//! Both work on documents only, with no `World`, and are always built so
//! that the ordinary tests cover them. The server itself (`session`,
//! `server`, the `gatequest-mcp` program) is behind the `mcp` feature.

pub mod batch;
pub mod view;

#[cfg(test)]
mod tests;
