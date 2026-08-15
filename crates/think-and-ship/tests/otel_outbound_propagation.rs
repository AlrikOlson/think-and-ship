//! Outbound OTel context propagation: the downstream half of SEP-414.
//!
//! The W3C spec is normative on the obligation and permissive on the shape — "a
//! vendor receiving a `traceparent` request header MUST send it to outgoing
//! requests. It MAY mutate the value" — so what is worth testing is not "is
//! there a header" but the two things the MAY decides:
//!
//! 1. **The header is actually on the wire**, asserted against
//!    `server.received_requests()` rather than against a field we set. A field
//!    that never reaches a header is the exact failure this suite exists to
//!    catch, one layer down.
//! 2. **No context means NO header**, never a fabricated root. This is asserted
//!    as a positive absence on a real received request, because "we did not
//!    add it" and "the request did not carry it" are different claims and only
//!    the second one matters.
//!
//! The derivation itself — that the parent-id we send is the span id the
//! offline export emits — is pinned in `trace_context.rs`'s unit tests, where
//! the persistence handle can be pointed at a scratch directory without
//! mutating process-global environment.

use think_and_ship::tracker::github::GithubTracker;
use think_and_ship::tracker::linear::LinearTracker;
use think_and_ship::tracker::port::TrackerPort;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A well-formed child header of the shape `outbound_traceparent` mints: the
/// caller's trace id, our own span id, sampled.
const CHILD: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-a1b2c3d4e5f60718-01";

/// The header the caller sent US. It must never be what we send onward — that
/// is the rejected pass-through option, and it would make the downstream leg a
/// sibling of our root rather than a child of it.
const CALLERS_OWN: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

async fn traceparents(server: &MockServer) -> Vec<Option<String>> {
    server
        .received_requests()
        .await
        .expect("mock server records requests")
        .iter()
        .map(|r| {
            r.headers
                .get("traceparent")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn a_github_request_carries_the_minted_child_traceparent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 12, "id": 900, "state": "open", "updated_at": "2026-07-25T09:00:00Z",
        })))
        .mount(&server)
        .await;

    let gh = GithubTracker::new("owner/repo")
        .expect("valid target")
        .with_api_base(&server.uri())
        .with_traceparent(Some(CHILD.to_string()));
    let _ = gh.fetch_one("owner/repo#12").await;

    let seen = traceparents(&server).await;
    assert_eq!(seen.len(), 1, "exactly one request was made");
    assert_eq!(
        seen[0].as_deref(),
        Some(CHILD),
        "the GitHub leg must carry the child traceparent"
    );
    assert_ne!(
        seen[0].as_deref(),
        Some(CALLERS_OWN),
        "pass-through was the REJECTED option"
    );
}

#[tokio::test]
async fn a_github_request_without_a_context_carries_no_traceparent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "number": 12, "id": 900, "state": "open", "updated_at": "2026-07-25T09:00:00Z",
        })))
        .mount(&server)
        .await;

    // The default: no caller context was ever adopted.
    let gh = GithubTracker::new("owner/repo")
        .expect("valid target")
        .with_api_base(&server.uri());
    let _ = gh.fetch_one("owner/repo#12").await;

    let seen = traceparents(&server).await;
    assert_eq!(seen.len(), 1, "the request was still made");
    assert_eq!(
        seen[0], None,
        "an unparented call must not acquire a fabricated root"
    );
}

#[tokio::test]
async fn a_linear_request_carries_the_minted_child_traceparent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "data": { "teams": { "nodes": [] } } })),
        )
        .mount(&server)
        .await;

    let linear = LinearTracker::new("ENG")
        .expect("valid target")
        .with_api_base(&server.uri())
        .with_traceparent(Some(CHILD.to_string()));
    let _ = linear.fetch_one("ENG-1").await;

    let seen = traceparents(&server).await;
    assert!(!seen.is_empty(), "at least one GraphQL call was made");
    assert!(
        seen.iter().all(|h| h.as_deref() == Some(CHILD)),
        "EVERY Linear call must carry it, not just the first: {seen:?}"
    );
}

#[tokio::test]
async fn a_linear_request_without_a_context_carries_no_traceparent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "data": { "teams": { "nodes": [] } } })),
        )
        .mount(&server)
        .await;

    let linear = LinearTracker::new("ENG")
        .expect("valid target")
        .with_api_base(&server.uri());
    let _ = linear.fetch_one("ENG-1").await;

    let seen = traceparents(&server).await;
    assert!(!seen.is_empty(), "the call was still made");
    assert!(
        seen.iter().all(Option::is_none),
        "an unparented call must not acquire a fabricated root: {seen:?}"
    );
}
