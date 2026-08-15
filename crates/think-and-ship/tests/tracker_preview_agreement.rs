//! Does `tracker push --dry-run` predict `tracker push`? (tracker-push-outcome-invisible)
//!
//! # The bug this file exists to make impossible
//!
//! Against the live THI workspace the preview said 7 of 46 items "would be
//! updated" while the real push said "0 created, 0 updated, 46 unchanged" —
//! stable across a real write, and reproducible days later. A preview whose
//! entire purpose is to predict the write was wrong about 15% of the items, in
//! the direction that manufactures phantom work.
//!
//! # Why it happened, and why prose could not have prevented it
//!
//! The projector has TWO skip gates. The first is cheap and pre-I/O: is the raw
//! item's `content_hash` what we last wrote? The second runs after `fetch_one`
//! and `reconcile_fields`: is the RECONCILED item's hash what we last wrote —
//! and that second hash is also the one it STORES. The CLI's preview
//! reimplemented gate one and knew nothing of gate two.
//!
//! The two gates disagree permanently wherever the ownership table hands a
//! field to the team. `State`, `Labels` and `Assignee` are all `Owner::Theirs`
//! by default, and all three are inside `content_hash`. So a chunk whose Linear
//! state moved on without us mismatches gate one on every run forever, while
//! gate two correctly skips it forever.
//!
//! # The refuted hypothesis
//!
//! An earlier diagnosis guessed the roof INITIATIVE was in the hash and that
//! the two paths therefore hashed different bodies. It is refuted and recorded
//! here so nobody re-chases it: `content_hash` does not cover the initiative,
//! and `to_work_item` takes no initiative argument on either path.
//! `the_initiative_is_not_in_the_hash` below is the executable form of that.
//!
//! # The shape of every test here
//!
//! Each pairs a proof that the preview is now RIGHT with a proof of the
//! guarantee that precision could have cost — an honest predicate is easy to
//! get by answering "unchanged" to everything, and that would be a worse bug
//! than the one being fixed.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::domain::{TrackerCapabilities, WorkItemState};
use think_and_ship::tracker::ownership::{Field, Owner, Ownership, authored_hash};
use think_and_ship::tracker::project::{
    PreviewVerdict, ProjectionOutcome, preview_verdict, project_all_with_policy, to_work_item,
};
use think_and_ship::tracker::{FakeTracker, TrackerPort, WorkItem};

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

/// The preview, computed exactly as the CLI computes it.
fn preview(e: &RoadmapEngine, id: &str, caps: &TrackerCapabilities) -> PreviewVerdict {
    let chunk = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == id)
        .expect("chunk exists");
    let item = to_work_item(e, chunk, PROVIDER, caps);
    preview_verdict(e.tracker_link(id, PROVIDER), &item, &Ownership::default())
}

async fn push(e: &mut RoadmapEngine, t: &FakeTracker) -> Vec<(String, ProjectionOutcome)> {
    project_all_with_policy(e, t, None, &Ownership::default(), None)
        .await
        .expect("projection")
        .outcomes
}

fn outcome_for(outcomes: &[(String, ProjectionOutcome)], id: &str) -> ProjectionOutcome {
    outcomes
        .iter()
        .find(|(c, _)| c == id)
        .map(|(_, o)| o.clone())
        .expect("chunk was in the run")
}

// ── THE AGREEMENT INVARIANT ─────────────────────────────────────────────────

/// THE regression test for the live bug, on the STATE axis.
///
/// The live sequence exactly, and it takes BOTH halves — the tracker's value
/// moving AND the plan's moving after it. As observed on a real issue:
/// the team started the issue in Linear, the plan later went `done`, and from
/// then on the preview said "would be updated" on every run forever while the
/// projector skipped it on every run forever.
#[tokio::test]
async fn a_tracker_owned_state_edit_makes_the_projector_skip_and_the_preview_must_agree() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");

    let created = push(&mut e, &t).await;
    let ext = match outcome_for(&created, "a") {
        ProjectionOutcome::Created { external_id } => external_id,
        other => panic!("expected a create, got {other:?}"),
    };

    // (1) The team starts the issue. `State` is `Owner::Theirs`, so their value
    // is authoritative and we will never write over it.
    t.remote_edit(&ext, |i| i.state = WorkItemState::InProgress);

    // (2) The plan finishes the chunk. This moves a field we DO author — the
    // provenance footer carries `status`, and the body is `Owner::Ours` — so
    // the cheap gate mismatches and a real reconciling write happens.
    e.set_status("a", ChunkStatus::InProgress).expect("start");
    e.set_status("a", ChunkStatus::Done).expect("finish");

    let adopted = push(&mut e, &t).await;
    assert!(
        matches!(
            outcome_for(&adopted, "a"),
            ProjectionOutcome::Patched { .. }
        ),
        "the plan's own change must still reach the tracker"
    );

    // (3) THE PERMANENT DIVERGENCE IS NOW SET. What we stored is what we SENT:
    // the new footer plus THEIR `InProgress`. What the raw item says is the new
    // footer plus the plan's `Done`. Those two can never converge, because the
    // plan's state is not ours to send — so the cheap gate mismatches on every
    // future run, and the old preview announced a pending update on every
    // future run. Nothing changes from here on.
    let writes_before = t.writes();
    let third = push(&mut e, &t).await;
    assert!(
        matches!(outcome_for(&third, "a"), ProjectionOutcome::Skipped { .. }),
        "the projector must skip a chunk that differs only where the tracker owns the field"
    );
    assert_eq!(t.writes(), writes_before, "and must make no call at all");

    // THE CLAIM.
    let verdict = preview(&e, "a", &caps);
    assert!(
        !verdict.promises_a_write(),
        "the preview promised a write the projector did not perform: {verdict:?}"
    );
    assert_eq!(verdict, PreviewVerdict::OnlyTrackerOwned);
}

/// The same invariant on the LABELS axis — the second shape found live, where
/// an issue holds `roadmap:critical` while the plan's band says `medium`.
///
/// A separate test rather than a parameter, because the two axes reach the
/// divergence through different fields and a fix could plausibly cover one.
#[tokio::test]
async fn a_tracker_owned_label_edit_also_agrees() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");

    let created = push(&mut e, &t).await;
    let ext = match outcome_for(&created, "a") {
        ProjectionOutcome::Created { external_id } => external_id,
        other => panic!("expected a create, got {other:?}"),
    };

    // The team relabels. `Labels` is `Owner::Theirs`, so their set is
    // authoritative. The plan's band also moves — priority 10 (critical) → 350
    // (low) — which changes nothing we author, but the description does, so a
    // reconciling write happens and stores THEIR labels.
    t.remote_edit(&ext, |i| i.labels = vec!["team-owned-label".into()]);
    e.update_chunk(
        "a",
        None,
        Some(350),
        Some("a description that moved too".into()),
        None,
        None,
        None,
    )
    .expect("reprioritize");
    let adopted = push(&mut e, &t).await;
    assert!(matches!(
        outcome_for(&adopted, "a"),
        ProjectionOutcome::Patched { .. }
    ));

    let writes_before = t.writes();
    let third = push(&mut e, &t).await;
    assert!(matches!(
        outcome_for(&third, "a"),
        ProjectionOutcome::Skipped { .. }
    ));
    assert_eq!(t.writes(), writes_before);
    assert_eq!(preview(&e, "a", &caps), PreviewVerdict::OnlyTrackerOwned);
}

/// THE PAIRED GUARANTEE, and the one that stops the cheap fix.
///
/// A preview that answered "nothing would be written" to everything would pass
/// every test above and be far more useless than the bug. A real edit to a
/// field WE author must still be predicted as a write — and the projector must
/// still perform it.
#[tokio::test]
async fn an_edit_to_a_field_we_author_is_still_predicted_and_still_written() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");
    push(&mut e, &t).await;

    // `Body` is `Owner::Ours`. Changing the description is a genuine pending
    // write and nothing about ownership can excuse it.
    e.update_chunk(
        "a",
        None,
        None,
        Some("a materially different description".into()),
        None,
        None,
        None,
    )
    .expect("update");

    let verdict = preview(&e, "a", &caps);
    assert_eq!(verdict, PreviewVerdict::WouldUpdate);
    assert!(verdict.promises_a_write());

    // And the promise is kept.
    let writes_before = t.writes();
    let second = push(&mut e, &t).await;
    assert!(matches!(
        outcome_for(&second, "a"),
        ProjectionOutcome::Patched { .. }
    ));
    assert!(
        t.writes() > writes_before,
        "the preview promised a write, so a call must actually have been made"
    );
}

/// The unchanged case must stay exactly as certain as it was: this is the
/// projector's own first gate, character for character, and a fix that made it
/// fuzzy would turn every no-op run into noise.
#[tokio::test]
async fn a_genuinely_unchanged_chunk_is_still_reported_certain() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");
    push(&mut e, &t).await;

    assert_eq!(preview(&e, "a", &caps), PreviewVerdict::Unchanged);

    let writes_before = t.writes();
    let second = push(&mut e, &t).await;
    assert!(matches!(
        outcome_for(&second, "a"),
        ProjectionOutcome::Skipped { .. }
    ));
    assert_eq!(
        t.writes(),
        writes_before,
        "an unchanged chunk must make no call at all"
    );
}

/// A chunk with no link is a create, and the preview must say so — the one
/// verdict that was never wrong, asserted so a refactor cannot lose it.
#[tokio::test]
async fn an_unprojected_chunk_reads_as_a_create() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");

    let verdict = preview(&e, "a", &caps);
    assert_eq!(verdict, PreviewVerdict::WouldCreate);
    assert!(verdict.promises_a_write());

    let outcomes = push(&mut e, &t).await;
    assert!(matches!(
        outcome_for(&outcomes, "a"),
        ProjectionOutcome::Created { .. }
    ));
}

// ── THE FENCE ITSELF ────────────────────────────────────────────────────────

/// A link written before the authored fence existed carries no evidence. It
/// must say so rather than pick the flattering answer in either direction.
#[tokio::test]
async fn a_pre_fence_link_admits_it_cannot_tell() {
    let t = FakeTracker::new(PROVIDER);
    let caps = t.capabilities();
    let mut e = engine();
    add(&mut e, "a");
    push(&mut e, &t).await;

    // Exactly the on-disk shape of a link written by an older binary.
    let mut link = e.tracker_link("a", PROVIDER).expect("link").clone();
    link.our_last_authored_hash = None;
    let chunk = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "a")
        .expect("chunk")
        .clone();
    let mut item = to_work_item(&e, &chunk, PROVIDER, &caps);
    // Make the full hash differ, as a tracker-owned drift would.
    item.state = WorkItemState::Done;

    assert_eq!(
        preview_verdict(Some(&link), &item, &Ownership::default()),
        PreviewVerdict::Unknown
    );
}

/// The fence is derived from the POLICY, not from a hardcoded copy of the
/// default table. A project that takes `State` back must see a state change as
/// a real pending write again.
#[test]
fn the_authored_digest_follows_the_table_rather_than_restating_it() {
    let mut base = WorkItem::new("t");
    base.body = "b".into();
    let mut moved = base.clone();
    moved.state = WorkItemState::Done;

    // Default table: State is Theirs, so a state change is not ours to author.
    let default = Ownership::default();
    assert_eq!(
        authored_hash(&default, &base),
        authored_hash(&default, &moved),
        "under the default table a state change is invisible to the authored digest"
    );

    // Take it back and the very same pair must now differ.
    let ours = Ownership::default().with(Field::State, Owner::Ours);
    assert_ne!(
        authored_hash(&ours, &base),
        authored_hash(&ours, &moved),
        "a project that owns State must see a state change as authored content"
    );
}

/// The authored digest must not collapse everything: a body change has to
/// survive it, or `OnlyTrackerOwned` would swallow real work.
#[test]
fn the_authored_digest_still_separates_a_body_change() {
    let policy = Ownership::default();
    let mut a = WorkItem::new("t");
    a.body = "one".into();
    let mut b = a.clone();
    b.body = "two".into();
    assert_ne!(authored_hash(&policy, &a), authored_hash(&policy, &b));
}

// ── THE REFUTED HYPOTHESIS, MADE EXECUTABLE ─────────────────────────────────

/// The carried hypothesis was that the roof initiative participates in the
/// content hash, so the preview (which passes none) and the projector (which
/// passes one) hash different bodies. It is false, and this pins it false so
/// the diagnosis cannot be re-run.
///
/// Two independent proofs: the projector's own initiative argument changes no
/// item's hash, and `to_work_item` has no initiative parameter to differ on in
/// the first place — the latter is enforced by this file compiling at all.
#[tokio::test]
async fn the_initiative_is_not_in_the_hash() {
    let caps = FakeTracker::new(PROVIDER).capabilities();

    let mut with_roof = engine();
    add(&mut with_roof, "a");
    let t1 = FakeTracker::new(PROVIDER);
    project_all_with_policy(
        &mut with_roof,
        &t1,
        None,
        &Ownership::default(),
        Some("some-initiative"),
    )
    .await
    .expect("projection");

    let mut without_roof = engine();
    add(&mut without_roof, "a");
    let t2 = FakeTracker::new(PROVIDER);
    project_all_with_policy(&mut without_roof, &t2, None, &Ownership::default(), None)
        .await
        .expect("projection");

    assert_eq!(
        with_roof
            .tracker_link("a", PROVIDER)
            .expect("link")
            .our_last_write_hash,
        without_roof
            .tracker_link("a", PROVIDER)
            .expect("link")
            .our_last_write_hash,
        "the initiative must not change the content hash — if this fails the \
         refuted hypothesis was right after all"
    );

    // And with a roof raised, the preview still reads unchanged: the roof is a
    // container, not content.
    assert_eq!(preview(&with_roof, "a", &caps), PreviewVerdict::Unchanged);
}
