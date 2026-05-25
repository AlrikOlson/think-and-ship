//! Backwards-compatible re-export shim for the public types surface.
//!
//! New code should import from [`crate::domain`] directly. This module
//! exists so external consumers (notably the Tauri viewer at
//! `app/src-tauri/`) keep working without an import sweep — they use
//! `deliberate_mcp::types::*`.
//!
//! The MCP input-argument structs (`StatusArgs`, `ExportArgs`, etc.) are
//! still defined here for now; they'll move to `crate::mcp::args` in a
//! follow-up.

pub use crate::domain::{
    Branch, BranchStatus, DeliberateHistory, DeliberateStep, DepEdge, HistoryMetadata, NextAction,
    SessionEntry, StructuredAction,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// `NoArgs` is the canonical empty parameters type for zero-arg tools.
// We avoid a `///` doc comment here because schemars surfaces those as the
// MCP-schema `description`, leaking implementation details to LLM consumers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "")]
pub struct NoArgs {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Export options.")]
pub struct ExportArgs {
    /// "markdown" (default, badged), "json" (full state), "console"
    /// (ANSI-styled), or "tree" (branch structure as text).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(
    description = "Update the projected total step count on the most recent step in place."
)]
pub struct ReviseEstimateArgs {
    /// New estimated total step count for the reasoning trace. Must be >= 1.
    pub estimated_total: u32,
    /// Optional human-readable note about why the estimate is changing.
    /// Surfaced back in the response text so the trace remains self-explaining.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Look up a single recorded step by its step number.")]
pub struct StepLookupArgs {
    /// The step number to retrieve.
    pub step_number: u32,
    /// When true, walk the `revised_by` chain forward and return the live
    /// (latest-revision) step instead of the original. Defaults to false so
    /// historical inspection still works.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resolve_latest: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Case-insensitive substring search across recorded steps.")]
pub struct SearchArgs {
    /// Substring to search for, case-insensitive. Matches across the
    /// `thought`, `outcome`, `context`, `purpose`, and text-form `next_action`
    /// fields of every recorded step (including branch steps).
    pub query: String,
    /// Maximum number of matches to return. Defaults to 10 when omitted.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Trace the dependency and revision graph around a step.")]
pub struct ImpactArgs {
    /// The step to analyze. Returns upstream dependencies, downstream
    /// dependents, the revision chain through this step, and any branches
    /// that fork off it.
    pub step_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Mark an existing branch as active, merged, or abandoned.")]
pub struct BranchStatusArgs {
    /// The id of the branch (e.g. as returned by the `deliberate` tool's
    /// `branch.id` field when the branch was created).
    pub branch_id: String,
    /// New status. One of: "active", "merged", "abandoned".
    pub status: String,
    /// Optional step number that synthesized this branch back into the
    /// main reasoning line. Recorded on the branch and surfaced by
    /// `deliberate_impact`. Only meaningful when `status: "merged"`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub merged_into: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Pin or unpin a step.")]
pub struct PinArgs {
    /// Step number to pin or unpin.
    pub step_number: u32,
    /// True to pin, false to unpin. Defaults to true.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[schemars(description = "Status snapshot options.")]
pub struct StatusArgs {
    /// When true, include `pinned[]` (full step descriptors) and
    /// `sessions[]` (per-session summaries) in the response. Default false
    /// keeps the snapshot compact.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verbose: Option<bool>,
}
