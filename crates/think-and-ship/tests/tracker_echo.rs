//! Echo suppression: the sync stops talking to itself.
//!
//! The unit tests in `tracker::echo` prove the classifier's truth table. This
//! file proves the thing a truth table cannot: that wiring the classifier into
//! the real projector actually CONVERGES. A rule can be correct on every row
//! and still loop if the system feeds it the wrong values, so the load-bearing
//! assertion here is a write count that stops growing.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::echo::{Verdict, classify};
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::port::TrackerPort;
use think_and_ship::tracker::project::project_all;

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

/// One full cycle of the loop we are trying to prevent: project, then let the
/// tracker deliver what it now holds back to us as if by webhook, and classify
/// it. Returns the verdict the inbound path would reach.
async fn round_trip(e: &mut RoadmapEngine, tracker: &FakeTracker, chunk: &str) -> Verdict {
    project_all(e, tracker, None).await.expect("project");

    let link = e
        .tracker_link(chunk, PROVIDER)
        .expect("projection must have recorded a link")
        .clone();
    let delivered = tracker
        .item(&link.external_id)
        .expect("the tracker holds the item we just wrote");

    classify(&delivered, Some(&link))
}

/// Drive the cycle repeatedly and assert the write count converges.
///
/// HONEST SCOPE: the convergence here is delivered by the projector's own
/// content-hash short-circuit in `project_all`, not by the classifier — the
/// projector was already refusing no-op writes on its own. What this test
/// proves is that the two mechanisms AGREE: the classifier returns an echo
/// exactly where the projector declines to write. Two independent answers
/// concurring is worth asserting, but it is not the same claim as "the
/// classifier stops the loop". For that, see the test below, which removes the
/// projector from the picture entirely.
#[tokio::test]
async fn the_write_count_converges_and_the_classifier_agrees() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);

    let first = round_trip(&mut e, &tracker, "c1").await;
    assert_eq!(tracker.writes(), 1, "the first pass creates the item");
    assert!(
        first.is_echo(),
        "our own write, delivered straight back, must not read as a remote change"
    );

    for round in 2..=6 {
        let verdict = round_trip(&mut e, &tracker, "c1").await;
        assert!(
            verdict.is_echo(),
            "round {round} classified our own item as {verdict:?} — that is the loop"
        );
        assert_eq!(
            tracker.writes(),
            1,
            "round {round} wrote again; the sync is amplifying itself"
        );
    }
}

/// Convergence must not come at the cost of deafness. A human edits the item in
/// the tracker's own UI; the very next inbound delivery has to say REMOTE, or we
/// have built a system that is quiet because it stopped listening.
#[tokio::test]
async fn a_real_remote_edit_still_gets_through_after_convergence() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);

    for _ in 0..3 {
        assert!(round_trip(&mut e, &tracker, "c1").await.is_echo());
    }

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();
    tracker.set_clock("2026-07-26T12:00:00+00:00");
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "a human retitled this".into();
    });

    let edited = tracker.item(&link.external_id).expect("item");
    assert_eq!(
        classify(&edited, Some(&link)),
        Verdict::Remote,
        "a human's edit must reach us even after the echo fence has settled"
    );
}

/// Several chunks converge independently. A fence that keys on anything coarser
/// than (chunk, provider) would let one item's write suppress another's.
#[tokio::test]
async fn many_chunks_converge_without_interfering() {
    let mut e = engine();
    for id in ["c1", "c2", "c3"] {
        add(&mut e, id);
    }
    let tracker = FakeTracker::new(PROVIDER);

    project_all(&mut e, &tracker, None).await.expect("project");
    assert_eq!(tracker.writes(), 3, "one write per chunk on the first pass");

    for round in 2..=5 {
        for id in ["c1", "c2", "c3"] {
            let verdict = round_trip(&mut e, &tracker, id).await;
            assert!(
                verdict.is_echo(),
                "round {round} chunk {id} read as {verdict:?}"
            );
        }
        assert_eq!(tracker.writes(), 3, "round {round} wrote again");
    }
}

/// An item the tracker holds that we have never projected — someone else's
/// ticket in the same repo — is a remote change, never an echo. Getting this
/// wrong means silently ignoring every issue we did not create.
#[tokio::test]
async fn an_item_we_never_wrote_is_remote() {
    let tracker = FakeTracker::new(PROVIDER);
    let stranger = think_and_ship::tracker::domain::WorkItem::new("someone else's ticket");
    let outcome = tracker.upsert_item(&stranger).await.expect("upsert");
    let held = tracker.item(&outcome.external_id).expect("item");

    assert_eq!(classify(&held, None), Verdict::Remote);
}

/// The drift signal, end to end. An adapter whose inbound parse does not invert
/// its outbound build produces content that will not hash to what we wrote —
/// while the provider still reports the record as unmoved. That combination is
/// evidence about the ADAPTER, and the fence reports it instead of looping.
#[tokio::test]
async fn a_lossy_round_trip_reports_drift_rather_than_looping() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("project");

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();
    let mut delivered = tracker.item(&link.external_id).expect("item");

    // Simulate the lossy parse: the provider handed back an assignee's display
    // name where we sent an identifier. Nothing on their side changed — the
    // version is untouched — but our hash of the readback no longer matches.
    delivered.assignee = Some("Ada Lovelace".into());

    let verdict = classify(&delivered, Some(&link));
    assert_eq!(
        verdict,
        Verdict::EchoWithDrift,
        "an unmoved version with a mismatched hash is adapter loss, not a remote edit"
    );
    assert!(
        verdict.is_echo(),
        "drift must still suppress, or naming the problem would cause the loop"
    );
}

/// THE regression test for the classifier, with the projector taken out of the
/// picture so nothing but the classifier can stop the loop.
///
/// This models the inbound webhook path, where no content-hash
/// short-circuit exists to help: an event arrives, and the ONLY thing standing
/// between it and another outbound write is the verdict. A `Remote` here means
/// a real write, which means a new event, which means the runaway. Counting the
/// writes the classifier would authorize is therefore the honest measure.
#[tokio::test]
async fn the_classifier_alone_authorizes_no_further_writes() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("project");

    let mut link = e.tracker_link("c1", PROVIDER).expect("link").clone();
    let mut authorized_writes = 0;

    for round in 1..=8 {
        let delivered = tracker.item(&link.external_id).expect("item");

        if classify(&delivered, Some(&link)) == Verdict::Remote {
            // The inbound path believed this was a genuine remote change, so it
            // writes — and the write is what fires the next event. Reproduce
            // that faithfully rather than merely counting.
            authorized_writes += 1;
            let outcome = tracker.upsert_item(&delivered).await.expect("re-write");
            e.record_tracker_link(
                "c1",
                PROVIDER,
                &outcome.external_id,
                &delivered.content_hash(),
                outcome.version.as_deref(),
            )
            .expect("record");
            link = e.tracker_link("c1", PROVIDER).expect("link").clone();
        }

        assert_eq!(
            authorized_writes, 0,
            "round {round}: the classifier authorized a write on our own echo — \
             each one fires another event, and that is the runaway"
        );
    }
}

/// The falsification. If the classifier were replaced by the naive rule this
/// chunk rejects — trust the actor, and our own actor means ignore — the human
/// edit in `a_real_remote_edit_still_gets_through_after_convergence` would be
/// dropped. This asserts the classifier's answer differs from that rule on
/// exactly the case that distinguishes them, so a future rewrite to the cheap
/// heuristic cannot pass this file.
#[tokio::test]
async fn the_verdict_is_not_reducible_to_who_made_the_change() {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    project_all(&mut e, &tracker, None).await.expect("project");

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    // Two events, INDISTINGUISHABLE by actor: both arrive attributed to us,
    // because both were made with our token. One is our echo; one is a human
    // typing in the tracker's UI while authenticated as our integration.
    let our_echo = tracker.item(&link.external_id).expect("item");

    tracker.set_clock("2026-07-26T12:00:00+00:00");
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "a human retitled this, using our token".into();
    });
    let human_edit = tracker.item(&link.external_id).expect("item");

    assert_ne!(
        classify(&our_echo, Some(&link)),
        classify(&human_edit, Some(&link)),
        "these two are identical to any actor-based filter; the fence must still \
         tell them apart, or a human's work disappears silently"
    );
    assert!(classify(&our_echo, Some(&link)).is_echo());
    assert_eq!(classify(&human_edit, Some(&link)), Verdict::Remote);
}
