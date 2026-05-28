//! `UnifiedService` — the single MCP server that exposes both tool families.
//!
//! Routes incoming `tools/call` by name prefix to either [`ThinkService`]
//! or [`ShipService`]. Each underlying service handles its own
//! `deliberate_*` / `resolute_*` alias resolution internally, so this
//! layer just dispatches by family.

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};

use crate::ship::ShipService;
use crate::think::ThinkService;

const SERVER_INSTRUCTIONS: &str = r#"think-and-ship — unified MCP server for structured reasoning + execution tracking.

Two tool families share one server:

  think_*   (11 tools)   reasoning trace
  ship_*    (11 tools)   execution trace

Cross-reference reasoning to execution via execution_ref on think_record_step
(e.g. "task:auth-refactor"). Cross-reference execution to reasoning via the
deliberate_step field on ship_record. Both halves resolve the same project
identity from the working directory so traces auto-correlate.

The legacy tool names (deliberate_*, resolute_*) are accepted as deprecated
aliases with _meta.deprecation_warning set per the MCP spec; they will be
removed in v0.3.0.
"#;

/// Single MCP server exposing both `think_*` and `ship_*` tool families.
#[derive(Clone)]
pub struct UnifiedService {
    think: Arc<ThinkService>,
    ship: Arc<ShipService>,
}

impl UnifiedService {
    pub fn new(think: ThinkService, ship: ShipService) -> Self {
        Self {
            think: Arc::new(think),
            ship: Arc::new(ship),
        }
    }

    /// Returns the combined `tools/list` view: 11 `think_*` canonicals,
    /// 11 `deliberate_*` deprecated aliases, 11 `ship_*` canonicals, 11
    /// `resolute_*` deprecated aliases.
    pub fn list_tools_view(&self) -> Vec<rmcp::model::Tool> {
        let mut tools = self.think.list_tools_view();
        tools.extend(self.ship.list_tools_view());
        tools
    }

    /// Which underlying family a tool name routes to. Returns `None` for
    /// names that don't match any family's prefix.
    pub fn route_of(name: &str) -> Option<Family> {
        if name.starts_with("think_") || name.starts_with("deliberate_") {
            Some(Family::Think)
        } else if name.starts_with("ship_") || name.starts_with("resolute_") {
            Some(Family::Ship)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Think,
    Ship,
}

impl ServerHandler for UnifiedService {
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
        Ok(ListToolsResult {
            tools: self.list_tools_view(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match Self::route_of(&request.name) {
            Some(Family::Think) => self.think.call_tool(request, context).await,
            Some(Family::Ship) => self.ship.call_tool(request, context).await,
            None => Err(ErrorData::invalid_params(
                format!(
                    "unknown tool '{}'; expected a think_*, deliberate_*, ship_*, or resolute_* name",
                    request.name
                ),
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_of_recognizes_all_four_prefixes() {
        assert_eq!(
            UnifiedService::route_of("think_record_step"),
            Some(Family::Think)
        );
        assert_eq!(
            UnifiedService::route_of("deliberate_record_step"),
            Some(Family::Think)
        );
        assert_eq!(
            UnifiedService::route_of("ship_set_objective"),
            Some(Family::Ship)
        );
        assert_eq!(
            UnifiedService::route_of("resolute_set_objective"),
            Some(Family::Ship)
        );
    }

    #[test]
    fn route_of_rejects_unknown_prefixes() {
        assert_eq!(UnifiedService::route_of("audit_foo"), None);
        assert_eq!(UnifiedService::route_of("foo"), None);
        assert_eq!(UnifiedService::route_of(""), None);
    }
}
