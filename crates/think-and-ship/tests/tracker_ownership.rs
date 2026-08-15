//! Ownership: the projector consults the ownership table, and cannot
//! be written not to.
//!
//! The unit tests in `tracker::ownership` prove the table's truth. These prove
//! the projector actually routes through it — a policy nothing consults is a
//! document, not a policy.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::domain::WorkItemState;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::ownership::{Field, Owner, Ownership};
use think_and_ship::tracker::project::{project_all, project_all_with_policy};

const PROVIDER: &str = "fake";

fn engine() -> RoadmapEngine {
    RoadmapEngine::new("proj".into())
}

fn add(e: &mut RoadmapEngine, id: &str) {
    e.add_chunk(
        id.into(),
        format!("Chunk {id}"),
        ChunkStatus::Pending,
        10,
        format!("why {id} exists"),
        vec![format!("{id} works")],
        vec![],
        false,
    )
    .expect("add chunk");
    e.set_tracker_opt_in(id, PROVIDER, true).expect("opt in");
}

/// THE promise. A person moves the ticket to a column and assigns it; the next
/// projection must not undo that. This is the failure the whole chunk exists to
/// prevent, and the one every naive last-write-wins sync commits.
#[tokio::test]
async fn a_projection_does_not_undo_the_teams_workflow() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("create");

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    // A human does their job in the tracker.
    tracker.remote_edit(&link.external_id, |item| {
        item.state = WorkItemState::InProgress;
        item.assignee = Some("Ada".into());
        item.labels.push("Bug".into());
    });

    // Now the plan changes, so a projection is due.
    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    project_all(&mut e, &tracker, None).await.expect("patch");

    let after = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        after.state,
        WorkItemState::InProgress,
        "their column was reset by our projection"
    );
    assert_eq!(
        after.assignee.as_deref(),
        Some("Ada"),
        "their assignee was cleared by our projection"
    );
    assert!(
        after.labels.contains(&"Bug".to_string()),
        "their label was dropped by our projection: {:?}",
        after.labels
    );
}

/// The body is ours, so the plan wins — but the discarded edit is REPORTED.
/// Silently winning is the same crime as silently losing.
#[tokio::test]
async fn an_overwritten_edit_to_a_field_we_own_is_reported() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("create");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    tracker.remote_edit(&link.external_id, |item| {
        item.body = "a human rewrote the description".into();
    });

    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    let report = project_all(&mut e, &tracker, None).await.expect("patch");

    let d = report
        .divergences
        .iter()
        .find(|(chunk, d)| chunk == "c1" && d.field == Field::Body)
        .map(|(_, d)| d)
        .expect("the overwritten edit must be reported, not silently dropped");
    assert_eq!(d.owner, Owner::Ours);
    assert!(d.summary().contains("written back over it"));
}

/// Contested does not mean we win. A remote retitle STANDS, and a human is told,
/// because the retitle carries information the plan does not have.
#[tokio::test]
async fn a_remote_retitle_survives_and_raises_a_concern() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("create");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    tracker.remote_edit(&link.external_id, |item| {
        item.title = "PM renamed this to something clearer".into();
    });

    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    let report = project_all(&mut e, &tracker, None).await.expect("patch");

    let after = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        after.title, "PM renamed this to something clearer",
        "the retitle was clobbered — that is the exact work-destroying behaviour"
    );
    assert!(
        report
            .divergences
            .iter()
            .any(|(c, d)| c == "c1" && d.field == Field::Title && d.owner == Owner::Contested),
        "a contested difference must reach a human"
    );
}

/// The table is overridable per project. A team that wants the plan to own the
/// title can say so, and then it does.
#[tokio::test]
async fn a_project_can_override_the_table() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    let policy = Ownership::default().with(Field::Title, Owner::Ours);

    project_all_with_policy(&mut e, &tracker, None, &policy, None)
        .await
        .expect("create");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    tracker.remote_edit(&link.external_id, |item| {
        item.title = "a retitle this project has chosen not to honour".into();
    });
    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    project_all_with_policy(&mut e, &tracker, None, &policy, None)
        .await
        .expect("patch");

    assert_eq!(
        tracker.item(&link.external_id).expect("item").title,
        "Chunk c1",
        "the project took the title back and the projection should honour that"
    );
}

/// Agreement is not conflict. A projection where nobody edited anything upstream
/// must report no divergence, or the signal channel fills with noise and stops
/// being read — which is how a real concern gets missed.
#[tokio::test]
async fn an_undisturbed_item_raises_nothing() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("create");

    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    let report = project_all(&mut e, &tracker, None).await.expect("patch");

    assert!(
        report.divergences.is_empty(),
        "nobody touched the tracker; got {:?}",
        report.divergences
    );
}

/// The hash recorded must be the hash of what we ACTUALLY sent, which after
/// reconciliation can differ from what we planned. Recording the plan's hash
/// would make the echo fence compare against bytes that never reached the
/// provider, and every subsequent inbound event would be misjudged.
#[tokio::test]
async fn the_recorded_hash_matches_what_was_actually_written() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("create");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    // A contested retitle means the item we send is NOT the item we planned.
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "their title".into();
    });
    e.update_chunk(
        "c1",
        None,
        None,
        Some("a revised description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    project_all(&mut e, &tracker, None).await.expect("patch");

    let after = tracker.item(&link.external_id).expect("item");
    let link = e.tracker_link("c1", PROVIDER).expect("link");
    assert_eq!(
        link.our_last_write_hash,
        after.content_hash(),
        "the fence records what we sent, not what we intended to send"
    );
}
