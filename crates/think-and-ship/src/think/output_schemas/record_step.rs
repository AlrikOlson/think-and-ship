//! Output schema for `deliberate_record_step`.

use schemars::JsonSchema;
use serde::Serialize;

use crate::think::domain::DepEdge;

/// What the engine returns after recording one reasoning step. Mirrors
/// the `response` map built at the end of `ReasoningServer::process_step`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecordStepOutput {
    pub step_number: u32,
    pub estimated_total: u32,
    pub total_steps: u32,

    /// Only present when the trace's final-step flag is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<bool>,

    /// The step number that this step revises, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_step: Option<u32>,

    /// Branch this step belongs to. Present when the step is on a branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchEcho>,

    /// First ~120 chars of `thought`, server-truncated for confirmation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_excerpt: Option<String>,

    /// First ~120 chars of `outcome`, server-truncated for confirmation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_excerpt: Option<String>,

    /// Echoed back so the model knows declared deps were accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<DepEdge>>,

    /// Tools the model declared it used on this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_recorded: Option<Vec<String>>,

    /// Soft advisories about this step (low confidence upstream, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,

    /// Rolling window of prior steps, pinned-aware.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_steps: Option<Vec<RecentStepRollup>>,

    /// Compact summary of every active/merged/abandoned branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches_summary: Option<Vec<BranchSummary>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BranchEcho {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RecentStepRollup {
    /// Step number.
    pub n: u32,
    pub purpose: String,
    pub thought_excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_by: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BranchSummary {
    pub id: String,
    pub name: String,
    pub from_step: u32,
    /// Count of steps on this branch.
    pub steps: u32,
    /// One of: `"active"`, `"merged"`, `"abandoned"`.
    pub status: String,
    pub depth: u32,
}
