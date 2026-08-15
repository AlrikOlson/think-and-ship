//! Roadmap pull/reconcile: the cloud's roadmap chunks merge into
//! the local engine, making the local store a cache. NEWEST-wins per chunk
//! (reconcile-recency-guard): a stale cloud copy must never clobber a newer
//! local mutation — a race observed live, where a realtime
//! refresh raced a mutation's own fire-and-forget push and wiped a cross-ref.

use serde_json::{Value, json};
use think_and_ship::cloud::build::from_chunk;
use think_and_ship::cloud::client::CloudClient;
use think_and_ship::cloud::pull::reconcile_roadmap;
use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::roadmap::domain::{Chunk, ChunkStatus};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chunk(id: &str, status: ChunkStatus) -> Chunk {
    chunk_at(id, status, "2026-06-08T00:00:00Z")
}

/// A chunk that PROVABLY belongs to `project`. Ownership has to be stated in
/// the fixture now: the push no longer supplies it for an unstamped chunk, so a
/// test that wants "ours" must say so rather than letting the pusher's identity
/// stand in — which is the very laundering this suite guards against.
fn owned_chunk(id: &str, status: ChunkStatus, project: &str) -> Chunk {
    Chunk {
        project_id: Some(project.into()),
        ..chunk(id, status)
    }
}

fn chunk_at(id: &str, status: ChunkStatus, updated_at: &str) -> Chunk {
    Chunk {
        tier: None,
        id: id.into(),
        title: format!("Title {id}"),
        name: "node".into(),
        status,
        priority: 100,
        description: String::new(),
        content: None,
        group: None,
        notes: String::new(),
        acceptance: vec![],
        deps: vec![],
        cross_refs: vec![],
        shared: false,
        reprioritize: None,
        status_proposal: None,
        title_proposal: None,
        obsoleted_reason: None,
        blocked_by: None,
        project_id: None,
        created_at: "2026-06-08T00:00:00Z".into(),
        updated_at: updated_at.into(),
    }
}

#[test]
fn upsert_chunk_replaces_by_id_or_inserts() {
    let mut engine = RoadmapEngine::new("t".into());
    engine.upsert_chunk(chunk("a", ChunkStatus::Backlog));
    engine.upsert_chunk(chunk("b", ChunkStatus::Backlog));
    assert_eq!(engine.roadmap().chunks.len(), 2);

    // A NEWER cloud copy replaces 'a' — no duplicate, status updated.
    engine.upsert_chunk(chunk_at("a", ChunkStatus::Done, "2026-06-08T01:00:00Z"));
    assert_eq!(engine.roadmap().chunks.len(), 2);
    let a = engine
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "a")
        .unwrap();
    assert!(matches!(a.status, ChunkStatus::Done));
}

/// THE OBSERVED RACE: a local mutation lands, then a refresh
/// delivers the cloud copy from BEFORE that mutation's push caught up. The
/// stale copy must not clobber the newer local state.
#[test]
fn upsert_chunk_keeps_a_newer_local_mutation_over_a_stale_cloud_copy() {
    let mut engine = RoadmapEngine::new("t".into());

    // The local chunk, freshly mutated (a cross-ref attached at T2).
    let mut local = chunk_at("a", ChunkStatus::InProgress, "2026-06-08T02:00:00Z");
    local.cross_refs = vec!["task:verify".into()];
    engine.upsert_chunk(local);

    // The reconcile delivers the cloud copy from T1 — before the push landed.
    engine.upsert_chunk(chunk_at(
        "a",
        ChunkStatus::InProgress,
        "2026-06-08T01:00:00Z",
    ));

    let a = engine
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "a")
        .unwrap();
    assert_eq!(
        a.cross_refs,
        vec!["task:verify".to_string()],
        "a stale cloud copy clobbered a newer local mutation"
    );
    assert_eq!(a.updated_at, "2026-06-08T02:00:00Z");
}

/// Equal stamps (the post-push echo of our own mutation) keep the local copy —
/// identical content, no churn. Same tie rule as the disk merge.
#[test]
fn upsert_chunk_keeps_local_on_equal_stamps() {
    let mut engine = RoadmapEngine::new("t".into());
    let mut local = chunk_at("a", ChunkStatus::Pending, "2026-06-08T01:00:00Z");
    local.notes = "local copy".into();
    engine.upsert_chunk(local);

    engine.upsert_chunk(chunk_at("a", ChunkStatus::Pending, "2026-06-08T01:00:00Z"));
    assert_eq!(engine.roadmap().chunks[0].notes, "local copy");
}

#[tokio::test]
async fn reconcile_roadmap_pulls_cloud_chunks_into_the_local_engine() {
    let server = MockServer::start().await;
    let records: Vec<Value> = ["a", "b"]
        .iter()
        .map(|id| {
            serde_json::to_value(from_chunk(
                "t",
                &owned_chunk(id, ChunkStatus::Backlog, "t"),
                &[],
                &[],
            ))
            .unwrap()
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "roadmap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": records })))
        .mount(&server)
        .await;

    let client = CloudClient::new(server.uri(), "tok");
    let mut engine = RoadmapEngine::new("t".into());

    let merged = reconcile_roadmap(&client, &mut engine).await.unwrap();
    assert_eq!(merged, 2);
    assert_eq!(engine.roadmap().chunks.len(), 2);

    // Re-pull is idempotent (cloud-wins replace, not duplicate).
    assert_eq!(reconcile_roadmap(&client, &mut engine).await.unwrap(), 2);
    assert_eq!(engine.roadmap().chunks.len(), 2);
}

/// sync-project-scope: with an org token every project in the org shares one
/// cloud tenant, so the reconcile must filter by the record's project stamp —
/// another project's chunks (or unstamped legacy records of unknowable
/// origin) must never land in this project's roadmap. This is the bleed
/// observed live between two projects sharing one tenant, as a test.
#[tokio::test]
async fn reconcile_skips_other_projects_records_in_the_same_tenant() {
    let server = MockServer::start().await;
    let mut unstamped = serde_json::to_value(from_chunk(
        "t",
        &chunk("legacy", ChunkStatus::Backlog),
        &[],
        &[],
    ))
    .unwrap();
    unstamped["record"]
        .as_object_mut()
        .unwrap()
        .remove("project_id");
    let records: Vec<Value> = vec![
        serde_json::to_value(from_chunk(
            "t",
            &owned_chunk("ours", ChunkStatus::Backlog, "t"),
            &[],
            &[],
        ))
        .unwrap(),
        serde_json::to_value(from_chunk(
            "other-project",
            &chunk("theirs", ChunkStatus::Backlog),
            &[],
            &[],
        ))
        .unwrap(),
        unstamped,
    ];
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "roadmap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": records })))
        .mount(&server)
        .await;

    let client = CloudClient::new(server.uri(), "tok");
    let mut engine = RoadmapEngine::new("t".into());

    let merged = reconcile_roadmap(&client, &mut engine).await.unwrap();
    assert_eq!(merged, 1, "only this project's stamped chunk merges");
    let ids: Vec<&str> = engine
        .roadmap()
        .chunks
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(ids, vec!["ours"]);
}

/// THE LAUNDERING ROUND TRIP — the 2026-06 bleed's second act, closed.
///
/// A chunk of UNKNOWN origin (`project_id: None`) must not become provably the
/// pusher's just by being pushed. This is not a hypothetical: 22 chunks that had
/// bled into another project were obsoleted there by a cleanup, and because sync
/// is write-through on mutation, that push stamped every one of them with the
/// cleaning project's id. They came back as provably its own — self-consistent,
/// and invisible to `prune`, which only removes what provably belongs elsewhere.
/// The cleanup is what made the bleed permanent.
///
/// So the property under test is a ROUND TRIP, not a field: build the envelope
/// the way a push does, feed it to the merge the way a pull does, and require
/// that it does NOT come home owned.
#[test]
fn a_push_cannot_launder_an_unprovable_chunk_into_ownership() {
    let orphan = chunk("bled-in-from-elsewhere", ChunkStatus::Obsoleted);
    assert!(
        orphan.project_id.is_none(),
        "precondition: origin genuinely unknown"
    );

    let envelope = serde_json::to_value(from_chunk("project-c59cfc", &orphan, &[], &[])).unwrap();
    assert_eq!(
        envelope["record"].get("project_id"),
        Some(&Value::Null),
        "an unprovable origin must cross the wire as explicitly unprovable, not \
         be filled in with whoever happened to push it"
    );

    // The half that actually protects anyone: it must not merge back as ours.
    let mut engine = RoadmapEngine::new("project-c59cfc".into());
    let merged = think_and_ship::cloud::pull::apply_roadmap_records(&mut engine, &[envelope]);
    assert_eq!(
        merged, 0,
        "the pusher got its own unprovable record back as provably its own — this \
         is the laundering that turned a cleanup into permanent ownership"
    );
    assert!(
        engine.roadmap().chunks.is_empty(),
        "nothing of unknown origin may land in a project's store by round trip"
    );
}

/// The other half of the contract: a chunk that DOES know where it came from is
/// stamped, merges, and is not relabelled by whoever pushes it.
#[test]
fn a_provable_chunk_still_round_trips_and_keeps_its_own_origin() {
    let ours = owned_chunk("genuinely-ours", ChunkStatus::Backlog, "project-c59cfc");
    let envelope = serde_json::to_value(from_chunk("project-c59cfc", &ours, &[], &[])).unwrap();
    assert_eq!(
        envelope["record"]["project_id"].as_str(),
        Some("project-c59cfc")
    );

    let mut engine = RoadmapEngine::new("project-c59cfc".into());
    let merged = think_and_ship::cloud::pull::apply_roadmap_records(&mut engine, &[envelope]);
    assert_eq!(merged, 1, "a provable record must still sync");

    // And a foreign one, pushed by its own project, stays foreign to us.
    let theirs = owned_chunk("theirs", ChunkStatus::Backlog, "think-and-ship-676f38");
    let foreign =
        serde_json::to_value(from_chunk("think-and-ship-676f38", &theirs, &[], &[])).unwrap();
    let merged = think_and_ship::cloud::pull::apply_roadmap_records(&mut engine, &[foreign]);
    assert_eq!(merged, 0, "another project's stamped record never merges");
}
