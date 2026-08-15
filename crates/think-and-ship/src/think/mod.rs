//! Reasoning trace domain and the `think_*` tool family.
//!
//! The body of this module is ported from the standalone reasoning server with
//! peer-module references rewritten to `crate::think::*`. The tool wire
//! wiring (the `mcp::*` adapter) lives in the `mcp` module.

#![allow(clippy::collapsible_if)]

pub mod broadcast;
pub mod config;
pub mod constants;
pub mod domain;
pub mod engine;
pub mod formatter;
pub mod mcp;
pub mod output_schemas;
pub mod persistence;
pub mod util;

pub use mcp::service::ThinkService;
