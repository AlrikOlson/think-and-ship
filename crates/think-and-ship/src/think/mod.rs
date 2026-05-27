//! Reasoning trace domain and the `think_*` tool family.
//!
//! The body of this module is ported from `deliberate-mcp` v0.3 with
//! peer-module references rewritten to `crate::think::*`. Tool wire
//! wiring (the `mcp::*` adapter and `ToolFamily` impl) lives in the
//! `mcp` module and is connected in a later phase.

#![allow(clippy::collapsible_if)]

pub mod broadcast;
pub mod config;
pub mod constants;
pub mod domain;
pub mod engine;
pub mod formatter;
pub mod output_schemas;
pub mod persistence;
pub mod util;
