//! `RoadmapService` — the rmcp `ServerHandler` for the `roadmap_*` family,
//! mirroring `ShipService`. Unlike think/ship there are NO deprecated aliases:
//! `roadmap_*` is a brand-new family with no legacy names to carry.

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
use crate::roadmap::output_schemas;

const SERVER_INSTRUCTIONS: &str = r#"roadmap-* drives the project roadmap: the long-horizon plan-of-plans that sits above ship_* objectives and links to think_* reasoning.

When to call which tool:
  - Add a phase/item to the plan          → roadmap_add_chunk
  - Move a chunk through its lifecycle     → roadmap_set_status
  - Edit a chunk's description/acceptance  → roadmap_update_chunk
  - Retire a chunk (kept for history)      → roadmap_obsolete_chunk
  - Suggest a reorder (human decides)      → roadmap_reprioritize
  - Pick the next ready chunk              → roadmap_next
  - Begin work on a chunk                  → roadmap_start_chunk
  - Close a chunk with proof-of-ship       → roadmap_complete_chunk
  - Attach a think:/task: cross-reference  → roadmap_link
  - Record a research refresh + its steps  → roadmap_record_refresh
  - Reconstruct the plan / get state       → roadmap_status
  - Produce a ROADMAP.md-shaped view       → roadmap_export
  - See if/where the plan is mirrored      → tracker_status
  - Mirror this roadmap into a tracker     → tracker_setup

tracker_setup reaches a THIRD-PARTY system. Its `create_missing` defaults to
false and creating a destination cannot be undone from here, so only pass true
when the human has asked for it. A missing destination with create_missing false
writes nothing at all, which is the safe outcome, not a failure to work around.

roadmap_next returns the most urgent `pending` chunk (smallest `priority`
number — lower sorts earlier) whose deps are all `done`. roadmap_reprioritize NEVER reorders — it records a proposal for a human
to accept. Every JSON-returning tool advertises an outputSchema and emits
structuredContent."#;

#[derive(Clone)]
pub struct RoadmapService {
    pub(super) engine: Arc<Mutex<RoadmapEngine>>,
    /// Optional wire-layer handle to the signal engine so
    /// `roadmap_status` can fold in a pending-signal count. `None` in unit/test
    /// contexts that don't wire signals — the count is simply omitted then. The
    /// RoadmapEngine type never references the SignalEngine (acyclic).
    pub(super) signal: Option<Arc<Mutex<crate::signal::engine::SignalEngine>>>,
    pub(super) tool_router: ToolRouter<RoadmapService>,
}

impl RoadmapService {
    pub fn new(engine: RoadmapEngine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
            signal: None,
            tool_router: Self::make_tool_router(),
        }
    }

    /// Wire the signal engine so `roadmap_status` includes a pending-signal
    /// count. Called by `cli::build_unified` with the same handle
    /// the `SignalService` holds.
    pub fn with_signal(mut self, signal: Arc<Mutex<crate::signal::engine::SignalEngine>>) -> Self {
        self.signal = Some(signal);
        self
    }

    /// Hand out a clone of the shared engine handle so a sibling family (the
    /// `signal_*` family's promote path) can create roadmap chunks
    /// without reaching into the roadmap *engine* type directly. The coupling
    /// lives at the service/wire layer; signal depends on roadmap (acyclic).
    pub fn engine(&self) -> Arc<Mutex<RoadmapEngine>> {
        Arc::clone(&self.engine)
    }

    /// Build the `roadmap_*` tools view with output schemas patched on. No
    /// alias appending — this is a new family.
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
        ErrorData::internal_error("roadmap engine mutex poisoned", None)
    }

    pub(super) fn ok_structured(value: serde_json::Value) -> CallToolResult {
        crate::infra::tool_result::ok(value)
    }

    /// A logical failure (unknown id, illegal transition, …). Delegates to
    /// [`crate::infra::tool_result::soft_error`] so the result is NEVER marked
    /// `is_error: true` — a non-error sibling can't trigger Claude Code's
    /// parallel-batch cascade-cancel. The `{ok:false, error_kind, message}`
    /// envelope is unchanged for callers that pattern-match it.
    pub(super) fn err_structured(kind: &str, message: impl Into<String>) -> CallToolResult {
        crate::infra::tool_result::soft_error(kind, message)
    }
}

impl ServerHandler for RoadmapService {
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
        // Captured before the router consumes the request; `peer` is the only
        // route to the human, and it exists only inside a tools/call.
        let asked_about = Self::consent_question_for(request.name.as_ref());
        let peer = context.peer.clone();
        let tcc = ToolCallContext::new(self, request, context);
        let result = self.tool_router.call(tcc).await;

        // Ask AFTER the work, and only when the work actually landed. A
        // question on top of a failure is noise, and `tracker_setup` failing
        // means there is no mirroring to consent about yet.
        if asked_about && Self::landed(&result) {
            let (data_dir, _, _) = crate::cli::tracker_config();
            // The outcome is deliberately dropped: by construction the ONLY
            // thing it can do is have already been remembered. Nothing here
            // touches `result`, so the caller's bytes are identical whether a
            // human was asked, declined, timed out, or was never there.
            let _ = crate::mcp::elicit::ask_and_remember_propose_consent(&peer, &data_dir).await;
        }
        result
    }
}

impl RoadmapService {
    /// Whether finishing this tool is the moment to ask about unattended
    /// proposals.
    ///
    /// Exactly one tool, and it is the onboarding one: a human who just
    /// connected their roadmap to a tracker is the only person for whom "may
    /// background sweeps propose?" is the next question rather than an
    /// interruption. Split out as a pure function so the *choice of moment* is
    /// assertable by exact name rather than by "some tool asks".
    #[must_use]
    pub fn consent_question_for(tool: &str) -> bool {
        tool == "tracker_setup"
    }

    /// Whether a tool call actually did its job.
    ///
    /// It reads the `ok` envelope, NOT `is_error`, and that is the whole point.
    /// `infra::tool_result::soft_error` sets `is_error: false` **deliberately**
    /// — "the load-bearing bit", in its own words — because a domain failure is
    /// not a protocol failure (degrade, never fail). So `is_error` is `Some(false)`
    /// on every result this server produces, success and failure alike, and a
    /// predicate reading it would call every failure a success.
    ///
    /// A first draft of this function did exactly that, and it took
    /// deliberately breaking the code under test to expose it: the test that
    /// was supposed to prove "a failed setup asks nobody anything" was really
    /// only proving that the env gate was off.
    fn landed(result: &Result<CallToolResponse, ErrorData>) -> bool {
        let Ok(CallToolResponse::Complete(r)) = result else {
            return false;
        };
        r.structured_content
            .as_ref()
            .and_then(|sc| sc.get("ok"))
            .and_then(serde_json::Value::as_bool)
            // A tool with no `ok` envelope at all has not reported a failure, so
            // it is treated as landed. Only an explicit `ok: false` blocks.
            .unwrap_or(true)
    }
}
