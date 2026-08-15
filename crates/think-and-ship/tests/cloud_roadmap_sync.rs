//! The roadmap dogfood loop: a local chunk mutation pushes to the
//! cloud backend. The wiremock test proves the engine→cloud wiring
//! deterministically; the #[ignore]'d test confirms it against a live deploy.

use std::time::Duration;

use think_and_ship::cloud::client::CloudClient;
use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::roadmap::domain::ChunkStatus;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

#[tokio::test]
async fn a_roadmap_mutation_pushes_to_the_cloud() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/v1/records"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let mut engine =
        RoadmapEngine::new("dogfood".into()).with_cloud(CloudClient::new(server.uri(), "tok"));
    engine
        .add_chunk(
            "c1".into(),
            "Title".into(),
            ChunkStatus::Backlog,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .expect("add_chunk");

    // The push is fire-and-forget on a detached task, so this waits on the
    // request ACTUALLY ARRIVING rather than on a duration guessed against it.
    // The budget bounds failure only — see support::wait_for_request.
    support::wait_for_request(&server, "/v1/records", Duration::from_secs(10))
        .await
        .expect("the mutation should have pushed to /v1/records");
}

/// Live end-to-end: mutate a chunk locally with a real cloud client, then read
/// it back from the cloud to prove it landed. Gated on env so it never runs
/// unprompted (the engine's project_id must equal the token's `tenant` claim).
#[tokio::test]
#[ignore = "requires THINK_AND_SHIP_CLOUD_URL/_TOKEN (a live backend)"]
async fn a_roadmap_mutation_reaches_the_live_cloud() {
    let url = std::env::var("THINK_AND_SHIP_CLOUD_URL").expect("THINK_AND_SHIP_CLOUD_URL");
    let token = std::env::var("THINK_AND_SHIP_CLOUD_TOKEN").expect("THINK_AND_SHIP_CLOUD_TOKEN");
    let tenant =
        std::env::var("THINK_AND_SHIP_CLOUD_TENANT").unwrap_or_else(|_| "rust-live".into());
    let id = format!("dogfood-{}", std::process::id());

    let mut engine =
        RoadmapEngine::new(tenant).with_cloud(CloudClient::new(url.clone(), token.clone()));
    engine
        .add_chunk(
            id.clone(),
            "live".into(),
            ChunkStatus::Backlog,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .expect("add_chunk");
    // Same shape as the wiremock twin, one hop further out: the condition is
    // the cloud actually SERVING the chunk back. A fixed sleep here was worse
    // than in the mock test, because a real network's tail is unbounded — the
    // one place a guessed constant is guaranteed to be wrong eventually.
    let endpoint = format!(
        "{}/v1/records/roadmap/chunk/{id}",
        url.trim_end_matches('/')
    );
    let http = reqwest::Client::new();
    support::wait_until(
        Duration::from_secs(30),
        "the chunk becoming readable",
        || async {
            http.get(&endpoint)
                .bearer_auth(&token)
                .send()
                .await
                .is_ok_and(|r| r.status().as_u16() == 200)
        },
    )
    .await
    .expect("the chunk should be readable in the cloud");
}
