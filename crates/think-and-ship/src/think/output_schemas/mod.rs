//! Output schemas for every MCP tool that returns structured JSON.
//!
//! These structs exist solely so `schemars::schema_for!(T)` can compute a
//! JSON Schema we attach to each `Tool.output_schema`. The engine emits
//! `serde_json::Value` directly; we never deserialize back into these
//! types at runtime. Field shapes here must therefore stay aligned with
//! the JSON the engine actually produces.
//!
//! A `tools/list` response in 2026-style MCP carries `outputSchema` on
//! every tool. Clients use it to validate `structuredContent`, agents use
//! it to pattern-match without parsing prose, and the
//! [2025-06-18 MCP spec](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)
//! requires servers that advertise an output schema to emit conformant
//! `structuredContent`.
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

use crate::think::domain::DeliberateStep;

/// Return the JSON Schema for a tool's structuredContent, keyed by tool
/// name. `None` for tools that don't emit structured output (currently
/// only `deliberate_export_trace` because its output is format-dependent
/// text).
pub fn output_schema_for(tool_name: &str) -> Option<Arc<JsonObject>> {
    let value: Value = match tool_name {
        "deliberate_record_step" => schema_for!(RecordStepOutput).to_value(),
        "deliberate_engine_status" => schema_for!(EngineStatusOutput).to_value(),
        "deliberate_get_step" => schema_for!(DeliberateStep).to_value(),
        "deliberate_search_trace" => schema_for!(SearchTraceOutput).to_value(),
        "deliberate_step_impact" => schema_for!(StepImpactOutput).to_value(),
        "deliberate_pin_step" => schema_for!(PinStepOutput).to_value(),
        "deliberate_revise_estimate" => schema_for!(ReviseEstimateOutput).to_value(),
        "deliberate_set_branch_status" => schema_for!(SetBranchStatusOutput).to_value(),
        "deliberate_trace_checkpoint" => schema_for!(TraceCheckpointOutput).to_value(),
        "deliberate_wipe_trace" => schema_for!(WipeTraceOutput).to_value(),
        _ => return None,
    };
    match value {
        Value::Object(map) => Some(Arc::new(map)),
        _ => None,
    }
}
