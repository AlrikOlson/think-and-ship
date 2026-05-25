//! Output schema for `deliberate_step_impact`.

use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StepImpactOutput {
    pub step_number: u32,
    pub purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_by: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revises_step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    pub upstream: ImpactUpstream,
    pub downstream: ImpactDownstream,
    pub revision_chain: Vec<u32>,
    pub branches_from: Vec<ImpactBranchFork>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactUpstream {
    /// Direct dependencies (as declared on the target step).
    pub direct: Vec<u32>,
    /// Walk of all transitive upstream deps, capped at 256.
    pub transitive: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactDownstream {
    pub direct: Vec<u32>,
    pub transitive: Vec<u32>,
    pub by_relation: ImpactByRelation,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactByRelation {
    pub supports: Vec<u32>,
    pub refutes: Vec<u32>,
    pub depends_on: Vec<u32>,
    pub unlabeled: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImpactBranchFork {
    pub id: String,
    pub name: String,
    /// One of: `"active"`, `"merged"`, `"abandoned"`.
    pub status: String,
    pub step_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<u32>,
}
