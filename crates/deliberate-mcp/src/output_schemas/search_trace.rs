//! Output schema for `deliberate_search_trace`.

use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchTraceOutput {
    pub query: String,
    pub match_count: u32,
    pub matches: Vec<SearchHit>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchHit {
    pub step_number: u32,
    pub purpose: String,
    /// Which field matched: one of `thought`, `outcome`, `context`,
    /// `purpose`, `rationale`, `next_action`.
    pub matched_field: String,
    pub excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_by: Option<u32>,
}
