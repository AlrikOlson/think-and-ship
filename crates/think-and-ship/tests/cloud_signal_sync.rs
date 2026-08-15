//! The signal dogfood loop: a local signal capture pushes to the
//! cloud backend. Mirrors the roadmap proof (cloud_roadmap_sync.rs).

use std::time::Duration;

use think_and_ship::cloud::client::CloudClient;
use think_and_ship::signal::SignalEngine;
use think_and_ship::signal::domain::SignalKind;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

#[tokio::test]
async fn a_signal_capture_pushes_to_the_cloud() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let mut engine =
        SignalEngine::new("dogfood".into()).with_cloud(CloudClient::new(server.uri(), "tok"));
    engine.capture(SignalKind::Bug, "dana".into(), "it crashes on save".into());

    // The push is fire-and-forget on a detached task, so this waits on the
    // request ACTUALLY ARRIVING rather than on a duration guessed against it.
    // The budget bounds failure only — see support::wait_for_request.
    support::wait_for_request(&server, "/v1/records", Duration::from_secs(10))
        .await
        .expect("the capture should have pushed to /v1/records");
}
