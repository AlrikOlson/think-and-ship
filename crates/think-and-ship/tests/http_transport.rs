//! End-to-end Streamable HTTP transport: spin up the unified server with the
//! production `StreamableHttpService` + axum wiring on an ephemeral TCP port,
//! drive it with rmcp's reqwest-backed Streamable HTTP client, and verify
//!
//!   - initialize handshake completes,
//!   - tools/list returns the full 44-tool surface (matching the stdio path),
//!   - a `deliberate_*` alias dispatches over the HTTP wire too.
//!
//! Mirrors `think_and_ship_e2e.rs` (stdio) but over a real TCP socket so the
//! production HTTP wiring in `cli::run_http` stays smoke-tested.

use std::sync::Arc;

use rmcp::{
    ClientHandler, ServiceExt,
    model::{CallToolRequestParams, ListToolsResult},
    transport::{
        StreamableHttpClientTransport,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use think_and_ship::mcp::UnifiedService;
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::DeliberateConfig;
use think_and_ship::think::engine::core::ReasoningServer;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct TestClient;
impl ClientHandler for TestClient {}

fn build_unified() -> UnifiedService {
    let mut cfg = DeliberateConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-http".into()));
    UnifiedService::new(think, ship)
}

/// Spawn the unified server in-process on an ephemeral port and return the URL
/// pointing at `/mcp` plus the cancellation token that shuts it down.
async fn spawn_http_server() -> (String, CancellationToken) {
    let unified = build_unified();
    let ct = CancellationToken::new();

    let service = StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(unified.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });

    (format!("http://{addr}/mcp"), ct)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_tools_list_returns_44() {
    let (url, ct) = spawn_http_server().await;

    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = TestClient.serve(transport).await.expect("serve");

    let tools: ListToolsResult = client.peer().list_tools(None).await.unwrap();
    assert_eq!(
        tools.tools.len(),
        44,
        "expected 22 canonical + 22 aliases = 44 over HTTP, got {}",
        tools.tools.len()
    );

    let _ = client.cancel().await;
    ct.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_deliberate_alias_dispatches_to_think_canonical() {
    let (url, ct) = spawn_http_server().await;

    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = TestClient.serve(transport).await.expect("serve");

    let req = CallToolRequestParams::new("deliberate_engine_status");
    let result = client.peer().call_tool(req).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "deliberate_engine_status alias should succeed over HTTP; got {result:?}"
    );

    let _ = client.cancel().await;
    ct.cancel();
}
