//! Execution tracking domain and the `ship_*` tool family.
//!
//! The body of this module is ported from the standalone execution server with
//! peer-module references rewritten to `crate::ship::*`. Tool wire wiring
//! lives in the `mcp` submodule; broadcast routes through the shared
//! `crate::infra::Broadcaster` with `Family::Ship`.

pub mod broadcast;
pub mod domain;
pub mod engine;
pub mod gate;
pub mod mcp;
pub mod output_schemas;
pub mod persistence;
pub mod report;

pub use mcp::service::ShipService;
