//! The think dogfood loop: recording a reasoning step pushes its
//! envelope to the cloud backend. Completes local→cloud push for all 4 families.

use std::time::Duration;

use think_and_ship::cloud::client::CloudClient;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::domain::{NextAction, ThinkStep};
use think_and_ship::think::engine::core::ReasoningServer;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

fn quiet_config() -> ThinkConfig {
    let mut c = ThinkConfig::default();
    c.display.color_output = false;
    c
}

fn step(n: u32) -> ThinkStep {
    ThinkStep {
        step_number: n,
        estimated_total: 5,
        purpose: "analysis".into(),
        context: format!("ctx {n}"),
        thought: format!("thought {n}"),
        outcome: format!("outcome {n}"),
        next_action: NextAction::Text(format!("next {n}")),
        rationale: format!("rationale {n}"),
        confidence: None,
        uncertainty_notes: None,
        revises_step: None,
        revision_reason: None,
        revised_by: None,
        is_final_step: None,
        branch_from: None,
        branch_id: None,
        branch_name: None,
        tools_used: None,
        dependencies: None,
        timestamp: None,
        duration_ms: None,
        session_id: None,
        pinned: None,
        cwd: None,
        execution_ref: None,
        record_id: None,
    }
}

#[tokio::test]
async fn a_think_step_pushes_to_the_cloud() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let mut engine =
        ReasoningServer::new(quiet_config()).with_cloud(CloudClient::new(server.uri(), "tok"));
    engine.process_step(step(1)).expect("process_step");

    // The push is fire-and-forget on a detached task, so this waits on the
    // request ACTUALLY ARRIVING rather than on a duration guessed against it.
    // The budget bounds failure only — see support::wait_for_request.
    support::wait_for_request(&server, "/v1/records", Duration::from_secs(10))
        .await
        .expect("the recorded step should have pushed to /v1/records");
}
