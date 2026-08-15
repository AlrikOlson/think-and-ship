//! The ship dogfood loop (action slice): recording a ship action
//! pushes its envelope to the cloud backend. Mirrors the roadmap/signal proofs.

use std::time::Duration;

use think_and_ship::cloud::client::CloudClient;
use think_and_ship::ship::domain::action::ActionType;
use think_and_ship::ship::domain::task::TaskType;
use think_and_ship::ship::engine::ShipEngine;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

#[tokio::test]
async fn a_ship_action_pushes_to_the_cloud() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let mut engine =
        ShipEngine::new("dogfood".into()).with_cloud(CloudClient::new(server.uri(), "tok"));
    // The objective/task frames are id-only and don't push; only the action does.
    engine.set_objective("o".into(), vec![], vec![], "s".into());
    engine.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    engine.start_task("t1").expect("start");
    engine
        .record_action(
            Some("t1"),
            ActionType::Code,
            "did the thing".into(),
            vec![],
            vec!["Edit".into()],
            String::new(),
            Some(907),
        )
        .expect("record_action");

    // The push is fire-and-forget on a detached task, so this waits on the
    // request ACTUALLY ARRIVING rather than on a duration guessed against it.
    // The budget bounds failure only — see support::wait_for_request.
    support::wait_for_request(&server, "/v1/records", Duration::from_secs(10))
        .await
        .expect("the action should have pushed to /v1/records");
}
