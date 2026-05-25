//! The core reasoning step type and its `next_action` companion.
//!
//! `DeliberateStep` is the canonical wire+storage shape recorded for every
//! `deliberate_record_step` call. The struct is `pub` because both the
//! engine and the Tauri viewer (via `deliberate_mcp::domain::DeliberateStep`)
//! deserialize from the same JSON.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::dep_edge::DepEdge;

/// Structured action for better tool integration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StructuredAction {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool: Option<String>,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parameters: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_output: Option<String>,
}

/// The `next_action` field accepts either a free-form string or a structured object.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum NextAction {
    Text(String),
    Structured(StructuredAction),
}

impl Default for NextAction {
    fn default() -> Self {
        NextAction::Text(String::new())
    }
}

impl NextAction {
    /// Return `true` when the action carries no content the formatter can render.
    pub fn is_empty(&self) -> bool {
        match self {
            NextAction::Text(s) => s.trim().is_empty(),
            NextAction::Structured(a) => a.action.trim().is_empty(),
        }
    }

    pub fn tool(&self) -> Option<&str> {
        match self {
            NextAction::Text(_) => None,
            NextAction::Structured(a) => a.tool.as_deref(),
        }
    }
}

/// One reasoning step recorded in the history.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DeliberateStep {
    pub step_number: u32,
    pub estimated_total: u32,
    // The next six fields are conceptually required (the tool description
    // says so), but we serde-default them so the rmcp framework accepts
    // partial calls into our handler. We then validate ourselves with a
    // diagnostic that can name "XML-injection in your thought text" as
    // the likely cause when the harness's tool-call parser swallowed
    // these fields. The previous behavior was a generic
    // `missing field 'outcome'` rejection that misled agents into
    // blaming the server.
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub thought: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub next_action: NextAction,
    #[serde(default)]
    pub rationale: String,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub uncertainty_notes: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revises_step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub revision_reason: Option<String>,
    // Server-set: never an input. `#[schemars(skip)]` keeps it out of the
    // JSON schema; serde still round-trips it through the engine.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[schemars(skip)]
    pub revised_by: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub is_final_step: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch_from: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub branch_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tools_used: Option<Vec<String>>,
    /// Each entry is either a bare step number or `{step, relation?}` where
    /// `relation` is "supports" | "refutes" | "depends_on". Unlabeled deps
    /// (the historical shape) are treated as "supports"-ish — no warning
    /// fires from them.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dependencies: Option<Vec<DepEdge>>,

    // Server-set: caller never provides timestamp or duration.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[schemars(skip)]
    pub timestamp: Option<String>,
    // u64 (not u128) so serde_json's default deserializer accepts the
    // value over the broadcast wire. u128 would require the
    // `arbitrary_precision` feature flag on every consumer, which the
    // Tauri viewer doesn't enable — frames with this field set were
    // being silently dropped at the socket boundary before this fix.
    // u64 milliseconds covers ~584 million years, so the range is fine.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[schemars(skip)]
    pub duration_ms: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub session_id: Option<String>,

    /// Pinned steps are surfaced in `recent_steps` even after they fall out
    /// of the chronological window — use for load-bearing conclusions you
    /// want every later step to keep in view.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pinned: Option<bool>,

    /// Canonical working directory of the MCP-server process at the time
    /// this step was recorded — the project root, in practice. Server-
    /// set on every step; never an input. Lets later tools (the viewer,
    /// migration scripts, cross-project analytics) tell which project a
    /// step belongs to without relying on `session_id` conventions.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[schemars(skip)]
    pub cwd: Option<String>,

    /// Cross-reference to a resolute-mcp entity. Format:
    /// "task:<id>", "action:<id>", "check:<name>". Links this reasoning
    /// step to the execution work it relates to.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub execution_ref: Option<String>,
}
