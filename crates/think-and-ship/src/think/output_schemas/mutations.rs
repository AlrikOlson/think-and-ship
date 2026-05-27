//! Output schemas for the small mutating tools — `deliberate_pin_step`,
//! `deliberate_revise_estimate`, `deliberate_set_branch_status`, and
//! `deliberate_wipe_trace`. Each emits a thin acknowledgment shape;
//! collected here because none warrants its own file.

use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinStepOutput {
    pub step_number: u32,
    pub was_pinned: bool,
    pub now_pinned: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReviseEstimateOutput {
    pub previous: u32,
    pub new_estimate: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SetBranchStatusOutput {
    pub branch_id: String,
    /// One of: `"active"`, `"merged"`, `"abandoned"`.
    pub previous_status: String,
    /// One of: `"active"`, `"merged"`, `"abandoned"`.
    pub new_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_into: Option<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WipeTraceOutput {
    pub cleared: bool,
}
