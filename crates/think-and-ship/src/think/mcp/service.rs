//! `ThinkService` — the MCP-facing handler.
//!
//! This module owns:
//!
//! * the service struct that wraps [`ReasoningServer`] in a `Mutex` so
//!   the rmcp routing layer (which calls handlers via `&self`) can
//!   mutate shared state,
//! * non-handler helpers (`new`, `engine`, `poisoned`, `list_tools_view`,
//!   `structured_err`), and
//! * the **manual** `ServerHandler` impl — manual rather than
//!   `#[tool_handler]`-generated because we override `list_tools` to
//!   patch each tool entry's `output_schema` with a
//!   `schemars::schema_for!(T)` blob (see [`output_schema_for`](crate::think::output_schemas::output_schema_for)).
//!
//! The 11 `#[tool]` handler methods live in [`super::handlers`], in a
//! sibling `impl` block annotated with `#[tool_router]`. Multiple impl
//! blocks for the same type are fine in Rust; the generated
//! `tool_router()` constructor is accessible from this file because
//! both impls share the `ThinkService` namespace.

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
use crate::think::engine::core::ReasoningServer;
use crate::think::output_schemas;

use super::instructions::SERVER_INSTRUCTIONS;

/// The MCP-facing handler. Wraps the engine in a Mutex so the rmcp routing
/// layer (which calls handlers via &self) can mutate the shared state.
#[derive(Clone)]
pub struct ThinkService {
    pub(super) engine: Arc<Mutex<ReasoningServer>>,
    pub(super) tool_router: ToolRouter<ThinkService>,
}

impl ThinkService {
    pub fn new(engine: ReasoningServer) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
            tool_router: Self::make_tool_router(),
        }
    }

    /// Test helper — hands out the inner engine handle.
    pub fn engine(&self) -> Arc<Mutex<ReasoningServer>> {
        self.engine.clone()
    }

    pub(super) fn poisoned() -> ErrorData {
        ErrorData::internal_error("engine mutex poisoned", None)
    }

    /// Test helper — returns the `tools/list` view exactly as 2026 clients
    /// see it, including patched `output_schema` and `annotations`.
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

    /// A logical failure, emitted so callers can pattern-match `error_kind`
    /// rather than parse prose. Delegates to
    /// [`crate::infra::tool_result::soft_error`], which keeps `is_error: false`
    /// so the result can never be the errored sibling that cancels a parallel
    /// tool-call batch (anthropics/claude-code#22264).
    pub(super) fn structured_err(error_kind: &str, message: impl Into<String>) -> CallToolResult {
        crate::infra::tool_result::soft_error(error_kind, message)
    }
}

impl ServerHandler for ThinkService {
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
