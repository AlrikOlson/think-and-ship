use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Commit,
    Pr,
    File,
    Config,
    Deployment,
    Release,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    #[serde(rename = "ref")]
    pub reference: String,
    pub description: String,
}
