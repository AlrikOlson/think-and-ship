//! The trace-level history container and its metadata.

use serde::{Deserialize, Serialize};

use super::{branch::Branch, step::ThinkStep};

/// Per-session aggregates surfaced by snapshots and the Tauri viewer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryMetadata {
    // u64 for the same reason as `ThinkStep.duration_ms` —
    // serde_json's default deserializer rejects u128 on the viewer side.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revisions_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branches_created: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tools_used: Option<Vec<String>>,
    /// Project id resolved at the moment of the first step in this
    /// session — `<basename>-<6hex>` from cwd, or a sanitized
    /// `THINK_AND_SHIP_PROJECT_NAME` override. Lets the viewer group sessions
    /// by project without parsing the session id and lets the server
    /// refuse cross-project writes.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub project_id: Option<String>,
    /// Set once this project's steps have been adopted from the legacy
    /// global `_default.json` (think-trace-durability errata). Adoption is
    /// keyed on this marker — NOT on the project file's existence, because a
    /// May-era bare-project session file can pre-exist under the same name.
    /// Also written by wipe so a restart can't resurrect legacy steps.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub legacy_default_migrated: Option<bool>,
}

/// One session's worth of reasoning — the canonical persistence shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkHistory {
    pub steps: Vec<ThinkStep>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branches: Option<Vec<Branch>>,
    pub completed: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<HistoryMetadata>,
}
