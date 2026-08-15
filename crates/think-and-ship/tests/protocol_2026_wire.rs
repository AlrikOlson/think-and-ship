//! What a strict `2026-07-28` client sees on the wire.
//!
//! This file exists because the rmcp client cannot see the bug it guards.
//! `a_2026_07_28_client_negotiates_and_calls_a_tool` (tests/think_and_ship_e2e.rs)
//! pairs the server with rmcp's own client, which deserializes SEP-2549's
//! `ttlMs` / `cacheScope` into `Option` and is perfectly happy with `None`. A
//! real client validating against the `2026-07-28` schema — where both are
//! required — rejects the whole `tools/list` result instead, and a rejected
//! list means every tool on the server vanishes. That failure was live while
//! the rmcp-client test was green, so the gate has to read the bytes.
//!
//! Hence a hand-rolled JSON-RPC driver over the same newline-delimited
//! transport the stdio deployment uses: no SDK types on the client side,
//! nothing to paper over an absent field.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

use rmcp::ServiceExt;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// The revision under test. Both fields are optional before it and required
/// from it onward, so it is the version that makes this file's claims bite.
const PROTOCOL: &str = "2026-07-28";

/// Every response must arrive well inside this; it only exists so a regression
/// that deadlocks the handler fails the suite instead of hanging it.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

fn build_unified() -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    UnifiedService::new(
        ThinkService::new(ReasoningServer::new(cfg)),
        ShipService::new(ShipEngine::new("test-wire".into())),
        RoadmapService::new(RoadmapEngine::new("test-wire".into())),
        SignalService::new(SignalEngine::new("test-wire".into())),
    )
}

/// A JSON-RPC peer that knows nothing about MCP beyond the framing.
struct RawClient {
    writer: WriteHalf<DuplexStream>,
    reader: BufReader<ReadHalf<DuplexStream>>,
    next_id: i64,
}

impl RawClient {
    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_string(&message).expect("serialize request");
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("write request");
        self.writer.flush().await.expect("flush request");
    }

    /// Send a request and return its `result` object.
    ///
    /// Reads until the response with the matching id turns up: the server is
    /// free to interleave notifications, and skipping them here is what keeps
    /// this driver from being sensitive to unrelated wire traffic.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;

        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(REPLY_TIMEOUT, self.reader.read_line(&mut line))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for a response to {method}"))
                .expect("read response");
            assert_ne!(read, 0, "server closed the transport during {method}");

            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("{method} produced non-JSON line {line:?}: {e}"));
            if message.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                panic!("{method} returned an error: {error}");
            }
            return message
                .get("result")
                .unwrap_or_else(|| panic!("{method} response carried neither result nor error"))
                .clone();
        }
    }
}

/// Handshake at [`PROTOCOL`] and hand back a driver on an initialized session.
async fn connect() -> (RawClient, tokio::task::JoinHandle<()>) {
    let server = build_unified();
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let handle = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });

    let (read_half, writer) = tokio::io::split(client_io);
    let mut client = RawClient {
        writer,
        reader: BufReader::new(read_half),
        next_id: 1,
    };

    let initialized = client
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "strict-wire-probe", "version": "0.0.0"},
            }),
        )
        .await;

    // The premise of every assertion below. If this ever fails, the server has
    // started negotiating down — at which point the fields stop being required
    // and these tests are asking the wrong question, so fail loudly here
    // rather than quietly passing for the wrong reason.
    assert_eq!(
        initialized["protocolVersion"], PROTOCOL,
        "server did not agree to {PROTOCOL}; it answered {}",
        initialized["protocolVersion"]
    );

    client
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;

    (client, handle)
}

/// Exactly the two keys Claude Code's validator reported missing.
fn assert_cacheable(method: &str, result: &Value) {
    let ttl = result.get("ttlMs").unwrap_or_else(|| {
        panic!("{method} result has no `ttlMs`; a {PROTOCOL} client discards it. Got: {result:#}")
    });
    assert!(
        ttl.is_u64(),
        "{method} `ttlMs` must be a non-negative number, got {ttl}"
    );

    let scope = result.get("cacheScope").unwrap_or_else(|| {
        panic!(
            "{method} result has no `cacheScope`; a {PROTOCOL} client discards it. Got: {result:#}"
        )
    });
    assert!(
        matches!(scope.as_str(), Some("public") | Some("private")),
        "{method} `cacheScope` must be \"public\" or \"private\", got {scope}"
    );
}

/// The headline failure: 53 tools dropped because one result lacked two keys.
#[tokio::test]
async fn tools_list_carries_the_cache_metadata_2026_requires() {
    let (mut client, handle) = connect().await;

    let result = client.request("tools/list", json!({})).await;
    assert_cacheable("tools/list", &result);
    assert!(
        result["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()),
        "tools/list returned no tools at all"
    );

    handle.abort();
}

/// `resources/list` failed identically in the report, and its templates
/// sibling shares the same result shape — so it fails the same way unobserved.
#[tokio::test]
async fn every_list_result_carries_the_cache_metadata() {
    let (mut client, handle) = connect().await;

    for method in ["resources/list", "resources/templates/list"] {
        let result = client.request(method, json!({})).await;
        assert_cacheable(method, &result);
    }

    handle.abort();
}

/// `resources/read` carries the same two required fields, and nothing in the
/// original report exercised it — it would have been the next thing to break.
#[tokio::test]
async fn read_resource_carries_the_cache_metadata() {
    let (mut client, handle) = connect().await;

    let result = client
        .request("resources/read", json!({"uri": "roadmap://view"}))
        .await;
    assert_cacheable("resources/read", &result);

    // Live project state must never sit in a shared cache, and it is stale as
    // soon as any tool call moves the roadmap.
    assert_eq!(result["cacheScope"], "private");
    assert_eq!(result["ttlMs"], 0);

    handle.abort();
}
