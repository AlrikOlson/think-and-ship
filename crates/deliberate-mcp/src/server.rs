//! Backwards-compatible alias for the engine module.
//!
//! New code should import from [`crate::engine`] directly. This shim
//! exists so external consumers (tests in `tests/server.rs`, the Tauri
//! viewer that uses `deliberate_mcp::server::ReasoningServer`) keep
//! working during the 0.3.0 reorganization without an import sweep on
//! the consumer side.

pub use crate::engine::{ProcessErr, ProcessOk, ProcessResult, ReasoningServer};
