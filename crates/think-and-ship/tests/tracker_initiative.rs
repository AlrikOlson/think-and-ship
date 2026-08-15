//! The initiative: the roof above the containers.
//!
//! Driven through `project_all_with_policy` against `FakeTracker`, so these
//! assert the PROJECTOR's behaviour — phase −1 ordering, roadmap-wide state,
//! degradation — rather than one adapter's. The Linear half (create-once,
//! join-mutation idempotence, the third vocabulary) is in `tracker_linear.rs`
//! against the GraphQL mock.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::domain::GroupState;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::ownership::Ownership;
use think_and_ship::tracker::port::TrackerError;
use think_and_ship::tracker::project::{project_all, project_all_with_policy};

const PROVIDER: &str = "fake";

fn engine_with(chunks: &[(&str, Option<&str>, ChunkStatus)]) -> RoadmapEngine {
    let mut e = RoadmapEngine::new("proj".into());
    for (id, group, status) in chunks {
        e.add_chunk(
            (*id).into(),
            format!("Chunk {id}"),
            *status,
            10,
            format!("why {id}"),
            vec![],
            vec![],
            false,
        )
        .expect("add");
        e.set_tracker_opt_in(id, PROVIDER, true).expect("opt in");
        if let Some(g) = group {
            e.set_group(id, Some((*g).into())).expect("group");
        }
    }
    e
}

async fn push(
    e: &mut RoadmapEngine,
    t: &FakeTracker,
    initiative: Option<&str>,
) -> think_and_ship::tracker::ProjectionReport {
    project_all_with_policy(e, t, None, &Ownership::default(), initiative)
        .await
        .expect("project")
}

/// THE promise: the caller names a roof and every push ensures it, with the
/// state derived from the WHOLE roadmap.
#[tokio::test]
async fn the_roof_is_ensured_with_the_roadmaps_derived_state() {
    let mut e = engine_with(&[
        ("tracker-a", Some("Harbour master"), ChunkStatus::Pending),
        ("saas-a", Some("Container yard"), ChunkStatus::InProgress),
    ]);
    let t = FakeTracker::new(PROVIDER);

    let report = push(&mut e, &t, Some("think-and-ship")).await;

    assert_eq!(
        t.initiative(),
        Some(("think-and-ship".to_string(), GroupState::Active)),
        "one chunk moving means the roadmap is moving"
    );
    assert_eq!(
        report.initiative_ensured.as_deref(),
        Some("think-and-ship"),
        "the report says the roof stands"
    );
    assert!(report.initiative_failure.is_none());
}

/// The regression guard: a caller with no name must change nothing — this is
/// every existing test and every provider without config.
#[tokio::test]
async fn no_roof_is_attempted_without_a_name() {
    let mut e = engine_with(&[("loner", Some("Gate house"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    let report = project_all(&mut e, &t, None).await.expect("project");

    assert_eq!(t.initiative(), None, "no name, no roof");
    assert_eq!(t.initiative_writes(), 0);
    assert!(report.initiative_ensured.is_none());
}

/// Called on every push, so an unchanged roof must cost nothing — the
/// `group_writes` contract one level up.
#[tokio::test]
async fn a_second_push_rewrites_no_roof() {
    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    push(&mut e, &t, Some("roof")).await;
    let after_first = t.initiative_writes();
    push(&mut e, &t, Some("roof")).await;

    assert_eq!(
        t.initiative_writes(),
        after_first,
        "an unchanged roof must be a no-op on re-push"
    );
}

/// The roof's state is ROADMAP-wide, not an aggregate of the groups: an
/// ungrouped chunk in progress moves the roadmap even though it moves no
/// container. This is the case that separates `derived_state(all planned)`
/// from "derive from the groups" — under the latter, the roof here would sit
/// at NotStarted.
#[tokio::test]
async fn an_ungrouped_chunk_still_counts_toward_the_roofs_state() {
    let mut e = engine_with(&[
        ("grouped", Some("Gate house"), ChunkStatus::Pending),
        ("loner", None, ChunkStatus::InProgress),
    ]);
    let t = FakeTracker::new(PROVIDER);

    push(&mut e, &t, Some("roof")).await;

    assert_eq!(
        t.groups(),
        vec![("Gate house".to_string(), GroupState::NotStarted)],
        "precondition: the only container has not started"
    );
    assert_eq!(
        t.initiative(),
        Some(("roof".to_string(), GroupState::Active)),
        "the roadmap is moving even though no container is"
    );
}

/// The degradation criterion, stated exactly: a failure creating the
/// roof must cost neither the containers nor the issues.
#[tokio::test]
async fn a_roof_failure_does_not_cost_the_projects_or_the_issues() {
    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);
    // Phase −1 runs first, so the next-call failure lands exactly on the roof.
    t.fail_next(TrackerError::Status {
        status: 500,
        body: "initiative service is down".into(),
    });

    let report = push(&mut e, &t, Some("roof")).await;

    assert!(
        report
            .initiative_failure
            .as_deref()
            .is_some_and(|why| why.contains("initiative service is down")),
        "the report says WHY the roof is missing: {report:?}"
    );
    assert!(report.initiative_ensured.is_none());
    assert_eq!(
        report.groups_ensured,
        vec!["Quayside".to_string()],
        "the container still landed"
    );
    assert_eq!(t.items().len(), 1, "and so did the issue");
}

/// A provider without the concept degrades SILENTLY — the refusing default is
/// not a failure, exactly as `upsert_group`'s flat-filing treatment.
#[tokio::test]
async fn a_provider_without_the_concept_is_not_reported_as_failing() {
    use async_trait::async_trait;
    use think_and_ship::tracker::TrackerCapabilities;
    use think_and_ship::tracker::domain::{WorkGroup, WorkItem};
    use think_and_ship::tracker::port::{TrackerPort, UpsertOutcome};

    // Delegates everything it understands to the fake and deliberately does
    // NOT implement `upsert_initiative` — the port's refusing default IS the
    // behaviour under test.
    struct RoofBlind(FakeTracker);

    #[async_trait]
    impl TrackerPort for RoofBlind {
        fn provider(&self) -> &str {
            self.0.provider()
        }
        fn capabilities(&self) -> TrackerCapabilities {
            self.0.capabilities()
        }
        async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
            self.0.upsert_item(item).await
        }
        async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError> {
            self.0.fetch_since(since).await
        }
        async fn upsert_group(&self, group: &WorkGroup) -> Result<UpsertOutcome, TrackerError> {
            self.0.upsert_group(group).await
        }
    }

    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = RoofBlind(FakeTracker::new(PROVIDER));

    let report = project_all_with_policy(&mut e, &t, None, &Ownership::default(), Some("roof"))
        .await
        .expect("project");

    assert!(
        report.initiative_failure.is_none(),
        "Unsupported is a provider without the concept, not a failure"
    );
    assert!(report.initiative_ensured.is_none());
    assert_eq!(t.0.items().len(), 1, "the issue landed regardless");
}
