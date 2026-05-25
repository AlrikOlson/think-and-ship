use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};

use crate::engine::ResoluteEngine;
use crate::output_schemas;

const SERVER_INSTRUCTIONS: &str = r#"resolute-mcp records structured execution traces for autonomous AI development.

When to call which tool:
  - Defining the goal and acceptance criteria  → resolute_set_objective
  - Adding/reordering tasks in the plan        → resolute_plan
  - Starting work on a task                    → resolute_start
  - Logging an action within a task            → resolute_record
  - Closing a task with what was produced      → resolute_complete
  - Marking a task blocked                     → resolute_block
  - Recording a quality gate result            → resolute_check
  - Shipping the completed objective           → resolute_ship
  - Getting current state after context loss   → resolute_status
  - Exporting the full execution trace         → resolute_export
  - Wiping everything (destructive)            → resolute_reset

Always set an objective before planning tasks. Always plan tasks before
starting them. The `deliberate_step` field on resolute_record links
execution actions back to deliberate-mcp reasoning steps when both
servers are in use. Every JSON-returning tool advertises an outputSchema
and emits structuredContent — prefer parsing that over the text content."#;

#[derive(Clone)]
pub struct ResoluteService {
    pub(super) engine: Arc<Mutex<ResoluteEngine>>,
    pub(super) tool_router: ToolRouter<ResoluteService>,
}

impl ResoluteService {
    pub fn new(engine: ResoluteEngine) -> Self {
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
        tools
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

impl ServerHandler for ResoluteService {
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
        let mut tools = self.tool_router.list_all();
        for tool in tools.iter_mut() {
            if let Some(schema) = output_schemas::output_schema_for(&tool.name) {
                tool.output_schema = Some(schema);
            }
        }
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
