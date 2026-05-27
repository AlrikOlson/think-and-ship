//! `think-and-ship` — unified MCP server for structured reasoning and execution
//! tracking.
//!
//! Two namespaced tool families behind one server: `think_*` records reasoning
//! traces, `ship_*` records execution traces. They cross-reference each other
//! through a typed `CrossRef` enum.
//!
//! See `docs/ARCHITECTURE.md` at the repo root for the full design.

pub mod cli;
pub mod engine;
pub mod mcp;
pub mod ship;
pub mod think;
