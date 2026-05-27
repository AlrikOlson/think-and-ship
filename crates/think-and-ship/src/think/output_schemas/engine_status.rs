//! Output schema for `deliberate_engine_status`.

use schemars::JsonSchema;
use serde::Serialize;

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
    pub total_steps: u32,
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
