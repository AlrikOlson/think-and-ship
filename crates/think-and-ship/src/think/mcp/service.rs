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
//!   `schemars::schema_for!(T)` blob (see [`output_schema_for`]).
//!
//! The 11 `#[tool]` handler methods live in [`super::handlers`], in a
//! sibling `impl` block annotated with `#[tool_router]`. Multiple impl
//! blocks for the same type are fine in Rust; the generated
//! `tool_router()` constructor is accessible from this file because
//! both impls share the `ThinkService` namespace.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::ToolCallContext},
    model::{
        CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult, Meta,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};

use crate::think::engine::core::ReasoningServer;
use crate::think::output_schemas::{self, StructuredError};

use super::instructions::SERVER_INSTRUCTIONS;

const TOOL_PREFIX: &str = "think_";
const DEPRECATED_PREFIX: &str = "deliberate_";
const DEPRECATION_WARNING: &str =
    "deliberate_* tool names are deprecated and will be removed in v0.3.0; use think_* instead.";

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
    /// see it, including patched `output_schema`, `annotations`, and the
    /// `deliberate_*` deprecated aliases.
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

    /// Build a `deliberate_*` deprecated alias for a canonical `think_*`
    /// tool. Returns `None` if the source tool isn't a `think_*` tool
    /// (so the function is safe to call on the whole list).
    fn deprecated_alias_of(canonical: &rmcp::model::Tool) -> Option<rmcp::model::Tool> {
        let suffix = canonical.name.strip_prefix(TOOL_PREFIX)?;
        let mut alias = canonical.clone();
        alias.name = Cow::Owned(format!("{DEPRECATED_PREFIX}{suffix}"));
        let mut meta = canonical.meta.clone().map(|m| m.0).unwrap_or_default();
        meta.insert(
            "deprecation_warning".to_string(),
            serde_json::Value::String(DEPRECATION_WARNING.to_string()),
        );
        alias.meta = Some(Meta(meta));
        Some(alias)
    }

    /// Build a `StructuredError` envelope and emit it as a structured
    /// error result so callers with the tool's outputSchema can pattern-
    /// match on `error_kind` rather than parsing prose.
    pub(super) fn structured_err(error_kind: &str, message: impl Into<String>) -> CallToolResult {
        let env = StructuredError {
            error_kind: error_kind.into(),
            message: message.into(),
            hint: None,
        };
        match serde_json::to_value(&env) {
            Ok(v) => CallToolResult::structured_error(v),
            Err(_) => CallToolResult::error(vec![Content::text(env.message)]),
        }
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
        if let Some(suffix) = request.name.strip_prefix(DEPRECATED_PREFIX) {
            request.name = Cow::Owned(format!("{TOOL_PREFIX}{suffix}"));
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }
}
