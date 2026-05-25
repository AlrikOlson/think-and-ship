use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveStatus {
    Defined,
    Active,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Objective {
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub scope: String,
    pub status: ObjectiveStatus,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}
