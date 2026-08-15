//! The sweep: state recovers when no webhook ever arrives.
//!
//! A warning worth keeping: it is easy to write a test that merely proves
//! `fetch_since` works and call it a webhook-loss test. The difference is
//! `FakeTracker::remote_edit`, which mutates an item WITHOUT touching the write
//! counter and without any event — exactly what a dropped delivery looks like
//! from our side. Every test here that claims to cover loss uses it.

use std::path::Path;

use tempfile::TempDir;
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::project::project_all;
use think_and_ship::tracker::sweep::{self, reconcile};

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

/// Project one chunk so there is a link to classify against.
async fn projected(dir: &Path) -> (RoadmapEngine, FakeTracker) {
    let mut e = engine();
    add(&mut e, "c1");
    let tracker = FakeTracker::new(PROVIDER);
    tracker.set_clock(T0);
    project_all(&mut e, &tracker, None).await.expect("project");
    let _ = dir;
    (e, tracker)
}

/// THE test this file exists for. A human edits the ticket and the webhook
/// never arrives — no event, nothing in any queue, our side has no idea. The
/// sweep is the only thing that can notice, and it must.
#[tokio::test]
async fn a_remote_edit_that_fired_no_webhook_is_surfaced_by_the_sweep() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();

    // The dropped delivery: a real change upstream, and NOTHING tells us.
    tracker.set_clock(T1);
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "a human retitled this and the webhook was lost".into();
    });

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");

    assert_eq!(
        report.remote.len(),
        1,
        "the edit reached us by no other path; if the sweep misses it the plan \
         is silently stale forever"
    );
    assert_eq!(
        report.remote[0].title,
        "a human retitled this and the webhook was lost"
    );
    assert_eq!(report.echoes, 0);
}

/// The sweep must not re-surface our own writes. Without the 5a classifier it
/// would report every projected chunk as a remote change on every run, and the
/// backstop would become the loop.
#[tokio::test]
async fn our_own_writes_come_back_as_echoes_not_as_changes() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");

    assert_eq!(report.fetched, 1);
    assert_eq!(report.echoes, 1, "our own projection is not news");
    assert!(report.remote.is_empty());
}

/// THE watermark rule. It advances to the instant the run STARTED, never to the
/// newest record's stamp — otherwise anything written between the provider's
/// response and the end of processing falls into a gap that no later sweep asks
/// for, and no webhook is coming, because that is the case this exists for.
#[tokio::test]
async fn the_watermark_advances_to_the_run_start_not_to_the_newest_record() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    // The newest record is stamped T0; the run starts much later at T2.
    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");

    assert_eq!(report.advanced_to.as_deref(), Some(T2));
    let marks = sweep::load(dir.path(), "proj");
    assert_eq!(
        marks.since(PROVIDER),
        Some(T2),
        "storing the newest record's stamp ({T0}) would re-open a window that \
         was already processed and lose anything written during the sweep"
    );
}

/// A sweep that fails partway must leave the window open. The next run redoes
/// it — costing one extra classification, which is free because classify is
/// pure — rather than skipping whatever it never got to.
#[tokio::test]
async fn a_failed_sweep_does_not_advance_the_watermark() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    // First sweep succeeds and closes the window at T1.
    reconcile(&e, &tracker, dir.path(), T1)
        .await
        .expect("sweep");
    assert_eq!(sweep::load(dir.path(), "proj").since(PROVIDER), Some(T1));

    // The next one dies inside fetch_since.
    tracker.fail_next(think_and_ship::tracker::port::TrackerError::Transport(
        "the provider hung up".into(),
    ));
    let err = reconcile(&e, &tracker, dir.path(), T2).await;
    assert!(
        err.is_err(),
        "a transport failure must surface, not be swallowed"
    );

    assert_eq!(
        sweep::load(dir.path(), "proj").since(PROVIDER),
        Some(T1),
        "the watermark moved despite the sweep failing — everything in the \
         unprocessed window is now unreachable"
    );
}

/// The first sweep of a provider must ask for EVERYTHING. Treating "no
/// watermark" as "nothing to fetch" would make the backstop a silent no-op
/// until some other path happened to write one.
#[tokio::test]
async fn the_first_sweep_asks_for_everything() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    assert_eq!(sweep::load(dir.path(), "proj").since(PROVIDER), None);
    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");
    assert_eq!(
        report.fetched, 1,
        "with no watermark the sweep must fetch from the beginning of time"
    );
}

/// Two providers keep independent windows. A shared watermark would let one
/// provider's successful sweep close another's unprocessed window.
#[tokio::test]
async fn providers_do_not_share_a_window() {
    let dir = TempDir::new().expect("tempdir");
    let (e, fake) = projected(dir.path()).await;

    reconcile(&e, &fake, dir.path(), T1).await.expect("sweep");

    let marks = sweep::load(dir.path(), "proj");
    assert_eq!(marks.since(PROVIDER), Some(T1));
    assert_eq!(
        marks.since("github"),
        None,
        "one provider's progress must never imply another's"
    );
}

/// An item we never projected — someone else's ticket in the same team — is a
/// remote change, not an echo. Getting this wrong means ignoring every issue we
/// did not create.
#[tokio::test]
async fn a_ticket_we_never_projected_is_remote() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;

    use think_and_ship::tracker::domain::WorkItem;
    use think_and_ship::tracker::port::TrackerPort;
    tracker.set_clock(T1);
    tracker
        .upsert_item(&WorkItem::new("someone else's ticket"))
        .await
        .expect("upsert");

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");

    assert_eq!(report.fetched, 2);
    assert_eq!(report.echoes, 1, "ours");
    assert_eq!(report.remote.len(), 1, "theirs");
    assert_eq!(report.remote[0].title, "someone else's ticket");
}

/// The sweep REPORTS. It must not write to the tracker, and it must not decide
/// what a remote change means — that is the conflict policy's job, and doing it
/// here would be rebuilding that policy badly, in the wrong place.
#[tokio::test]
async fn the_sweep_writes_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let (e, tracker) = projected(dir.path()).await;
    let writes_before = tracker.writes();

    let link = e.tracker_link("c1", PROVIDER).expect("link").clone();
    tracker.set_clock(T1);
    tracker.remote_edit(&link.external_id, |item| {
        item.title = "a change the sweep must not act on".into();
    });

    let report = reconcile(&e, &tracker, dir.path(), T2)
        .await
        .expect("sweep");

    assert_eq!(report.remote.len(), 1, "it noticed");
    assert_eq!(
        tracker.writes(),
        writes_before,
        "and it did nothing about it, which is correct: deciding belongs to the \
         conflict policy, not to detection"
    );
}
