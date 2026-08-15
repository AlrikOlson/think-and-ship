//! The GitHub App manifest exchange, proven against a mock.
//!
//! The unit tests beside the module cover the pure half (the permission set, the
//! manifest, the form). This file covers the half that talks: the conversion is
//! unauthenticated and single-shot, a stale code is refused with a message that
//! names the one-hour window, and a body that echoes the code does not print it.
//!
//! The no-secret-in-logs proof lives in `tracker_credential_github_app_logs.rs`,
//! alone in its own test binary — it has to be, and that file says why.

use think_and_ship::tracker::credential::github_app::{
    self, AppManifest, DEFAULT_PERMISSIONS, Owner,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The sentinels. Distinctive enough that a substring scan cannot match them by
/// accident, and long enough that a truncated log line still shows the prefix.
const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----SENTINEL-PEM-MATERIAL-9f13";
const CLIENT_SECRET: &str = "SENTINEL-CLIENT-SECRET-4d7a";
const WEBHOOK_SECRET: &str = "SENTINEL-WEBHOOK-SECRET-b0c2";
const CODE: &str = "SENTINEL-MANIFEST-CODE-77ab";

fn conversion_body() -> serde_json::Value {
    serde_json::json!({
        "id": 654_321,
        "slug": "think-and-ship-dogfood",
        "node_id": "MDM6SW50ZWdyYXRpb24x",
        "name": "think-and-ship",
        "html_url": "https://github.com/apps/think-and-ship-dogfood",
        "client_id": "Iv1.0000public0000",
        "client_secret": CLIENT_SECRET,
        "pem": PEM,
        "webhook_secret": WEBHOOK_SECRET,
        "permissions": {
            "issues": "write",
            "metadata": "read",
            "organization_projects": "write",
        },
        "events": ["issues"],
    })
}

async fn server_returning(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/app-manifests/{CODE}/conversions")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// The whole point of the flow: one temporary code becomes the entire credential
/// set, machine to machine, with nothing copied out of a browser.
#[tokio::test]
async fn a_temporary_code_becomes_the_id_the_pem_the_webhook_secret_and_the_client_secret() {
    let server = server_returning(201, conversion_body()).await;

    let app = github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect("the conversion must succeed");

    assert_eq!(app.id, 654_321);
    assert_eq!(app.slug, "think-and-ship-dogfood");
    assert_eq!(app.client_id, "Iv1.0000public0000");
    assert_eq!(app.pem.expose(), PEM);
    assert_eq!(app.client_secret.expose(), CLIENT_SECRET);
    assert_eq!(
        app.webhook_secret
            .as_ref()
            .map(think_and_ship::tracker::credential::Secret::expose),
        Some(WEBHOOK_SECRET),
        "the webhook secret is the one a human never sees, so losing it silently is the failure \
         mode this asserts against"
    );
    // What GitHub recorded is exactly what the manifest asked for.
    assert!(app.unrequested_permissions().is_empty());
    assert_eq!(app.events, vec!["issues".to_string()]);
}

/// UNAUTHENTICATED and SINGLE-SHOT, asserted on what actually reached the wire
/// rather than on the return value. A conversion that sent a token would work
/// exactly as well and would be leaking one into a request that did not need it.
#[tokio::test]
async fn the_conversion_is_one_unauthenticated_request_and_the_code_travels_in_the_path() {
    let server = server_returning(201, conversion_body()).await;

    github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect("conversion");

    let requests = server
        .received_requests()
        .await
        .expect("the mock must record requests");
    assert_eq!(
        requests.len(),
        1,
        "the code is spendable exactly once — a retry burns it"
    );

    let req = &requests[0];
    assert_eq!(req.method, wiremock::http::Method::POST);
    assert!(req.url.path().contains(CODE), "got: {}", req.url.path());
    assert!(
        req.headers.get("authorization").is_none(),
        "this endpoint is unauthenticated; the code IS the credential"
    );
    assert!(
        req.body.is_empty(),
        "the conversion carries no request body"
    );
}

/// A stale code is the single most likely failure a human will hit, because the
/// browser step is where a person wanders off. The message has to say so.
#[tokio::test]
async fn an_expired_code_is_refused_with_a_message_naming_the_one_hour_window() {
    let server = server_returning(
        404,
        serde_json::json!({ "message": "Not Found", "documentation_url": "https://docs.github.com/rest" }),
    )
    .await;

    let err = github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect_err("an expired code must be refused");
    let msg = err.to_string();

    assert!(msg.contains("404"), "{msg}");
    assert!(msg.contains("ONE HOUR"), "{msg}");
    assert!(msg.contains("only once"), "{msg}");
    // Diagnosable: GitHub's own words survive, because a 422 here is usually a
    // manifest validation error and hiding it makes the flow unfixable.
    assert!(msg.contains("Not Found"), "{msg}");
}

/// The code is single-shot and is the whole credential set. GitHub echoing it
/// into an error body must not put it in front of a human.
#[tokio::test]
async fn a_rejection_that_echoes_the_code_does_not_print_it() {
    let server = server_returning(
        422,
        serde_json::json!({ "message": format!("code {CODE} was already converted") }),
    )
    .await;

    let msg = github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect_err("must refuse")
        .to_string();

    assert!(!msg.contains(CODE), "the manifest code leaked into: {msg}");
    assert!(msg.contains("«redacted»"), "{msg}");
    assert!(msg.contains("already converted"), "{msg}");
}

/// The manifest that actually goes on the wire, rendered end to end, with the
/// redirect the loopback receiver is really listening on.
///
/// `LoopbackReceiver::bind(0)` is correct HERE because GitHub exempts loopback
/// callbacks from port matching — a fact about GitHub's policy and
/// NOT about Atlassian's, whose half of the question is still open.
#[test]
fn the_form_carries_the_loopback_redirect_and_the_pinned_permissions() {
    let receiver = think_and_ship::tracker::credential::LoopbackReceiver::bind(0).expect("bind");
    let redirect = receiver.redirect_uri().expect("redirect uri");
    assert!(redirect.starts_with("http://127.0.0.1:"));
    assert!(
        !redirect.contains("localhost"),
        "RFC 8252 wants the loopback literal, and GitHub's carve-out is written for it"
    );

    let manifest = AppManifest::new(
        "think-and-ship",
        "https://example.com",
        "https://api.example.com/webhooks/github",
        &redirect,
    );
    let html = github_app::manifest_form_html(&manifest, &Owner::Personal, "state-1");

    assert!(html.contains(&format!("&quot;redirect_url&quot;:&quot;{redirect}&quot;")));
    for p in DEFAULT_PERMISSIONS {
        assert!(
            html.contains(&format!("&quot;{}&quot;:&quot;{}&quot;", p.key, p.level)),
            "{} missing from the form",
            p.key
        );
    }
    // Nothing beyond the pinned three ever reaches GitHub.
    let manifest_json = manifest.to_json();
    let permissions = manifest_json["default_permissions"]
        .as_object()
        .expect("object");
    assert_eq!(permissions.len(), 3, "got: {permissions:?}");
}

/// A body missing the private key is a conversion that did not deliver, and
/// treating it as success would store an app that can never mint a token.
#[tokio::test]
async fn a_response_without_the_pem_is_not_a_successful_conversion() {
    let mut body = conversion_body();
    body.as_object_mut().expect("object").remove("pem");
    let server = server_returning(201, body).await;

    let err = github_app::convert_manifest_at(&reqwest::Client::new(), &server.uri(), CODE)
        .await
        .expect_err("a conversion with no private key must not look like a success");
    assert!(err.to_string().contains("unreadable"), "{err}");
}
