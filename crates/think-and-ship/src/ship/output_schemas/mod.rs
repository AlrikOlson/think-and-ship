use std::sync::Arc;

use rmcp::model::JsonObject;
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, JsonSchema)]
pub struct ObjectiveOutput {
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub scope: String,
    pub status: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct PlanOutput {
    pub tasks: Vec<TaskSummary>,
    pub total: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct TaskSummary {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct TaskOutput {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub task_type: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub actions_count: usize,
    pub checks_count: usize,
    pub artifacts_count: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct ActionOutput {
    pub id: u32,
    pub task_id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub description: String,
    pub files_touched: Vec<String>,
    pub tools_used: Vec<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think_step: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub struct CheckOutput {
    #[serde(rename = "type")]
    pub check_type: String,
    pub name: String,
    pub passed: bool,
    pub details: String,
    pub required: bool,
    /// `true` when `passed` came from a command the server actually ran.
    pub verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timestamp: String,
}

#[derive(Serialize, JsonSchema)]
pub struct ShipReport {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub tasks: TaskCounts,
    pub artifacts_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct TaskCounts {
    pub total: usize,
    pub completed: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct StatusOutput {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<ObjectiveOutput>,
    pub tasks: StatusCounts,
    pub task_list: Vec<TaskSummary>,
    pub recent_actions: Vec<ActionOutput>,
    pub checks: Vec<CheckRef>,
    pub artifacts: Vec<ArtifactRef>,
    pub think_refs: Vec<ThinkRef>,
}

#[derive(Serialize, JsonSchema)]
pub struct StatusCounts {
    pub planned: usize,
    pub active: usize,
    pub blocked: usize,
    pub completed: usize,
    pub skipped: usize,
    pub total: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct CheckRef {
    pub task_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub check_type: String,
    pub passed: bool,
    pub required: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ArtifactRef {
    pub task_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub description: String,
}

#[derive(Serialize, JsonSchema)]
pub struct ThinkRef {
    pub task_id: String,
    pub ref_type: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub struct ExportOutput {
    pub format: String,
    pub trace: String,
}

#[derive(Serialize, JsonSchema)]
pub struct ClearOutput {
    pub status: String,
}

#[derive(Serialize, JsonSchema)]
pub struct StructuredError {
    pub error_kind: String,
    pub message: String,
}

/// `ship_gate_open` — either the opened pending gate, or the immediate
/// default resolution when no workspace could carry it (headless-safe).
#[derive(Serialize, JsonSchema)]
pub struct GateOpenOutput {
    pub opened: bool,
    /// `pending` when opened; `unopened` when the default applied at once.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// The declared safe default (the option key expiry resolves to).
    #[serde(rename = "default", skip_serializing_if = "Option::is_none")]
    pub default_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Set when `opened` is false: the default that applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// `ship_gate_wait` — the gate's resolution at this moment.
#[derive(Serialize, JsonSchema)]
pub struct GateWaitOutput {
    /// `answered` | `expired` | `pending`.
    pub state: String,
    /// The decided (or defaulted) option key, absent while pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seconds_left: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn output_schema_for(tool_name: &str) -> Option<Arc<JsonObject>> {
    let value: Value = match tool_name {
        "ship_set_objective" => schema_for!(ObjectiveOutput).to_value(),
        "ship_plan" => schema_for!(PlanOutput).to_value(),
        "ship_start" => schema_for!(TaskOutput).to_value(),
        "ship_record" => schema_for!(ActionOutput).to_value(),
        "ship_complete" => schema_for!(TaskOutput).to_value(),
        "ship_block" => schema_for!(TaskOutput).to_value(),
        "ship_check" => schema_for!(CheckOutput).to_value(),
        "ship_gate_open" => schema_for!(GateOpenOutput).to_value(),
        "ship_gate_wait" => schema_for!(GateWaitOutput).to_value(),
        "ship_finalize" => schema_for!(ShipReport).to_value(),
        "ship_status" => schema_for!(StatusOutput).to_value(),
        "ship_export" => schema_for!(ExportOutput).to_value(),
        "ship_reset" => schema_for!(ClearOutput).to_value(),
        _ => return None,
    };
    match value {
        Value::Object(map) => Some(Arc::new(map)),
        _ => None,
    }
}
