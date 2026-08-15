//! End-to-end MCP roundtrip: pair a real rmcp client with the unified
//! server over an in-memory duplex and verify that
//!
//!   - tools/list returns the full 66-tool surface,
//!   - a name the server does not serve fails with its replacement named.
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
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// Minimal ClientHandler — all methods have sensible defaults; we only
/// need the trait impl to satisfy the rmcp client bound.
#[derive(Clone)]
struct TestClient;

impl ClientHandler for TestClient {}

fn build_unified() -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-abc123".into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new("test-abc123".into()));
    let signal = SignalService::new(SignalEngine::new("test-abc123".into()));
    UnifiedService::new(think, ship, roadmap, signal)
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
async fn tools_list_returns_53_via_real_client() {
    let (client_service, server_handle) = pair().await;

    let tools: ListToolsResult = client_service.peer().list_tools(None).await.unwrap();
    assert_eq!(
        tools.tools.len(),
        53,
        "expected 11 think_* + 13 ship_* + 17 roadmap_* + 2 tracker_* + 10 signal_* = 53 entries, got {}",
        tools.tools.len()
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

/// Through real rmcp dispatch, a name the server does not serve comes back with
/// its replacement named rather than a bare "unknown tool". `ship_ship` is the
/// case that matters: it is what a caller derives from the family prefix.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_misderived_name_fails_with_its_replacement_named() {
    let (client_service, server_handle) = pair().await;

    let req = CallToolRequestParams::new("ship_ship");
    let err = client_service
        .peer()
        .call_tool(req)
        .await
        .expect_err("ship_ship is not served");
    let message = err.to_string();
    assert!(
        message.contains("ship_finalize"),
        "calling ship_ship should name ship_finalize, got: {message}"
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

/// `ship_finalize` works end-to-end over the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ship_finalize_round_trips_over_the_wire() {
    let (client_service, server_handle) = pair().await;

    let mut args = serde_json::Map::new();
    args.insert(
        "description".to_string(),
        Value::String("smoke test goal".to_string()),
    );
    let mut set_obj = CallToolRequestParams::new("ship_set_objective");
    set_obj.arguments = Some(args);
    let result = client_service.peer().call_tool(set_obj).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "set_objective failed: {result:?}"
    );

    let req = CallToolRequestParams::new("ship_finalize");
    let result = client_service.peer().call_tool(req).await.unwrap();
    assert_ne!(
        result.is_error,
        Some(true),
        "ship_finalize should close the objective; got {result:?}"
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roadmap_family_round_trips_over_the_wire() {
    let (client_service, server_handle) = pair().await;

    // Add a chunk via the roadmap_* family.
    let mut args = serde_json::Map::new();
    args.insert("id".to_string(), Value::String("phase-1".to_string()));
    args.insert(
        "title".to_string(),
        Value::String("First chunk".to_string()),
    );
    args.insert("priority".to_string(), Value::from(5));
    let mut add = CallToolRequestParams::new("roadmap_add_chunk");
    add.arguments = Some(args);
    let result = client_service.peer().call_tool(add).await.unwrap();
    assert_ne!(result.is_error, Some(true), "add_chunk failed: {result:?}");

    // roadmap_status should report the chunk and select it as next.
    let status = client_service
        .peer()
        .call_tool(CallToolRequestParams::new("roadmap_status"))
        .await
        .unwrap();
    let sc = status
        .structured_content
        .expect("roadmap_status returns structured content");
    assert_eq!(sc["counts"]["total"], 1);
    assert_eq!(sc["next"], "phase-1");

    // roadmap_export markdown projection should contain the chunk title.
    let export = client_service
        .peer()
        .call_tool(CallToolRequestParams::new("roadmap_export"))
        .await
        .unwrap();
    let ec = export
        .structured_content
        .expect("roadmap_export returns structured content");
    let md = ec["roadmap"].as_str().unwrap_or_default();
    assert!(
        md.contains("First chunk"),
        "markdown export should contain the chunk title, got: {md}"
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roadmap_cross_family_linkage_over_the_wire() {
    let (client_service, server_handle) = pair().await;

    let call = |name: &'static str, args: serde_json::Map<String, Value>| {
        let mut req = CallToolRequestParams::new(name);
        req.arguments = Some(args);
        let peer = client_service.peer().clone();
        async move { peer.call_tool(req).await.unwrap() }
    };

    // Seed a chunk.
    let mut a = serde_json::Map::new();
    a.insert("id".to_string(), Value::String("phase-1".to_string()));
    a.insert("title".to_string(), Value::String("Wire it".to_string()));
    assert_ne!(call("roadmap_add_chunk", a).await.is_error, Some(true));

    // start_chunk → in_progress + the chunk:<id> backref for ship/think.
    let mut s = serde_json::Map::new();
    s.insert("id".to_string(), Value::String("phase-1".to_string()));
    let started = call("roadmap_start_chunk", s).await;
    let sc = started.structured_content.expect("start structured");
    assert_eq!(sc["backref"], "chunk:phase-1");
    assert_eq!(sc["chunk"]["status"], "in_progress");

    // link a think step to the chunk.
    let mut l = serde_json::Map::new();
    l.insert("id".to_string(), Value::String("phase-1".to_string()));
    l.insert(
        "cross_ref".to_string(),
        Value::String("think:1".to_string()),
    );
    assert_ne!(call("roadmap_link", l).await.is_error, Some(true));

    // complete with a proof-of-ship ref.
    let mut c = serde_json::Map::new();
    c.insert("id".to_string(), Value::String("phase-1".to_string()));
    c.insert(
        "ship_ref".to_string(),
        Value::String("task:wire-it".to_string()),
    );
    let done = call("roadmap_complete_chunk", c).await;
    let dc = done.structured_content.expect("complete structured");
    assert_eq!(dc["status"], "done");
    let refs = dc["cross_refs"].as_array().unwrap();
    assert!(refs.iter().any(|r| r == "think:1"));
    assert!(refs.iter().any(|r| r == "task:wire-it"));

    // record a refresh note.
    let mut r = serde_json::Map::new();
    r.insert(
        "summary".to_string(),
        Value::String("shipped phase-1".to_string()),
    );
    r.insert(
        "think_steps".to_string(),
        Value::Array(vec![Value::from(8)]),
    );
    let refreshed = call("roadmap_record_refresh", r).await;
    let rc = refreshed.structured_content.expect("refresh structured");
    assert_eq!(rc["total_refreshes"], 1);

    let _ = client_service.cancel().await;
    server_handle.abort();
}

/// mcp-resources: a real rmcp client lists and reads the three read-only
/// resources over the wire. Resource reads must reflect engine state and
/// never mutate it.
#[tokio::test]
async fn resources_list_and_read_end_to_end() {
    use rmcp::model::ReadResourceRequestParams;

    let (client_service, server_handle) = pair().await;
    let peer = client_service.peer();

    // Capabilities advertise resources (and the listing has our three URIs).
    let listed = peer.list_resources(None).await.expect("resources/list");
    let uris: Vec<&str> = listed.resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(uris.contains(&"roadmap://view"), "got {uris:?}");
    assert!(uris.contains(&"decisions://pinned"), "got {uris:?}");
    assert!(uris.contains(&"digest://since/24h"), "got {uris:?}");

    let templates = peer
        .list_resource_templates(None)
        .await
        .expect("resources/templates/list");
    assert!(
        templates
            .resource_templates
            .iter()
            .any(|t| t.uri_template == "digest://since/{window}"),
    );

    // Seed state through the normal tool surface: one pinned step + a chunk.
    let mut step = serde_json::Map::new();
    for (k, v) in [
        ("purpose", "anchor decision"),
        ("context", "ctx"),
        ("thought", "the load-bearing conclusion"),
        ("outcome", "decided X"),
        ("next_action", "n/a"),
        ("rationale", "because"),
    ] {
        step.insert(k.into(), Value::String(v.into()));
    }
    step.insert("step_number".into(), Value::from(1));
    step.insert("estimated_total".into(), Value::from(1));
    step.insert("pinned".into(), Value::Bool(true));
    let req = CallToolRequestParams::new("think_record_step").with_arguments(step);
    peer.call_tool(req).await.expect("record step");

    let mut chunk = serde_json::Map::new();
    chunk.insert("id".into(), Value::String("res-e2e".into()));
    chunk.insert("title".into(), Value::String("resource e2e chunk".into()));
    let req = CallToolRequestParams::new("roadmap_add_chunk").with_arguments(chunk);
    peer.call_tool(req).await.expect("add chunk");

    let read = |uri: &str| {
        let peer = peer.clone();
        let uri = uri.to_string();
        async move {
            let res = peer
                .read_resource(ReadResourceRequestParams::new(uri))
                .await
                .expect("resources/read");
            match res.contents.into_iter().next().expect("one content") {
                rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
                other => panic!("expected text contents, got {other:?}"),
            }
        }
    };

    let roadmap_md = read("roadmap://view").await;
    assert!(roadmap_md.contains("res-e2e") || roadmap_md.contains("resource e2e chunk"));

    let pinned_md = read("decisions://pinned").await;
    assert!(pinned_md.contains("anchor decision"), "got: {pinned_md}");

    let digest_md = read("digest://since/24h").await;
    assert!(digest_md.contains("anchor decision"), "got: {digest_md}");
    assert!(digest_md.contains("res-e2e"), "got: {digest_md}");

    // Unknown URI and bad window are clean errors, not crashes.
    assert!(
        peer.read_resource(ReadResourceRequestParams::new("nope://x"))
            .await
            .is_err()
    );
    assert!(
        peer.read_resource(ReadResourceRequestParams::new("digest://since/banana"))
            .await
            .is_err()
    );

    let _ = client_service.cancel().await;
    server_handle.abort();
}

/// MCP 2026 readiness: a client that declares MCP `2026-07-28` must be
/// able to drive this server, end to end.
///
/// WHAT THIS PROVES, precisely — the wording matters because a run against
/// deliberately broken code caught the first version of this test claiming
/// more than it showed.
/// rmcp's `negotiate_protocol_version` echoes any version found in the GLOBAL
/// `ProtocolVersion::KNOWN_VERSIONS` constant; it does **not** consult this
/// server's `supported_protocol_versions()`. So the `protocol_version`
/// assertion below is evidence that the SDK will carry a 2026-07-28
/// connection and that our handlers work over one — it is NOT evidence that
/// we declare support. That declaration is a separate claim with its own
/// test (`we_declare_support_for_2026_07_28` in tests/unified_service.rs),
/// because neither implies the other.
///
/// The round trip is the load-bearing part: negotiate at `2026-07-28`, list
/// tools, then complete a real `tools/call`.
#[tokio::test]
async fn a_2026_07_28_client_negotiates_and_calls_a_tool() {
    use rmcp::model::{ClientCapabilities, ClientInfo, Implementation, ProtocolVersion};

    #[derive(Clone)]
    struct Client2026;
    impl ClientHandler for Client2026 {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::new(
                ClientCapabilities::default(),
                Implementation::new("think-and-ship-spec-test", "0.0.0"),
            )
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
        }
    }

    let server = build_unified();
    let (server_tx, client_tx) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });
    let client = Client2026
        .serve(client_tx)
        .await
        .expect("a 2026-07-28 client must be able to connect");

    // The server met us at the version we asked for rather than falling back.
    let info = client.peer_info().expect("peer info");
    assert_eq!(
        info.protocol_version,
        ProtocolVersion::V_2026_07_28,
        "the SDK failed to carry a 2026-07-28 connection; negotiated {} instead",
        info.protocol_version
    );

    // …and the connection actually works: list, then call.
    let listed = client.list_tools(None).await.expect("tools/list");
    assert!(
        listed.tools.iter().any(|t| t.name == "roadmap_status"),
        "expected roadmap_status in the 2026-07-28 tools/list"
    );
    let called = client
        .peer()
        .call_tool(CallToolRequestParams::new("roadmap_status"))
        .await
        .expect("tools/call over a 2026-07-28 connection");
    assert_ne!(called.is_error, Some(true), "roadmap_status errored");

    let _ = client.cancel().await;
    server_handle.abort();
}
