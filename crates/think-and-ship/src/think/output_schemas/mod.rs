//! Output schemas for every MCP tool that returns structured JSON.
//!
//! These structs exist solely so `schemars::schema_for!(T)` can compute a
//! JSON Schema we attach to each `Tool.output_schema`. The engine emits
//! `serde_json::Value` directly; we never deserialize back into these
//! types at runtime. Field shapes here must therefore stay aligned with
//! the JSON the engine actually produces.
//!
//! `outputSchema` is what lets a client validate `structuredContent` and an
//! agent pattern-match without parsing prose, and the
//! [2025-06-18 MCP spec](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
//! requires a server that advertises one to emit conformant
//! `structuredContent`.
//!
//! This module used to claim, flatly, that "a `tools/list` response in
//! 2026-style MCP carries `outputSchema` on every tool." **That was never true
//! of this server** — 15 of 48 tools carried none — and the claim is what made
//! the gap invisible: it read as a description of the code rather than the
//! aspiration it was (mcp-outputschema-inconsistency). Corrected here, because
//! a doc that overstates the invariant is worse than no doc; it stops anyone
//! from checking.
//!
//! The real invariant, and it IS enforced — by
//! `every_tool_is_dispositioned_for_output_schema` in
//! `tests/unified_service.rs` — is weaker and honest: **every tool either
//! carries a schema or is declared**, in
//! [`crate::mcp::unified::SCHEMA_EXEMPT`] (a shape that genuinely cannot be
//! described) or [`crate::mcp::unified::SCHEMA_PENDING_BUDGET`] (a known gap,
//! which must shrink and never grow). A tool with neither fails the build.
//!
//! One file per response shape (or one file per family of small shapes —
//! see [`mutations`]). The [`output_schema_for`] dispatcher in this module
//! maps tool names to compiled `JsonSchema` blobs.

use std::sync::Arc;

use schemars::schema_for;
use serde_json::Value;

/// JSON object alias matching `rmcp::model::JsonObject`. Re-exported by
/// the wire adapter when rmcp is plugged in.
pub type JsonObject = serde_json::Map<String, Value>;

pub mod engine_status;
pub mod error_envelope;
pub mod mutations;
pub mod record_step;
pub mod search_trace;
pub mod step_impact;
pub mod trace_checkpoint;

pub use engine_status::{EngineStatusOutput, PinnedStepDescriptor, SessionDescriptor};
pub use error_envelope::StructuredError;
pub use mutations::{PinStepOutput, ReviseEstimateOutput, SetBranchStatusOutput, WipeTraceOutput};
pub use record_step::{BranchEcho, BranchSummary, RecentStepRollup, RecordStepOutput};
pub use search_trace::{SearchHit, SearchTraceOutput};
pub use step_impact::{
    ImpactBranchFork, ImpactByRelation, ImpactDownstream, ImpactUpstream, StepImpactOutput,
};
pub use trace_checkpoint::{
    CheckpointHypothesis, CheckpointRefutedChain, CheckpointRevisedUndefended,
    CheckpointStaleBranch, TraceCheckpointOutput,
};

use crate::think::domain::ThinkStep;

/// Return the JSON Schema for a tool's structuredContent, keyed by tool
/// name. `None` for tools this family does not serve, and for the one
/// declared exemption — `think_export_trace`, whose output is format-dependent
/// text. See `SCHEMA_EXEMPT`.
pub fn output_schema_for(tool_name: &str) -> Option<Arc<JsonObject>> {
    let value: Value = match tool_name {
        "think_record_step" => schema_for!(RecordStepOutput).to_value(),
        "think_engine_status" => schema_for!(EngineStatusOutput).to_value(),
        "think_get_step" => schema_for!(ThinkStep).to_value(),
        "think_search_trace" => schema_for!(SearchTraceOutput).to_value(),
        "think_step_impact" => schema_for!(StepImpactOutput).to_value(),
        "think_pin_step" => schema_for!(PinStepOutput).to_value(),
        "think_revise_estimate" => schema_for!(ReviseEstimateOutput).to_value(),
        "think_set_branch_status" => schema_for!(SetBranchStatusOutput).to_value(),
        "think_trace_checkpoint" => schema_for!(TraceCheckpointOutput).to_value(),
        "think_wipe_trace" => schema_for!(WipeTraceOutput).to_value(),
        _ => return None,
    };
    match value {
        Value::Object(map) => Some(Arc::new(map)),
        _ => None,
    }
}
