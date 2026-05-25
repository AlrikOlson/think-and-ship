//! Structured error envelope returned by tools with an `outputSchema`.
//!
//! When a tool that advertises a typed output fails, we still emit a
//! structured payload so agents with the schema can pattern-match on
//! `error_kind` rather than parsing prose. The MCP-layer
//! `CallToolResult::structured_error` carries this shape.

use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StructuredError {
    /// Machine-stable label, e.g. `"validation_failed"`, `"unknown_branch"`.
    pub error_kind: String,
    /// Human-readable failure message.
    pub message: String,
    /// Optional follow-up hint for the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
