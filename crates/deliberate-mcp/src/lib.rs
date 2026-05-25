//! `deliberate-mcp` — structured, branching, revisable reasoning over MCP.
//!
//! The library exposes the engine so the binary and integration tests share
//! the same code path. End users invoke the `deliberate-mcp` binary; LLM
//! clients call the `deliberate_record_step` MCP tool (and 10 siblings).
//!
//! ## Module layout
//!
//! | Module              | What lives there                                |
//! |---------------------|-------------------------------------------------|
//! | [`domain`]          | Pure data types (`DeliberateStep`, `Branch`, `DeliberateHistory`, …) shared with the Tauri viewer |
//! | [`engine`]          | `ReasoningServer` and its sub-modules ([`engine::recovery`], [`engine::validation`], [`engine::core`]) |
//! | [`mcp`]             | MCP wire adapter: [`mcp::DeliberateService`], handlers, args, instructions |
//! | [`output_schemas`]  | `schemars`-derived response types, one per tool, plus a dispatcher |
//! | [`persistence`]     | Atomic session-file IO                         |
//! | [`broadcast`]       | NDJSON-over-Unix-socket fan-out for the live Tauri viewer |
//! | [`formatter`]       | Step pretty-printing (markdown / console / json) |
//! | [`config`]          | `DeliberateConfig`, env-var resolution         |
//! | [`constants`]       | Validation tables (purposes, prefixes, etc.)   |
//! | [`util::text`]      | UTF-8-safe excerpt/truncate helpers           |
//!
//! ## Back-compat shims
//!
//! [`types`] and [`server`] (the old top-level modules from 0.1) and
//! [`tool`] (the pre-rename MCP-adapter path from 0.2) are kept as
//! re-export shims so external consumers — most notably the Tauri viewer
//! at `app/src-tauri/` and the existing integration tests — keep working
//! without an import sweep. New code should import directly from
//! [`domain`], [`engine`], and [`mcp`].

// The let-chains style (`if let Some(x) = a && let Some(y) = b`) is stable in
// edition 2024 but feels heavier than the nested-`if` form for readability
// here. Keep nested `if let`s and silence the lint.
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
pub mod server;
pub mod types;
pub mod util;

/// Backwards-compatible alias for the old `crate::tool` path. New code
/// should import [`crate::mcp::DeliberateService`] directly. Tests and
/// the Tauri viewer still reference `deliberate_mcp::tool::DeliberateService`;
/// this re-export keeps them green during the transition.
pub mod tool {
    pub use crate::mcp::service::DeliberateService;
}
