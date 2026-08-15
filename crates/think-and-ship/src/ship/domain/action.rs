use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Code,
    Test,
    Debug,
    Research,
    Config,
    Refactor,
    Review,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: u32,
    pub task_id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub action_type: ActionType,
    pub description: String,
    #[serde(default)]
    pub files_touched: Vec<String>,
    /// Tools the agent CHOSE TO MENTION — a self-report, not a measurement.
    /// See the same warning on `think::domain::step::Step::tools_used`; real
    /// invocation counts live in [`crate::usage`].
    #[serde(default)]
    pub tools_used: Vec<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think_step: Option<u32>,
}
