//! Does the tracker seam hold without a provider?
//!
//! The falsification test for the tracker port itself. The claim under
//! test is that a roadmap chunk can be projected onto a tracker, bound to its
//! twin, and reconciled back **using only the public seam** — no provider SDK,
//! no network, and no core type that knows what GitHub or Jira are.
//!
//! Note what is deliberately absent: there is no production chunk-to-WorkItem
//! projector. Mapping lives in this file, in the test, because building the real
//! projector (capability degradation, the provenance footer, outbox delivery) is
//! its own concern. If the seam were insufficient, that mapping could not be
//! written from outside the crate — which is exactly the property being checked.

use think_and_ship::infra::{CrossRef, Domain, Persistence, PersistenceConfig};
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::{
    FakeTracker, TrackerCapabilities, TrackerPort, WorkItem, WorkItemState,
};

use tempfile::TempDir;

const PROVIDER: &str = "fake";

fn engine() -> RoadmapEngine {
    RoadmapEngine::new("proj".into())
}

fn add(engine: &mut RoadmapEngine, id: &str) {
    engine
        .add_chunk(
            id.into(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            10,
            format!("description of {id}"),
            vec!["it works".into()],
            vec![],
            false,
        )
        .unwrap();
}

/// The whole chunk-to-canonical mapping, written from outside the crate against
/// the public seam alone. Intentionally minimal — the real one is the
/// production projector.
fn to_work_item(engine: &RoadmapEngine, chunk_id: &str) -> WorkItem {
    let chunk = engine
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == chunk_id)
        .expect("chunk exists");

    let mut body = chunk.description.clone();
    if !chunk.acceptance.is_empty() {
        body.push_str("\n\n## Acceptance\n");
        for a in &chunk.acceptance {
            body.push_str(&format!("- [ ] {a}\n"));
        }
    }

    WorkItem {
        group: None,
        // An existing binding makes this a patch; its absence makes it a create.
        external_id: engine
            .tracker_link(chunk_id, PROVIDER)
            .map(|l| l.external_id.clone()),
        title: chunk.title.clone(),
        body,
        state: match chunk.status {
            ChunkStatus::InProgress => WorkItemState::InProgress,
            ChunkStatus::Done => WorkItemState::Done,
            ChunkStatus::Obsoleted => WorkItemState::Cancelled,
            _ => WorkItemState::Todo,
        },
        labels: vec![],
        assignee: None,
        version: engine
            .tracker_link(chunk_id, PROVIDER)
            .and_then(|l| l.last_seen_version.clone()),
    }
}

/// Project once, binding the result. The three-line shape a real projector will
/// keep: build, write, record.
async fn project(engine: &mut RoadmapEngine, tracker: &dyn TrackerPort, chunk_id: &str) {
    let item = to_work_item(engine, chunk_id);
    let outcome = tracker.upsert_item(&item).await.expect("upsert succeeds");
    engine
        .record_tracker_link(
            chunk_id,
            tracker.provider(),
            &outcome.external_id,
            &item.content_hash(),
            outcome.version.as_deref(),
        )
        .expect("link recorded");
}

#[tokio::test]
async fn a_chunk_round_trips_through_the_seam_with_no_provider_code() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);

    project(&mut e, &tracker, "c1").await;

    // The tracker holds a faithful projection of the chunk…
    let remote = &tracker.items()[0];
    assert_eq!(remote.title, "Chunk c1");
    assert!(
        remote.body.contains("- [ ] it works"),
        "acceptance travelled"
    );
    assert_eq!(remote.state, WorkItemState::Todo);

    // …and the chunk knows its twin, in the SAME provenance graph as think:/task:.
    let link = e.tracker_link("c1", PROVIDER).expect("bound");
    assert_eq!(link.external_id, remote.external_id.clone().unwrap());
    assert_eq!(
        link.our_last_write_hash,
        to_work_item(&e, "c1").content_hash()
    );

    let expected_ref = CrossRef::external(PROVIDER, &link.external_id).to_wire();
    assert!(
        e.roadmap().chunks[0].cross_refs.contains(&expected_ref),
        "the ext: cross-ref is on the chunk: {:?}",
        e.roadmap().chunks[0].cross_refs
    );
}

/// Re-projecting an UNCHANGED chunk must patch its existing twin, never mint a
/// second one. This is the duplicate-ticket failure that makes users distrust an
/// integration permanently, and the reason identity lives in the link record.
#[tokio::test]
async fn reprojection_patches_the_same_twin() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);

    project(&mut e, &tracker, "c1").await;
    let first = e.tracker_link("c1", PROVIDER).unwrap().external_id.clone();

    project(&mut e, &tracker, "c1").await;
    project(&mut e, &tracker, "c1").await;

    assert_eq!(tracker.items().len(), 1, "one twin, not three");
    assert_eq!(
        e.tracker_link("c1", PROVIDER).unwrap().external_id,
        first,
        "identity is stable across re-projection"
    );
    assert_eq!(
        e.roadmap().chunks[0]
            .cross_refs
            .iter()
            .filter(|r| r.starts_with("ext:"))
            .count(),
        1,
        "the cross-ref does not accumulate"
    );
}

/// A rename on OUR side must follow the identity, not create a new item — the
/// bug a title-matching projector would ship.
#[tokio::test]
async fn a_retitled_chunk_follows_its_twin() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project(&mut e, &tracker, "c1").await;

    e.update_chunk("c1", Some("Renamed".into()), None, None, None, None, None)
        .unwrap();
    project(&mut e, &tracker, "c1").await;

    assert_eq!(tracker.items().len(), 1);
    assert_eq!(tracker.items()[0].title, "Renamed");
}

/// The hash recorded on the link is what the echo fence compares against, so
/// it must actually MOVE when the projected content moves, and stand still when
/// it doesn't.
#[tokio::test]
async fn the_recorded_hash_tracks_authored_content() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);

    project(&mut e, &tracker, "c1").await;
    let hash_before = e
        .tracker_link("c1", PROVIDER)
        .unwrap()
        .our_last_write_hash
        .clone();

    // A no-op re-projection leaves the content hash where it was…
    project(&mut e, &tracker, "c1").await;
    assert_eq!(
        e.tracker_link("c1", PROVIDER).unwrap().our_last_write_hash,
        hash_before
    );

    // …while a real edit moves it.
    e.update_chunk(
        "c1",
        None,
        None,
        Some("new description".into()),
        None,
        None,
        None,
    )
    .unwrap();
    project(&mut e, &tracker, "c1").await;
    assert_ne!(
        e.tracker_link("c1", PROVIDER).unwrap().our_last_write_hash,
        hash_before
    );
}

/// Reading back is the other half of "two-way". A remote edit must be visible
/// through the port, and distinguishable from our own last write by comparing
/// against the stored hash — the foundation the echo fence builds on.
#[tokio::test]
async fn a_remote_edit_is_distinguishable_from_our_own_write() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock("2026-07-01T00:00:00+00:00");
    project(&mut e, &tracker, "c1").await;

    let link = e.tracker_link("c1", PROVIDER).unwrap().clone();

    // Nothing changed upstream: what we fetch still matches what we wrote.
    let fetched = tracker
        .fetch_since("2026-01-01T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(
        fetched[0].content_hash(),
        link.our_last_write_hash,
        "our own write hashes back to what we recorded — an echo"
    );

    // A human edits in the tracker's UI.
    tracker.set_clock("2026-07-02T00:00:00+00:00");
    tracker.remote_edit(&link.external_id, |i| {
        i.title = "Edited by a human".into();
    });

    let fetched = tracker
        .fetch_since("2026-07-02T00:00:00+00:00")
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_ne!(
        fetched[0].content_hash(),
        link.our_last_write_hash,
        "a genuine remote change must NOT look like our echo"
    );
}

/// One chunk may have a twin per provider, and they must not collide.
#[tokio::test]
async fn twins_on_different_providers_coexist() {
    let mut e = engine();
    add(&mut e, "c1");
    let a = FakeTracker::new("alpha");
    let b = FakeTracker::new("beta");

    let item = WorkItem::new("Chunk c1");
    for t in [&a, &b] {
        let outcome = t.upsert_item(&item).await.unwrap();
        e.record_tracker_link(
            "c1",
            t.provider(),
            &outcome.external_id,
            &item.content_hash(),
            outcome.version.as_deref(),
        )
        .unwrap();
    }

    assert_eq!(e.tracker_links_for("c1").len(), 2);
    assert_eq!(e.tracker_link("c1", "alpha").unwrap().provider, "alpha");
    assert_eq!(e.tracker_link("c1", "beta").unwrap().provider, "beta");
    assert_eq!(
        e.roadmap().chunks[0]
            .cross_refs
            .iter()
            .filter(|r| r.starts_with("ext:"))
            .count(),
        2
    );
}

/// The link must survive a restart on the SAME store the chunks use — the
/// acceptance criterion that forbids a second persistence implementation.
#[test]
fn links_persist_across_engine_instances() {
    let tmp = TempDir::new().unwrap();
    let cfg = PersistenceConfig::from_env()
        .with_data_dir(tmp.path().to_path_buf())
        .enabled(true);

    {
        let mut e = RoadmapEngine::new("proj".into())
            .with_persistence(Persistence::new(&cfg, Domain::Roadmap));
        add(&mut e, "c1");
        e.record_tracker_link("c1", PROVIDER, "42", "deadbeef", Some("7"))
            .unwrap();
    }

    let e2 =
        RoadmapEngine::new("proj".into()).with_persistence(Persistence::new(&cfg, Domain::Roadmap));
    let link = e2.tracker_link("c1", PROVIDER).expect("link survived");
    assert_eq!(link.external_id, "42");
    assert_eq!(link.our_last_write_hash, "deadbeef");
    assert_eq!(link.last_seen_version.as_deref(), Some("7"));
    assert!(
        e2.roadmap().chunks[0]
            .cross_refs
            .contains(&"ext:fake/42".to_string()),
        "the cross-ref survived too"
    );
}

/// Binding a chunk that doesn't exist must fail loudly rather than orphan a
/// link record no chunk owns.
#[test]
fn binding_an_unknown_chunk_is_rejected() {
    let mut e = engine();
    assert!(
        e.record_tracker_link("ghost", PROVIDER, "1", "h", None)
            .is_err()
    );
    assert!(e.roadmap().links.is_empty());
}

/// A provider that can't express something refuses BEFORE the call, with a
/// message naming what to fix — the Jira screen-scheme path, exercised without
/// Jira.
#[tokio::test]
async fn capability_refusal_precedes_any_write() {
    let mut caps = TrackerCapabilities::full();
    caps.required_fields = vec!["customfield_10010".into()];
    let tracker = FakeTracker::new(PROVIDER).with_capabilities(caps);

    let err = tracker.upsert_item(&WorkItem::new("t")).await.unwrap_err();
    assert!(
        format!("{err}").contains("customfield_10010"),
        "the message names the field: {err}"
    );
    assert!(!err.retryable(), "a config problem must not be retried");
    assert_eq!(tracker.writes(), 0);
}

/// The OCP claim, made mechanical: a brand-new provider is a type defined
/// entirely outside this crate. It compiles against the port alone and drops
/// straight into the same `Box<dyn TrackerPort>` slot — no core file edited, no
/// enum extended, no registration.
#[tokio::test]
async fn a_fourth_provider_needs_nothing_from_the_core() {
    use async_trait::async_trait;
    use think_and_ship::tracker::port::{TrackerError, UpsertOutcome};

    struct Homegrown;

    #[async_trait]
    impl TrackerPort for Homegrown {
        fn provider(&self) -> &str {
            "homegrown"
        }
        fn capabilities(&self) -> TrackerCapabilities {
            TrackerCapabilities::full()
        }
        async fn upsert_item(&self, _item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
            Ok(UpsertOutcome {
                external_id: "HG-1".into(),
                version: Some("1".into()),
                created: true,
            })
        }
        async fn fetch_since(&self, _since: &str) -> Result<Vec<WorkItem>, TrackerError> {
            Ok(vec![])
        }
    }

    let mut e = engine();
    add(&mut e, "c1");

    let providers: Vec<Box<dyn TrackerPort>> =
        vec![Box::new(FakeTracker::new(PROVIDER)), Box::new(Homegrown)];

    for p in &providers {
        project(&mut e, p.as_ref(), "c1").await;
    }

    assert_eq!(e.tracker_links_for("c1").len(), 2);
    assert_eq!(
        e.tracker_link("c1", "homegrown").unwrap().external_id,
        "HG-1"
    );
}
