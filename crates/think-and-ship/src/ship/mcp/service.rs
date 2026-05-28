use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResult, Implementation, ListToolsResult, Meta,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};

use crate::ship::engine::ShipEngine;
use crate::ship::output_schemas;

const TOOL_PREFIX: &str = "ship_";
const DEPRECATED_PREFIX: &str = "resolute_";
const DEPRECATION_WARNING: &str =
    "resolute_* tool names are deprecated and will be removed in v0.3.0; use ship_* instead.";

const SERVER_INSTRUCTIONS: &str = r#"resolute-mcp records structured execution traces for autonomous AI development.

When to call which tool:
  - Defining the goal and acceptance criteria  → ship_set_objective
  - Adding/reordering tasks in the plan        → ship_plan
  - Starting work on a task                    → ship_start
  - Logging an action within a task            → ship_record
  - Closing a task with what was produced      → ship_complete
  - Marking a task blocked                     → ship_block
  - Recording a quality gate result            → ship_check
  - Shipping the completed objective           → ship_finalize
  - Getting current state after context loss   → ship_status
  - Exporting the full execution trace         → ship_export
  - Wiping everything (destructive)            → ship_reset

Always set an objective before planning tasks. Always plan tasks before
starting them. The `deliberate_step` field on ship_record links
execution actions back to deliberate-mcp reasoning steps when both
servers are in use. Every JSON-returning tool advertises an outputSchema
and emits structuredContent — prefer parsing that over the text content."#;

#[derive(Clone)]
pub struct ShipService {
    pub(super) engine: Arc<Mutex<ShipEngine>>,
    pub(super) tool_router: ToolRouter<ShipService>,
}

impl ShipService {
    pub fn new(engine: ShipEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
            tool_router: Self::make_tool_router(),
        }
    }

    pub fn list_tools_view(&self) -> Vec<rmcp::model::Tool> {
        let mut tools = self.tool_router.list_all();
        for tool in tools.iter_mut() {
            if let Some(schema) = output_schemas::output_schema_for(&tool.name) {
                tool.output_schema = Some(schema);
            }
        }
        let aliases: Vec<rmcp::model::Tool> =
            tools.iter().filter_map(Self::deprecated_alias_of).collect();
        tools.extend(aliases);
        tools
    }

    fn deprecated_alias_of(canonical: &rmcp::model::Tool) -> Option<rmcp::model::Tool> {
        let suffix = canonical.name.strip_prefix(TOOL_PREFIX)?;
        // Most tools follow the simple `ship_X` → `resolute_X` pattern.
        // `ship_finalize` is the one rename whose legacy name doesn't
        // match that pattern (it was historically `resolute_ship` /
        // `ship_ship`), so its alias is named explicitly.
        let legacy_name = if canonical.name == "ship_finalize" {
            "resolute_ship".to_string()
        } else {
            format!("{DEPRECATED_PREFIX}{suffix}")
        };
        let mut alias = canonical.clone();
        alias.name = Cow::Owned(legacy_name);
        let mut meta = canonical.meta.clone().map(|m| m.0).unwrap_or_default();
        meta.insert(
            "deprecation_warning".to_string(),
            serde_json::Value::String(DEPRECATION_WARNING.to_string()),
        );
        alias.meta = Some(Meta(meta));
        Some(alias)
    }

    /// Map a deprecated tool name to its canonical name. Returns `None`
    /// if the input is already canonical (or doesn't match any alias).
    fn canonical_of(name: &str) -> Option<String> {
        if name == "resolute_ship" || name == "ship_ship" {
            return Some("ship_finalize".to_string());
        }
        // General resolute_X → ship_X for everything else.
        name.strip_prefix(DEPRECATED_PREFIX)
            .map(|s| format!("{TOOL_PREFIX}{s}"))
    }

    pub(super) fn poisoned() -> ErrorData {
        ErrorData::internal_error("engine mutex poisoned", None)
    }

    pub(super) fn ok_structured(value: serde_json::Value) -> CallToolResult {
        CallToolResult::structured(value)
    }

    pub(super) fn err_structured(kind: &str, message: impl Into<String>) -> CallToolResult {
        let value = serde_json::json!({
            "error_kind": kind,
            "message": message.into(),
        });
        CallToolResult::structured_error(value)
    }
}

impl ServerHandler for ShipService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(SERVER_INSTRUCTIONS)
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.list_tools_view();
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(canonical) = Self::canonical_of(&request.name) {
            request.name = Cow::Owned(canonical);
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
