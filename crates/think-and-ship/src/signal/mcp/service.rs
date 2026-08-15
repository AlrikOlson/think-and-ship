//! `SignalService` — the rmcp `ServerHandler` for the `signal_*` family,
//! mirroring `RoadmapService`. A brand-new family with NO deprecated aliases.

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
use crate::roadmap::engine::RoadmapEngine;
use crate::signal::engine::SignalEngine;

const SERVER_INSTRUCTIONS: &str = r#"signal-* captures and tracks stakeholder signals: questions, ideas, concerns, bugs, and feedback raised about the project. A signal is the local subset of the cloud wire envelope (docs/SIGNAL_CONTRACT.md); the local store is a CACHE of the cloud system-of-record.

When to call which tool:
  - Record a new stakeholder signal       → signal_capture
  - Reconstruct the inbox / get counts     → signal_status
  - Read one signal by id                  → signal_get
  - Attach a cross-ref (think:/chunk:/…)   → signal_link
  - Churn on a signal (enrich + advance)   → signal_research
  - What's ready to raise (earned)         → signal_pending
  - Raise one to the human                 → signal_surface
  - Defer one (snooze) / drop it (ignore)  → signal_snooze / signal_ignore
  - Promote a validated signal to a chunk  → signal_promote

Lifecycle: new → triaged → researched → surfaced → promoted; any non-terminal
state may be dismissed. Illegal transitions are rejected. signal_promote turns
a researched/surfaced signal into a backlog roadmap chunk and writes
bidirectional cross-refs (chunk:X onto the signal, signal:X onto the chunk);
promoting twice is idempotent."#;

#[derive(Clone)]
pub struct SignalService {
    pub(super) engine: Arc<Mutex<SignalEngine>>,
    /// Optional handle to the roadmap engine, shared at the wire layer so
    /// `signal_promote` can create a backlog chunk. `None` in
    /// unit/test contexts that don't wire a roadmap — promotion soft-errors then.
    pub(super) roadmap: Option<Arc<Mutex<RoadmapEngine>>>,
    pub(super) tool_router: ToolRouter<SignalService>,
}

impl SignalService {
    pub fn new(engine: SignalEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
            roadmap: None,
            tool_router: Self::make_tool_router(),
        }
    }

    /// Wire the roadmap engine so `signal_promote` can create chunks. Called by
    /// `cli::build_unified` with the same handle the `RoadmapService` holds.
    pub fn with_roadmap(mut self, roadmap: Arc<Mutex<RoadmapEngine>>) -> Self {
        self.roadmap = Some(roadmap);
        self
    }

    /// Hand out a clone of the shared signal engine handle so the
    /// `RoadmapService` can compose a pending-signal count into `roadmap_status`
    /// at the wire layer — without coupling the `RoadmapEngine` to
    /// the `SignalEngine`.
    pub fn engine(&self) -> Arc<Mutex<SignalEngine>> {
        Arc::clone(&self.engine)
    }

    /// The `signal_*` tools view. No alias appending — a new family.
    pub fn list_tools_view(&self) -> Vec<rmcp::model::Tool> {
        let mut tools = self.tool_router.list_all();
        for tool in tools.iter_mut() {
            if let Some(schema) = crate::signal::output_schemas::output_schema_for(&tool.name) {
                tool.output_schema = Some(schema);
            }
            crate::mcp::schema_sanitize::sanitize_tool_schemas(tool);
        }
        tools
    }

    pub(super) fn poisoned() -> ErrorData {
        ErrorData::internal_error("signal engine mutex poisoned", None)
    }

    pub(super) fn ok_structured(value: serde_json::Value) -> CallToolResult {
        crate::infra::tool_result::ok(value)
    }

    /// A logical failure (unknown id, illegal transition, …). Delegates to
    /// [`crate::infra::tool_result::soft_error`] so the result is NEVER marked
    /// `is_error: true` — keeping the `{ok:false, error_kind, message}` envelope
    /// without tripping Claude Code's parallel-batch cascade-cancel.
    pub(super) fn err_structured(kind: &str, message: impl Into<String>) -> CallToolResult {
        crate::infra::tool_result::soft_error(kind, message)
    }
}

impl ServerHandler for SignalService {
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
        Ok(catalog(ListToolsResult::with_all_items(
            self.list_tools_view(),
        )))
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
