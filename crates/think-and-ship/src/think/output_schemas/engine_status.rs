//! Output schema for `think_engine_status`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::mcp::client_view::ClientView;

/// Engine introspection — config, counts, version, optionally per-session.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EngineStatusOutput {
    pub persistence_enabled: bool,
    pub data_dir: String,
    pub sessions_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session: Option<String>,
    pub sessions_count: u32,
    /// A COUNT of retained steps — NOT the highest step number used. The
    /// numbering is sparse, so this is smaller than the head by however many
    /// steps have been trimmed. Never derive a step number from it; use
    /// [`Self::next_step_number`].
    pub total_steps: u32,
    /// The next safe `step_number` for `think_record_step`: the highest step
    /// number recorded anywhere in this project, plus one. This is the same
    /// expression the engine uses to auto-assign an omitted number, so the
    /// published value and the value a caller actually gets cannot drift.
    ///
    /// Monotonic across history trimming (the trim drains the oldest, i.e.
    /// lowest-numbered, steps first) and across a process restart. NOT
    /// preserved by `think_wipe_trace`, which deletes the persisted files it
    /// would have to be read from.
    pub next_step_number: u32,
    pub branches_count: u32,
    pub pinned_count: u32,
    pub completed: bool,
    pub recent_steps_limit: u32,
    pub max_history_size: u32,
    pub strict_mode: bool,
    pub version: String,
    /// Present only when `verbose: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<Vec<PinnedStepDescriptor>>,
    /// Present only when `verbose: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<SessionDescriptor>>,
    /// The live MCP client on the other end of *this* request — what it
    /// declared, and where the declaration was read from. Absent when the
    /// engine is inspected outside a wire call (the CLI path has no client).
    ///
    /// This is the readable half of the capability reporting whose stderr half
    /// some hosts discard after connect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinnedStepDescriptor {
    pub step_number: u32,
    pub purpose: String,
    pub thought_excerpt: String,
    pub outcome_excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_by: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionDescriptor {
    pub session_id: String,
    pub step_count: u32,
    pub completed: bool,
    pub last_accessed_ms: u64,
    pub active: bool,
}
