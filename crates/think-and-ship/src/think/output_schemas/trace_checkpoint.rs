//! Output schema for `think_trace_checkpoint`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::think::engine::maintenance::MaintenanceReport;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TraceCheckpointOutput {
    pub open_hypotheses: Vec<CheckpointHypothesis>,
    pub stale_branches: Vec<CheckpointStaleBranch>,
    /// One of: `"rising"`, `"falling"`, `"stable"`, `"insufficient_data"`.
    pub confidence_trend: String,
    pub revised_but_undefended: Vec<CheckpointRevisedUndefended>,
    pub refuted_chain_alerts: Vec<CheckpointRefutedChain>,
    /// Pinned-set size and rollup pressure, plus any pins the decay rule
    /// proposes releasing. The engine's own type, reused rather than mirrored —
    /// a hand-copied mirror is how the advertised schema drifts from what the
    /// tool actually returns.
    pub maintenance: MaintenanceReport,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckpointHypothesis {
    pub step_number: u32,
    pub thought_excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckpointStaleBranch {
    pub id: String,
    pub name: String,
    pub last_step: u32,
    pub steps_behind: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckpointRevisedUndefended {
    pub step_number: u32,
    pub revised_by: u32,
    pub depending_steps_unaware: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CheckpointRefutedChain {
    pub step_number: u32,
    pub refuted_ancestors: Vec<u32>,
}
