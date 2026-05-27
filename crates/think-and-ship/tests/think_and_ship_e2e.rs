//! End-to-end MCP roundtrip: pair a real rmcp client with the unified
//! server over an in-memory duplex and verify that
//!
//!   - tools/list returns the full 44-tool surface,
//!   - a `deliberate_*` alias dispatches to its `think_*` canonical, and
//!   - `resolute_ship` (legacy non-1:1 rename) routes to `ship_finalize`.
//!
//! Replaces the structural input/output-schema parity tests that lived
//! in `think_mcp.rs` and `ship_mcp.rs` — those still run as cheap
//! sanity checks, but this test exercises the actual wire path.

use rmcp::{
    ClientHandler, ServiceExt,
    model::{CallToolRequestParams, ListToolsResult},
};
use serde_json::Value;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::DeliberateConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// Minimal ClientHandler — all methods have sensible defaults; we only
/// need the trait impl to satisfy the rmcp client bound.
#[derive(Clone)]
struct TestClient;

impl ClientHandler for TestClient {}

fn build_unified() -> UnifiedService {
    let mut cfg = DeliberateConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-abc123".into()));
    UnifiedService::new(think, ship)
}

async fn pair() -> (
    rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    tokio::task::JoinHandle<()>,
) {
    let server = build_unified();
    let (server_tx, client_tx) = tokio::io::duplex(4096);

    let server_handle = tokio::spawn(async move {
        // `serve()` returns a RunningService handle; if it's dropped
        // immediately the transport closes before the client can even
        // send `initialized`. Keep it alive with `.waiting().await` so
        // the server stays connected for the duration of the test.
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });
    let client_service = TestClient.serve(client_tx).await.unwrap();
    (client_service, server_handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_returns_44_via_real_client() {
    let (client_service, server_handle) = pair().await;

    let tools: ListToolsResult = client_service.peer().list_tools(None).await.unwrap();
    assert_eq!(
        tools.tools.len(),
        44,
        "expected 22 canonical + 22 aliases = 44 entries, got {}",
        tools.tools.len()
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deliberate_alias_dispatches_to_think_canonical() {
    let (client_service, server_handle) = pair().await;

    // `deliberate_engine_status` should route to `think_engine_status`
    // and return a non-error structured result.
    let req = CallToolRequestParams::new("deliberate_engine_status");
    let result = client_service.peer().call_tool(req).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "deliberate_engine_status alias should succeed; got {result:?}"
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolute_ship_routes_to_ship_finalize() {
    let (client_service, server_handle) = pair().await;

    // Seed an objective so the ship_finalize handler has state to close.
    let mut args = serde_json::Map::new();
    args.insert(
        "description".to_string(),
        Value::String("smoke test goal".to_string()),
    );
    let mut set_obj = CallToolRequestParams::new("resolute_set_objective");
    set_obj.arguments = Some(args);
    let result = client_service.peer().call_tool(set_obj).await.unwrap();
    assert_ne!(result.is_error, Some(true), "set_objective failed: {result:?}");

    // resolute_ship is the legacy non-1:1 rename whose canonical_of()
    // maps to ship_finalize. Round-trip success confirms the alias
    // resolution is wired end-to-end through real rmcp dispatch.
    let req = CallToolRequestParams::new("resolute_ship");
    let result = client_service.peer().call_tool(req).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "resolute_ship should route to ship_finalize; got {result:?}"
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}
