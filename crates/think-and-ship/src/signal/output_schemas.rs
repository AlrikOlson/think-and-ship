//! Output schemas for the `signal_*` tools.
//!
//! # Why this module exists
//!
//! It didn't, and that was the whole defect. `think_*` and `roadmap_*` each
//! carry an `output_schemas` module and patch `Tool.output_schema` in their
//! `list_tools_view`; `signal_*` had neither, so all ten of its tools shipped
//! with no schema at all while their siblings shipped with one. Nothing said
//! so — absence is indistinguishable from an oversight, which is exactly how
//! ten tools got here (mcp-outputschema-inconsistency).
//!
//! # The shapes are the handlers', not an idealization of them
//!
//! As in [`crate::think::output_schemas`], these structs exist solely so
//! `schemars::schema_for!(T)` can compute a blob. The engine emits
//! `serde_json::Value` directly and we never deserialize back into these
//! types, so a field that drifts from what the handler actually builds is a
//! silent lie to every client that validates against it. Each type below
//! names the handler it mirrors for that reason.
//!
//! Seven of the ten tools return a bare [`Signal`], so they share its schema
//! rather than wrapping it in ten near-identical envelopes.

use std::sync::Arc;

use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::Value;

use crate::signal::domain::Signal;
use crate::think::output_schemas::JsonObject;

/// `signal_status` — mirrors `SignalEngine::status`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalStatusOutput {
    /// Project this inbox belongs to.
    pub project_id: String,
    pub counts: SignalCounts,
    /// Newest-first digest, capped at `SignalEngine::STATUS_LIST_CAP`.
    pub signals: Vec<SignalDigest>,
    /// How many signals the cap left out.
    pub omitted: usize,
}

/// Per-status tallies. `total` is every signal, not the sum of a subset.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalCounts {
    pub new: usize,
    pub triaged: usize,
    pub researched: usize,
    pub surfaced: usize,
    pub promoted: usize,
    pub dismissed: usize,
    pub total: usize,
}

/// The trimmed signal `signal_status` lists — deliberately NOT a full
/// [`Signal`]: `body` is truncated and enrichments/cross-refs are omitted, so
/// a client must not expect the full record here.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalDigest {
    pub id: String,
    pub kind: String,
    pub status: String,
    /// Who raised it, when recorded.
    pub from: Option<String>,
    /// Truncated. Fetch the full text with `signal_get`.
    pub body: String,
    pub created: String,
}

/// `signal_pending` — the inbox slice above the confidence floor.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalPendingOutput {
    /// Length of `signals` after the limit was applied — NOT the total number
    /// pending, so it cannot be used to decide whether more remain.
    pub count: usize,
    pub signals: Vec<Signal>,
}

/// `signal_promote` — the signal-to-chunk hop.
///
/// Two shapes share one schema because they are one response with a flag:
/// `created: false` means the signal was already promoted and `chunk` carries
/// the existing chunk; `created: true` means a chunk was minted and `signal`
/// carries the updated record. Both optional fields are absent in the other
/// case, which is why neither is required.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalPromoteOutput {
    pub signal_id: String,
    pub chunk_id: String,
    /// False when this call was a no-op against an already-promoted signal.
    pub created: bool,
    /// Present on the already-promoted path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<Value>,
    /// Present on the newly-created path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
}

/// The JSON Schema for a `signal_*` tool's `structuredContent`, or `None` for
/// a name this family does not serve.
///
/// Every `signal_*` tool is covered. There is no exemption in this family —
/// see [`crate::mcp::unified::SCHEMA_EXEMPT`] for the one tool server-wide
/// that legitimately has no single output shape.
pub fn output_schema_for(tool_name: &str) -> Option<Arc<JsonObject>> {
    let value: Value = match tool_name {
        // The seven that return the signal itself.
        "signal_capture" | "signal_get" | "signal_link" | "signal_research" | "signal_surface"
        | "signal_snooze" | "signal_ignore" => schema_for!(Signal).to_value(),
        "signal_status" => schema_for!(SignalStatusOutput).to_value(),
        "signal_pending" => schema_for!(SignalPendingOutput).to_value(),
        "signal_promote" => schema_for!(SignalPromoteOutput).to_value(),
        _ => return None,
    };
    match value {
        Value::Object(map) => Some(Arc::new(map)),
        _ => None,
    }
}
