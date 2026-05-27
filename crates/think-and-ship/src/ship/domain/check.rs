use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    Test,
    Lint,
    Typecheck,
    Build,
    Review,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    #[serde(rename = "type")]
    pub check_type: CheckType,
    pub name: String,
    pub passed: bool,
    pub details: String,
    pub required: bool,
    pub timestamp: String,
}
