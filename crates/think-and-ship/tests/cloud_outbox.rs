//! sync-offline-queue (31d-c) — failed pushes queue durably and flush when
//! connectivity returns. Capture lives inside `CloudClient::push` (the one
//! chokepoint), so every writer inherits it; replay is idempotent by the
//! envelope's idempotency key.

use std::sync::Arc;

use serde_json::json;
use think_and_ship::cloud::client::{CloudClient, PushOutcome};
use think_and_ship::cloud::envelope::{Family, Kind, UnifiedRecordEnvelope};
use think_and_ship::cloud::outbox::Outbox;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn envelope(id: &str) -> UnifiedRecordEnvelope {
    UnifiedRecordEnvelope::owner(
        "t",
        Family::Roadmap,
        Kind::Chunk,
        id,
        "2026-06-10T00:00:00Z",
        json!({ "id": id, "status": "backlog" }),
        vec![],
    )
}

#[tokio::test]
async fn an_offline_push_queues_and_flushes_on_recovery() {
    // The server is down for the first push (connection refused), then up.
    let outbox = Arc::new(Outbox::new(None));
    let offline = CloudClient::new("http://127.0.0.1:1", "tok").with_outbox(outbox.clone());

    assert!(offline.push(&envelope("c1")).await.is_err());
    assert_eq!(outbox.len(), 1, "transport failure queues the push");

    // Connectivity returns (a real server now) — same shared outbox.
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let online = CloudClient::new(server.uri(), "tok").with_outbox(outbox.clone());

    assert_eq!(online.flush_outbox().await, 1);
    assert!(outbox.is_empty(), "the queue drains on reconnect");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_5xx_queues_but_a_contract_rejection_does_not() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({ "error": "schema_invalid" })),
        )
        .mount(&server)
        .await;

    let outbox = Arc::new(Outbox::new(None));
    let client = CloudClient::new(server.uri(), "tok").with_outbox(outbox.clone());

    assert!(client.push(&envelope("c1")).await.is_err()); // 503 → queued
    assert_eq!(outbox.len(), 1);

    assert!(client.push(&envelope("c2")).await.is_err()); // 422 → NOT queued
    assert_eq!(outbox.len(), 1, "a contract rejection would fail forever");
}

#[tokio::test]
async fn flush_stops_on_failure_and_drops_rejected_entries() {
    // Two queued entries; the server 422s the first replay (drop + continue)
    // and accepts the second.
    let outbox = Arc::new(Outbox::new(None));
    let offline = CloudClient::new("http://127.0.0.1:1", "tok").with_outbox(outbox.clone());
    assert!(offline.push(&envelope("c1")).await.is_err());
    assert!(offline.push(&envelope("c2")).await.is_err());
    assert_eq!(outbox.len(), 2);

    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(json!({ "error": "schema_invalid" })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let online = CloudClient::new(server.uri(), "tok").with_outbox(outbox.clone());

    assert_eq!(online.flush_outbox().await, 2); // 1 dropped-as-rejected + 1 delivered
    assert!(outbox.is_empty());
}

#[tokio::test]
async fn a_successful_push_never_touches_the_queue() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let outbox = Arc::new(Outbox::new(None));
    let client = CloudClient::new(server.uri(), "tok").with_outbox(outbox.clone());

    assert_eq!(
        client.push(&envelope("c1")).await.unwrap(),
        PushOutcome::Created
    );
    assert!(outbox.is_empty());
    assert_eq!(client.flush_outbox().await, 0);
}
