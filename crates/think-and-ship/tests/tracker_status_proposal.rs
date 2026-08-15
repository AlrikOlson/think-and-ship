//! Status proposals: the machine proposes, the human disposes — and two
//! writers converge.
//!
//! # What "concurrent" means here, stated before it is tested
//!
//! There is no shared clock between the plan and the tracker, so "at the same
//! instant" is not a thing this system can observe or a test can stage. What it
//! CAN observe is the only definition that matters operationally: **both sides
//! changed since the last synchronization point.** That is exactly what the
//! version fence detects — our recorded `last_seen_version` versus the one the
//! provider reports now.
//!
//! So the concurrency test below does not sequence two edits and call it
//! concurrency. It puts the system into the state where our link's recorded
//! version is stale AND our local plan has moved, then projects once, and
//! asserts the outcome is defined, recorded, and stable on a second run.

use think_and_ship::roadmap::domain::{ChunkStatus, StatusProposal};
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::concern::propose_status_from_sweep;
use think_and_ship::tracker::domain::WorkItemState;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::ownership::Field;
use think_and_ship::tracker::project::project_all;
use think_and_ship::tracker::sweep::reconcile;

const PROVIDER: &str = "fake";
const T0: &str = "2026-07-26T10:00:00+00:00";
const T1: &str = "2026-07-26T11:00:00+00:00";
const T2: &str = "2026-07-26T12:00:00+00:00";

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

fn proposal(e: &RoadmapEngine, id: &str) -> Option<StatusProposal> {
    e.roadmap()
        .chunks
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| c.status_proposal.clone())
}

/// THE promise. Somebody closes the ticket. The chunk does NOT go done — a
/// close means the ticket is finished, not that the acceptance criteria were
/// met, and transitioning silently removes the one moment a human was going to
/// look at the evidence.
#[tokio::test]
async fn a_closed_ticket_proposes_done_it_does_not_transition() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();
    tracker.set_clock(T1);
    tracker.remote_edit(&link.external_id, |item| {
        item.state = WorkItemState::Done;
    });

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");
    let touched = propose_status_from_sweep(&mut e, PROVIDER, &report);

    assert_eq!(touched, vec!["c1".to_string()]);

    let status = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .map(|c| c.status);
    assert_eq!(
        status,
        Some(ChunkStatus::Pending),
        "the chunk TRANSITIONED. A tracker close is not evidence the acceptance \
         criteria were met, and the machine must not decide that"
    );

    let p = proposal(&e, "c1").expect("a proposal must be recorded");
    assert_eq!(p.suggested_status, ChunkStatus::Done);
    assert!(
        p.source.starts_with("ext:fake/"),
        "a proposal a human cannot trace back is one they cannot evaluate: {}",
        p.source
    );
    assert!(
        p.reason
            .contains("not that the acceptance criteria were met")
    );
}

/// The proposal surface is the one that already exists. A second mechanism
/// doing the same job with different words is a thing a human has to learn
/// twice and will check once.
#[test]
fn the_proposal_sits_where_reprioritize_already_taught_people_to_look() {
    let mut e = engine();
    add(&mut e, "c1");

    e.propose_reprioritize("c1", 5, "faster".into())
        .expect("reprioritize");
    e.propose_status(
        "c1",
        ChunkStatus::Done,
        "the ticket closed".into(),
        "ext:fake/1".into(),
    )
    .expect("status");

    let c = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .expect("chunk");
    assert!(c.reprioritize.is_some(), "the established surface");
    assert!(c.status_proposal.is_some(), "the new one, alongside it");
    assert_eq!(c.priority, 10, "neither proposal touched the real value");
    assert_eq!(c.status, ChunkStatus::Pending);
}

/// A sweep runs on a schedule. Re-proposing the same thing must not restamp
/// `proposed_at`, or an old suggestion looks perpetually new and the reader
/// cannot tell what is actually recent.
#[tokio::test]
async fn an_unchanged_proposal_is_not_restamped() {
    let mut e = engine();
    add(&mut e, "c1");

    e.propose_status(
        "c1",
        ChunkStatus::Done,
        "same reason".into(),
        "ext:fake/1".into(),
    )
    .expect("first");
    let first = proposal(&e, "c1").expect("proposal").proposed_at;

    e.propose_status(
        "c1",
        ChunkStatus::Done,
        "same reason".into(),
        "ext:fake/1".into(),
    )
    .expect("second");
    let second = proposal(&e, "c1").expect("proposal").proposed_at;

    assert_eq!(
        first, second,
        "an unchanged suggestion was made to look new"
    );
}

/// A ticket nobody projected has no chunk to propose against. Inventing one
/// would be worse than silence.
#[tokio::test]
async fn a_ticket_we_never_projected_proposes_nothing() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");

    use think_and_ship::tracker::domain::WorkItem;
    use think_and_ship::tracker::port::TrackerPort;
    tracker.set_clock(T1);
    let mut stranger = WorkItem::new("someone else's ticket");
    stranger.state = WorkItemState::Done;
    tracker.upsert_item(&stranger).await.expect("upsert");

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");
    let touched = propose_status_from_sweep(&mut e, PROVIDER, &report);
    assert!(touched.is_empty());
}

/// THE CONCURRENCY TEST, and the definition is the point.
///
/// Concurrent here means BOTH SIDES CHANGED SINCE THE LAST SYNCHRONIZATION
/// POINT — the only sense a system with no shared clock can observe, and
/// exactly what the version fence detects. So: the plan is edited locally AND
/// the ticket is edited remotely, both after the same projection, and then one
/// projection runs against that doubly-moved state.
///
/// Two properties must hold. The outcome is DEFINED — the ownership table says
/// who wins per field, so there is no coin flip. And it CONVERGES — a second
/// projection changes nothing further, because a system that oscillates between
/// two writers never settles and every push rewrites the other side's work.
#[tokio::test]
async fn simultaneous_edits_converge_and_the_conflict_is_recorded() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    // BOTH sides move after the same synchronization point. Neither knows about
    // the other; this is what concurrency looks like without a shared clock.
    tracker.set_clock(T1);
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "renamed in the tracker".into();
        item.body = "rewritten in the tracker".into();
        item.state = WorkItemState::InProgress;
    });
    e.update_chunk(
        "c1",
        Some("renamed in the plan".into()),
        None,
        Some("rewritten in the plan".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");

    let first = project_all(&mut e, &tracker, None).await.expect("converge");

    // DEFINED: the table decided each field, and nothing was a coin flip.
    let after = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        after.title, "renamed in the tracker",
        "title is contested, so the remote value stands"
    );
    // The body is the whole projected document — description, acceptance
    // checklist, provenance footer — so assert on the part that moved.
    assert!(
        after.body.contains("rewritten in the plan"),
        "the body is ours, so the plan wins: {:?}",
        after.body
    );
    assert!(
        !after.body.contains("rewritten in the tracker"),
        "the tracker's body survived a field we own"
    );
    assert_eq!(
        after.state,
        WorkItemState::InProgress,
        "state is theirs and must survive"
    );

    // RECORDED: the losing edit was not discarded in silence.
    assert!(
        first
            .divergences
            .iter()
            .any(|(c, d)| c == "c1" && d.field == Field::Body),
        "the tracker's body edit was overwritten and nobody was told: {:?}",
        first.divergences
    );
    assert!(
        first
            .divergences
            .iter()
            .any(|(c, d)| c == "c1" && d.field == Field::Title),
        "the contested title disagreement was not recorded: {:?}",
        first.divergences
    );

    // REMEMBERED: the concession is durable state on the chunk, not a
    // one-round mood. Round 1 wrote a title proposal; a human can see it,
    // trace it, and resolve it.
    let concession = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .and_then(|c| c.title_proposal.clone())
        .expect("the concession must be recorded as a title proposal");
    assert_eq!(concession.suggested_title, "renamed in the tracker");
    assert!(
        concession.source.starts_with("ext:fake/"),
        "a proposal a human cannot trace back is one they cannot evaluate: {}",
        concession.source
    );

    // DURABLE: round 2 does NOT re-assert the plan's title. The open proposal
    // is the memory that we conceded, so the projector keeps deferring — and
    // because what it would send is exactly what it last wrote, it writes
    // nothing at all.
    let second = project_all(&mut e, &tracker, None).await.expect("again");
    let round2 = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        round2.title, "renamed in the tracker",
        "the contested value survived exactly one projection — the concession \
         was forgotten (tracker-contested-memory regressed)"
    );
    assert_eq!(
        second.writes(),
        0,
        "an already-conceded contest re-wrote the tracker"
    );

    // BOUNDED: it stays settled for as long as nobody resolves.
    let third = project_all(&mut e, &tracker, None).await.expect("third");
    let settled = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        settled.title, "renamed in the tracker",
        "the title unsettled"
    );
    assert!(
        settled.body.contains("rewritten in the plan"),
        "the body never settled"
    );
    assert_eq!(
        settled.state,
        WorkItemState::InProgress,
        "the state we do not own was disturbed on the way to settling"
    );
    assert_eq!(third.writes(), 0, "a settled system rewrote itself");

    // RESOLVED — reject: the human says the plan's title is right after all.
    // The proposal clears, and the very next projection pushes the plan's
    // title back; the round after that is quiet again.
    e.resolve_title_proposal("c1", false).expect("reject");
    let fourth = project_all(&mut e, &tracker, None).await.expect("fourth");
    let after_reject = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        after_reject.title, "renamed in the plan",
        "rejecting the proposal must let the plan's title flow again"
    );
    assert_eq!(fourth.writes(), 1, "the rejection is exactly one write");
    let fifth = project_all(&mut e, &tracker, None).await.expect("fifth");
    assert_eq!(fifth.writes(), 0, "resolution did not settle");
}

/// The other resolution: ACCEPT adopts the tracker's title into the plan.
/// After that the two sides agree, so the next projection is a no-op — the
/// cheap skip fires before any I/O.
#[tokio::test]
async fn accepting_the_concession_adopts_the_trackers_title() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    tracker.set_clock(T1);
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "the human's better title".into();
    });
    // A local edit forces a real patch, which is what surfaces the contest.
    e.update_chunk(
        "c1",
        None,
        None,
        Some("locally edited description".into()),
        None,
        None,
        None,
    )
    .expect("edit the plan");
    project_all(&mut e, &tracker, None).await.expect("round 1");

    e.resolve_title_proposal("c1", true).expect("accept");
    let c = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .expect("chunk");
    assert_eq!(
        c.title, "the human's better title",
        "accepting must adopt the tracker's title into the plan"
    );
    assert!(
        c.title_proposal.is_none(),
        "resolution must clear the proposal"
    );

    let after = project_all(&mut e, &tracker, None).await.expect("after");
    assert_eq!(after.writes(), 0, "agreement projected as activity");
    let remote = tracker.item(&link.external_id).expect("item");
    assert_eq!(remote.title, "the human's better title");
}

/// Criterion 2, at the integration level: an ordinary local rename that
/// nobody contested reaches the tracker. Durable concession must not become
/// "the plan can never rename anything again".
#[tokio::test]
async fn an_uncontested_local_rename_still_reaches_the_tracker() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");
    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    e.update_chunk(
        "c1",
        Some("a plain local rename".into()),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("rename");
    project_all(&mut e, &tracker, None).await.expect("push");

    let remote = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        remote.title, "a plain local rename",
        "an uncontested rename never reached the tracker"
    );
}

/// The projection runs on every push, so re-projecting the same disagreement
/// must not restamp `proposed_at` — the exact discipline `propose_status`
/// already has, proven here for its sibling.
#[test]
fn an_unchanged_title_proposal_is_not_restamped() {
    let mut e = engine();
    add(&mut e, "c1");

    e.propose_title(
        "c1",
        "their title".into(),
        "same reason".into(),
        "ext:fake/1".into(),
    )
    .expect("first");
    let first = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .and_then(|c| c.title_proposal.clone())
        .expect("proposal")
        .proposed_at;

    e.propose_title(
        "c1",
        "their title".into(),
        "same reason".into(),
        "ext:fake/1".into(),
    )
    .expect("second");
    let second = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .and_then(|c| c.title_proposal.clone())
        .expect("proposal")
        .proposed_at;

    assert_eq!(
        first, second,
        "an unchanged suggestion was made to look new"
    );
}

/// An explicit local title edit is the human speaking about exactly the field
/// the proposal is about — it disposes the proposal, or the projector would
/// keep sending the conceded value over the edit the human just made.
#[test]
fn an_explicit_title_edit_disposes_the_open_proposal() {
    let mut e = engine();
    add(&mut e, "c1");
    e.propose_title(
        "c1",
        "their title".into(),
        "contested".into(),
        "ext:fake/1".into(),
    )
    .expect("propose");

    e.update_chunk(
        "c1",
        Some("the human's own new title".into()),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("edit");

    let c = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .expect("chunk");
    assert!(
        c.title_proposal.is_none(),
        "the proposal outlived an explicit human edit to the same field"
    );
    assert_eq!(c.title, "the human's own new title");
}

/// "Resolved" must mean something happened. Resolving a chunk with no open
/// proposal is an error, not a silent no-op.
#[test]
fn resolving_without_an_open_proposal_is_loud() {
    let mut e = engine();
    add(&mut e, "c1");
    let err = e
        .resolve_title_proposal("c1", true)
        .expect_err("must refuse");
    assert!(err.contains("no open title proposal"), "{err}");
}
