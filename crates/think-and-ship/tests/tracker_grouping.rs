//! Grouping: chunks group into containers.
//!
//! Driven through `project_all` against `FakeTracker`, so these assert the
//! PROJECTOR's behaviour rather than one adapter's. The Linear half is the same
//! contract with a network under it.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::domain::GroupState;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::project::project_all;

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

/// THE promise: one container per group, and every grouped issue filed in its own.
#[tokio::test]
async fn each_group_becomes_one_container_and_its_items_carry_it() {
    let mut e = engine_with(&[
        ("tracker-a", Some("Harbour master"), ChunkStatus::Pending),
        ("tracker-b", Some("Harbour master"), ChunkStatus::Pending),
        ("saas-a", Some("Container yard"), ChunkStatus::Pending),
    ]);
    let t = FakeTracker::new(PROVIDER);

    let report = project_all(&mut e, &t, None).await.expect("project");

    assert_eq!(
        report.groups_ensured,
        vec!["Container yard".to_string(), "Harbour master".to_string()],
        "one container per DISTINCT group, not one per chunk"
    );
    assert_eq!(t.group_writes(), 2, "two containers, two writes");

    for item in t.items() {
        let expected = if item.title.contains("tracker") {
            "Harbour master"
        } else {
            "Container yard"
        };
        assert_eq!(
            item.group.as_deref(),
            Some(expected),
            "every item must carry its container: {item:?}"
        );
    }
}

/// The regression that matters most: adding grouping must not change what
/// happens to a chunk that has none.
#[tokio::test]
async fn an_ungrouped_chunk_projects_exactly_as_before() {
    let mut e = engine_with(&[("loner", None, ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    let report = project_all(&mut e, &t, None).await.expect("project");

    assert!(
        report.groups_ensured.is_empty(),
        "no group means NO container is invented for it"
    );
    assert_eq!(t.group_writes(), 0);
    assert_eq!(t.items().len(), 1, "the issue is still created");
    assert_eq!(
        t.items()[0].group,
        None,
        "and it is filed flat, exactly as every item was before containers existed"
    );
}

/// Called on every push for every group, so an unchanged container must cost
/// nothing — otherwise the cadence churns the workspace forever.
#[tokio::test]
async fn a_second_push_rewrites_no_container() {
    let mut e = engine_with(&[("tracker-a", Some("Harbour master"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    project_all(&mut e, &t, None).await.expect("first");
    let after_first = t.group_writes();
    project_all(&mut e, &t, None).await.expect("second");

    assert_eq!(
        t.group_writes(),
        after_first,
        "an unchanged container must be a no-op on re-push, not a re-write"
    );
    assert_eq!(t.groups().len(), 1, "and must not be duplicated");
}

/// State is DERIVED, and the derivation is the only opinion we assert upstream.
#[tokio::test]
async fn container_state_is_derived_from_its_chunks() {
    // All pending -> nothing has started.
    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);
    project_all(&mut e, &t, None).await.expect("project");
    assert_eq!(
        t.groups(),
        vec![("Quayside".to_string(), GroupState::NotStarted)]
    );

    // One in progress -> the group is moving.
    let mut e = engine_with(&[
        ("b-1", Some("Paperwork"), ChunkStatus::Pending),
        ("b-2", Some("Paperwork"), ChunkStatus::InProgress),
    ]);
    let t = FakeTracker::new(PROVIDER);
    project_all(&mut e, &t, None).await.expect("project");
    assert_eq!(
        t.groups(),
        vec![("Paperwork".to_string(), GroupState::Active)]
    );

    // Every chunk done -> finished.
    let mut e = engine_with(&[("c-1", Some("Cold store"), ChunkStatus::Pending)]);
    e.set_status("c-1", ChunkStatus::InProgress).expect("start");
    e.set_status("c-1", ChunkStatus::Done).expect("finish");
    let t = FakeTracker::new(PROVIDER);
    project_all(&mut e, &t, None).await.expect("project");
    assert_eq!(
        t.groups(),
        vec![("Cold store".to_string(), GroupState::Complete)]
    );
}

/// The obsoleted filter, tested where it actually DISCRIMINATES.
///
/// The first version of this asserted an all-obsoleted group is NotStarted —
/// which is true whether or not obsoleted chunks are filtered, so deliberately
/// removing the filter still passed it. The case that separates them is a MIXED
/// group: with the filter the abandoned chunk is ignored and the rest being
/// done makes the group done; without it, the group is stuck Active forever
/// because one chunk will never reach Done.
#[tokio::test]
async fn an_obsoleted_chunk_does_not_hold_its_group_open_forever() {
    let mut e = engine_with(&[
        ("d-1", Some("Dry dock"), ChunkStatus::Pending),
        ("d-2", Some("Dry dock"), ChunkStatus::Backlog),
    ]);
    e.set_status("d-1", ChunkStatus::InProgress).expect("start");
    e.set_status("d-1", ChunkStatus::Done).expect("finish");
    e.set_status("d-2", ChunkStatus::Obsoleted)
        .expect("abandon");
    let t = FakeTracker::new(PROVIDER);

    project_all(&mut e, &t, None).await.expect("project");

    assert_eq!(
        t.groups(),
        vec![("Dry dock".to_string(), GroupState::Complete)],
        "the abandoned chunk must be IGNORED, not treated as unfinished work \
         that keeps the group open forever"
    );
}

/// The bug the LIVE workspace found that the fake never could: every real
/// roadmap grows its groups AFTER its issues exist. The first push records a
/// content hash; seeding a group changes nothing that hash covered, so the
/// cheap skip ate the projectId attach and every container stayed empty.
/// A group-only change must dirty the item.
#[tokio::test]
async fn grouping_an_already_pushed_chunk_reaches_the_tracker() {
    let mut e = engine_with(&[("late-1", None, ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    project_all(&mut e, &t, None).await.expect("first push");
    assert_eq!(t.items()[0].group, None, "precondition: filed flat");

    e.set_group("late-1", Some("Night shift".into()))
        .expect("group");
    project_all(&mut e, &t, None).await.expect("second push");

    assert_eq!(
        t.items()[0].group.as_deref(),
        Some("Night shift"),
        "a group set after the first push must reach the tracker, \
         not be skipped as an unchanged item"
    );
}

/// The other half: abandoning EVERYTHING is not the same as finishing it.
#[tokio::test]
async fn an_all_obsoleted_group_is_not_reported_complete() {
    let mut e = engine_with(&[("g-1", Some("Gate house"), ChunkStatus::Backlog)]);
    e.set_status("g-1", ChunkStatus::Obsoleted)
        .expect("obsolete");
    let t = FakeTracker::new(PROVIDER);

    project_all(&mut e, &t, None).await.expect("project");

    assert_eq!(
        t.groups(),
        vec![("Gate house".to_string(), GroupState::NotStarted)],
        "a workstream whose every chunk was abandoned has not been completed"
    );
}
