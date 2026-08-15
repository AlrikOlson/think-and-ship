//! sync-think-reconcile (31d-a) — think pull/reconcile + the startup hydrate.
//!
//! The think trace merges by the ONE rule the disk merge already uses
//! (`merge_histories`): insert-if-absent by `step_number`, the local copy
//! NEVER replaced — so a stale cloud copy cannot clobber local reasoning
//! (the think twin of the reconcile-recency-guard race). `reconcile_all` is
//! the one-shot boot hydrate across think + roadmap + signal.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use think_and_ship::cloud::build::from_step;
use think_and_ship::cloud::client::CloudClient;
use think_and_ship::cloud::pull::{apply_think_records, reconcile_all, reconcile_think};
use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::signal::SignalEngine;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::domain::{NextAction, ThinkStep};
use think_and_ship::think::engine::core::ReasoningServer;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn quiet_server() -> ReasoningServer {
    let mut c = ThinkConfig::default();
    c.display.color_output = false;
    // Same project the fixtures stamp — reconcile filters by record.project_id
    // (sync-project-scope), so the engine and the envelopes must agree.
    ReasoningServer::new_for_project(c, "t".into())
}

fn step(n: u32, thought: &str) -> ThinkStep {
    ThinkStep {
        step_number: n,
        estimated_total: 10,
        purpose: format!("step {n}"),
        context: "ctx".into(),
        thought: thought.into(),
        outcome: "out".into(),
        next_action: NextAction::Text("next".into()),
        rationale: "why".into(),
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

fn envelope_for(n: u32, thought: &str) -> Value {
    serde_json::to_value(from_step("t", &step(n, thought))).unwrap()
}

#[test]
fn adopts_absent_steps_and_sorts_the_trace() {
    let mut engine = quiet_server();
    engine.adopt_steps(vec![step(5, "local five")]);

    let adopted = apply_think_records(
        &mut engine,
        &[
            envelope_for(3, "cloud three"),
            envelope_for(7, "cloud seven"),
        ],
    );
    assert_eq!(adopted, 2);

    let numbers: Vec<u32> = engine
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert_eq!(numbers, vec![3, 5, 7], "trace stays sorted by step_number");
}

/// THE RACE (think twin of reconcile-recency-guard): a refresh delivers the
/// cloud copy of a step this process already holds — possibly staler than the
/// local copy. The local step must survive untouched.
#[test]
fn an_existing_local_step_is_never_replaced_by_a_cloud_copy() {
    let mut engine = quiet_server();
    engine.adopt_steps(vec![step(42, "local, freshly revised reasoning")]);

    let adopted = apply_think_records(&mut engine, &[envelope_for(42, "stale cloud copy")]);
    assert_eq!(adopted, 0, "an already-known step adopts nothing");

    let s = &engine.history().steps[0];
    assert_eq!(s.thought, "local, freshly revised reasoning");
}

#[test]
fn malformed_records_are_skipped_not_fatal() {
    let mut engine = quiet_server();
    let adopted = apply_think_records(
        &mut engine,
        &[
            // Stamped for this project so it reaches (and fails) deserialization.
            json!({ "record": { "step_number": "not-a-number", "project_id": "t" } }),
            json!({ "no_record": true }),
            envelope_for(1, "good"),
        ],
    );
    assert_eq!(adopted, 1);
}

/// sync-project-scope: an org tenant holds EVERY project's records; a step
/// stamped for another project — or an unstamped legacy record whose origin
/// is unknowable — must never merge in. Step numbers are project-global, so
/// adopting a foreign step silently poisons this project's numbering (a
/// bleed observed live between two projects sharing one tenant).
#[test]
fn foreign_and_unstamped_records_never_merge() {
    let mut engine = quiet_server();
    let foreign = serde_json::to_value(from_step("other-project", &step(9, "theirs"))).unwrap();
    let mut unstamped = envelope_for(11, "pre-stamp legacy");
    unstamped["record"]
        .as_object_mut()
        .unwrap()
        .remove("project_id");

    let adopted = apply_think_records(&mut engine, &[foreign, unstamped, envelope_for(2, "ours")]);
    assert_eq!(adopted, 1, "only this project's stamped record merges");
    let numbers: Vec<u32> = engine
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert_eq!(numbers, vec![2]);
}

#[tokio::test]
async fn reconcile_think_pulls_cloud_steps_into_the_local_engine() {
    let server = MockServer::start().await;
    let records: Vec<Value> = vec![envelope_for(1, "one"), envelope_for(2, "two")];
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "think"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": records })))
        .mount(&server)
        .await;

    let client = CloudClient::new(server.uri(), "tok");
    let mut engine = quiet_server();

    assert_eq!(reconcile_think(&client, &mut engine).await.unwrap(), 2);
    assert_eq!(engine.history().steps.len(), 2);

    // Re-pull is a no-op (insert-if-absent), not a duplicate.
    assert_eq!(reconcile_think(&client, &mut engine).await.unwrap(), 0);
    assert_eq!(engine.history().steps.len(), 2);
}

/// The startup hydrate: one call converges all three pull-able families, and
/// a failing family (signal here: 500) never blocks the others.
#[tokio::test]
async fn reconcile_all_hydrates_every_family_and_tolerates_one_failing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "think"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "records": [envelope_for(1, "one")] })),
        )
        .mount(&server)
        .await;
    let chunk = json!({
        "record": {
            "id": "c1", "title": "T", "status": "backlog", "priority": 1,
            "description": "", "notes": "", "acceptance": [], "deps": [],
            "cross_refs": [], "shared": false, "project_id": "t",
            "created_at": "2026-06-08T00:00:00Z", "updated_at": "2026-06-08T00:00:00Z"
        }
    });
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "roadmap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": [chunk] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "signal"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = CloudClient::new(server.uri(), "tok");
    let think = Arc::new(Mutex::new(quiet_server()));
    let roadmap = Arc::new(Mutex::new(RoadmapEngine::new("t".into())));
    let signal = Arc::new(Mutex::new(SignalEngine::new("t".into())));

    let (t, r, s) = reconcile_all(&client, &think, &roadmap, &signal).await;
    assert_eq!((t, r, s), (1, 1, 0));
    assert_eq!(think.lock().unwrap().history().steps.len(), 1);
    assert_eq!(roadmap.lock().unwrap().roadmap().chunks.len(), 1);
}
