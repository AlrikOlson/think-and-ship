use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};

use crate::mcp::cache::catalog;
use crate::ship::engine::ShipEngine;
use crate::ship::output_schemas;

/// Names a caller derives for the finalize verb instead of reading it: the
/// prefix is `ship_`, so `ship_ship` is the shape a model reaches for. Answered
/// with the real name (see `UnifiedService::replacement_for_retired`) rather
/// than a bare "unknown tool".
pub(crate) const MISDERIVED_FINALIZE_NAMES: [&str; 1] = ["ship_ship"];

const SERVER_INSTRUCTIONS: &str = r#"ship_* records structured execution traces for autonomous AI development.

When to call which tool:
  - Defining the goal and acceptance criteria  → ship_set_objective
  - Adding/reordering tasks in the plan        → ship_plan
  - Starting work on a task                    → ship_start
  - Logging an action within a task            → ship_record
  - Closing a task with what was produced      → ship_complete
  - Marking a task blocked                     → ship_block
  - Recording a quality gate result            → ship_check
  - Pausing for a human yes (webapp-answered)  → ship_gate_open, then poll ship_gate_wait
  - Shipping the completed objective           → ship_finalize
  - Getting current state after context loss   → ship_status
  - Exporting the full execution trace         → ship_export
  - Wiping everything (destructive)            → ship_reset

Always set an objective before planning tasks. Always plan tasks before
starting them. The `think_step` field on ship_record links an
execution action back to the think_* reasoning step that motivated it
(the field keeps its historical name). Every JSON-returning tool advertises an outputSchema
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
            crate::mcp::schema_sanitize::sanitize_tool_schemas(tool);
        }
        tools
    }

    pub(super) fn poisoned() -> ErrorData {
        ErrorData::internal_error("engine mutex poisoned", None)
    }

    pub(super) fn ok_structured(value: serde_json::Value) -> CallToolResult {
        crate::infra::tool_result::ok(value)
    }

    /// A logical failure. Delegates to [`crate::infra::tool_result::soft_error`]
    /// so the result is never `is_error: true` and can't be the errored sibling
    /// that cancels a parallel tool-call batch. Envelope shape is unchanged.
    pub(super) fn err_structured(kind: &str, message: impl Into<String>) -> CallToolResult {
        crate::infra::tool_result::soft_error(kind, message)
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
        Ok(catalog(ListToolsResult::with_all_items(tools)))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
