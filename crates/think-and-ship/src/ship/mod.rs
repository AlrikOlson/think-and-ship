//! Execution tracking domain and the `ship_*` tool family.
//!
//! The body of this module is ported from `resolute-mcp` v0.1 with
//! peer-module references rewritten to `crate::ship::*`. Tool wire wiring
//! lives in the `mcp` submodule; broadcast routes through the shared
//! `crate::engine::Broadcaster` with `Family::Ship`.

pub mod broadcast;
pub mod domain;
pub mod engine;
pub mod mcp;
pub mod output_schemas;
pub mod persistence;

pub use mcp::service::ShipService;
