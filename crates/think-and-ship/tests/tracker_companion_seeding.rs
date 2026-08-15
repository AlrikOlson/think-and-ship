//! projects-v2-board-link-seeding: a patch-only lane gets its first identities
//! from a lane that can create, and the copy is made where neither adapter can
//! see it.
//!
//! The decision these gates hold: a GitHub Projects v2 board is not
//! a destination you switch TO — a board item wraps an issue that already
//! exists, so the board is a COMPANION to an issues lane. The seed is what makes
//! that companion useful, and two things about it are easy to get wrong in ways
//! no ordinary test would notice:
//!
//! 1. Seeding with the source lane's content hash would make the companion's
//!    first push SKIP every chunk, because that hash is exactly what the
//!    projector compares to decide "nothing changed". The loud refusal would
//!    become a silent no-op.
//! 2. Copying identities for chunks the companion was never opted into, or
//!    overwriting identities it already has, would make the seed a synchroniser
//!    rather than a first-identity-only step.
//!
//! Every negative here is paired with a positive in the SAME test against the
//! same engine, so none of them can pass by doing nothing.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::config::{CompanionLane, TrackerConfig, companion_lane};
use think_and_ship::tracker::domain::TrackerCapabilities;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::project::{ProjectionOutcome, project_all, to_work_item};
use think_and_ship::tracker::seed::seed_links_from;

/// The issues lane, and the board that follows it. Named once so a rename
/// cannot leave half the file pointing at the old key.
const ISSUES: &str = "github";
const BOARD: &str = "github_projects";

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
}

fn opt_in(e: &mut RoadmapEngine, id: &str, provider: &str) {
    e.set_tracker_opt_in(id, provider, true).expect("opt in");
}

fn chunk_of<'a>(e: &'a RoadmapEngine, id: &str) -> &'a think_and_ship::roadmap::domain::Chunk {
    e.roadmap()
        .chunks
        .iter()
        .find(|c| c.id == id)
        .expect("chunk exists")
}

/// THE gate. A chunk the issues lane has already filed reaches the board's lane
/// carrying the ISSUES lane's coordinate — the exact `owner/repo#number` a board
/// item needs — and a chunk the issues lane has never filed is left alone rather
/// than given a made-up one.
#[test]
fn the_board_takes_the_issue_coordinate_the_issues_lane_already_recorded() {
    let mut e = engine();
    add(&mut e, "filed");
    add(&mut e, "not-filed");
    // Both want the board. Only one has an issue.
    opt_in(&mut e, "filed", BOARD);
    opt_in(&mut e, "not-filed", BOARD);
    e.record_tracker_link(
        "filed",
        ISSUES,
        "owner/repo#7",
        "hash-of-our-last-issue",
        None,
    )
    .expect("issues link");

    let report = seed_links_from(&mut e, ISSUES, BOARD).expect("seed");

    // LOAD-BEARING FIRST: the board's own link now holds the issue coordinate.
    assert_eq!(
        e.tracker_link("filed", BOARD)
            .map(|l| l.external_id.clone()),
        Some("owner/repo#7".to_string()),
        "the board lane must carry the issues lane's coordinate verbatim"
    );
    assert_eq!(report.seeded, vec!["filed".to_string()]);
    // THE PAIRED NEGATIVE, same engine, same pass: nothing is invented for a
    // chunk the source lane has never written.
    assert!(
        e.tracker_link("not-filed", BOARD).is_none(),
        "a chunk with no issue must not be given a board identity"
    );
    assert_eq!(report.not_yet_upstream, 1);
    // And the source lane is untouched — a seed reads it, never writes it.
    assert_eq!(
        e.tracker_link("filed", ISSUES)
            .map(|l| l.our_last_write_hash.clone()),
        Some("hash-of-our-last-issue".to_string())
    );
}

/// THE TRAP, proven at the projector itself rather than at the preview.
///
/// The obvious implementation seeds the link with the SOURCE lane's content
/// hash. That hash is character for character what the projector's cheap skip
/// compares, so the companion's first push would skip every chunk: the loud
/// refusal this whole chunk removes would become a silent no-op, which is worse,
/// because a refusal at least says what to fix.
///
/// The positive control is the same chunk, the same double, the same run —
/// re-seeded by hand with the hash of what we are about to send. It goes to
/// `Skipped`, which is what proves the empty hash is load-bearing rather than
/// incidental.
///
/// NOT asserted here, and worth stating: `preview_verdict` reports a seeded link
/// as `Unknown` ("cannot tell without reading the tracker"), because a seed has
/// no authored hash — it is exactly the pre-fence link
/// `tracker-preview-precision-backfill` is about. The preview being unsure and
/// the projector writing are both correct; only the projector is the contract.
#[tokio::test]
async fn a_seeded_link_is_written_and_a_matching_hash_would_be_skipped() {
    let mut e = engine();
    add(&mut e, "c1");
    opt_in(&mut e, "c1", BOARD);
    // THE COLLISION IS REAL, and this is what makes the trap dangerous rather
    // than theoretical: `content_hash` deliberately ignores external_id and
    // version, so two lanes with the same capabilities produce the SAME hash for
    // the same chunk. The issues lane's recorded hash is therefore exactly what
    // the board is about to send.
    let item = to_work_item(&e, chunk_of(&e, "c1"), BOARD, &TrackerCapabilities::full());
    e.record_tracker_link("c1", ISSUES, "owner/repo#7", &item.content_hash(), None)
        .expect("issues link");
    seed_links_from(&mut e, ISSUES, BOARD).expect("seed");

    let board = FakeTracker::new(BOARD);

    let report = project_all(&mut e, &board, None).await.expect("push");

    // LOAD-BEARING FIRST: the seeded chunk was NOT skipped. The double rejects
    // an id it has never seen, which is itself proof the write was attempted
    // against the seeded coordinate.
    assert!(
        !matches!(report.outcomes[0].1, ProjectionOutcome::Skipped { .. }),
        "a seeded link must not be skipped, or the board's first push does nothing at all"
    );
    assert_ne!(
        e.tracker_link("c1", BOARD)
            .map(|l| l.our_last_write_hash.as_str()),
        Some(item.content_hash().as_str()),
        "the seeded hash must be one no payload can produce"
    );

    // THE POSITIVE CONTROL: same chunk, same double, hash that matches what we
    // would send — and the projector skips. So the assertion above is about the
    // seed's hash, not about this projector never skipping.
    e.record_tracker_link("c1", BOARD, "owner/repo#7", &item.content_hash(), None)
        .expect("relink");
    let again = project_all(&mut e, &board, None).await.expect("push");
    assert!(
        matches!(again.outcomes[0].1, ProjectionOutcome::Skipped { .. }),
        "a link whose hash matches what we would send must be skipped"
    );
}

/// A seed gives FIRST identities only. Running it before every push — which is
/// what the push path does — must cost nothing after the first, and must never
/// overwrite an identity the companion lane established for itself.
#[test]
fn seeding_is_first_identity_only_and_never_restamps() {
    let mut e = engine();
    add(&mut e, "c1");
    opt_in(&mut e, "c1", BOARD);
    e.record_tracker_link("c1", ISSUES, "owner/repo#7", "issue-hash", None)
        .expect("issues link");
    seed_links_from(&mut e, ISSUES, BOARD).expect("first seed");
    // The companion writes, so its link now holds a real hash of its own.
    e.record_tracker_link("c1", BOARD, "owner/repo#7", "board-wrote-this", None)
        .expect("board write");

    let second = seed_links_from(&mut e, ISSUES, BOARD).expect("second seed");

    // LOAD-BEARING FIRST: the companion's own record survived the second pass.
    assert_eq!(
        e.tracker_link("c1", BOARD)
            .map(|l| l.our_last_write_hash.clone()),
        Some("board-wrote-this".to_string()),
        "a second seed must not reset a lane that has since written"
    );
    assert!(second.seeded.is_empty());
    assert_eq!(second.already_linked, 1);
    assert!(!second.changed());
}

/// Scope is the companion's OWN opt-in, not the source lane's. A chunk mirrored
/// to Issues but deliberately kept off the board stays off it.
#[test]
fn only_chunks_opted_into_the_companion_are_seeded() {
    let mut e = engine();
    add(&mut e, "on-board");
    add(&mut e, "issues-only");
    opt_in(&mut e, "on-board", BOARD);
    opt_in(&mut e, "issues-only", ISSUES);
    for id in ["on-board", "issues-only"] {
        e.record_tracker_link(id, ISSUES, &format!("owner/repo#{id}"), "h", None)
            .expect("issues link");
    }

    let report = seed_links_from(&mut e, ISSUES, BOARD).expect("seed");

    // LOAD-BEARING FIRST: the excluded chunk gained nothing.
    assert!(
        e.tracker_link("issues-only", BOARD).is_none(),
        "a chunk kept off the board must not be seeded onto it"
    );
    // Paired positive, same pass: the included one did.
    assert_eq!(report.seeded, vec!["on-board".to_string()]);
}

/// The end-to-end proof the seed owes, driven through the REAL
/// projector: after a seed, the companion's push carries the issues lane's
/// identity rather than minting one of its own.
///
/// The double here can create, so "did not create" is the observable that
/// matters — a real board cannot, and would refuse. Without the seed the
/// projector invents an identity (Created); with it, the projector addresses the
/// coordinate the issues lane recorded. Both halves run against the same double
/// in the same test.
#[tokio::test]
async fn the_companion_push_addresses_the_issues_identity_instead_of_minting_one() {
    let mut e = engine();
    add(&mut e, "seeded");
    add(&mut e, "unseeded");
    opt_in(&mut e, "seeded", BOARD);
    opt_in(&mut e, "unseeded", BOARD);
    // Only one of them has an issue to inherit from.
    e.record_tracker_link("seeded", ISSUES, "owner/repo#7", "issue-hash", None)
        .expect("issues link");
    seed_links_from(&mut e, ISSUES, BOARD).expect("seed");

    let board = FakeTracker::new(BOARD);
    let report = project_all(&mut e, &board, None).await.expect("push");

    let outcome = |id: &str| {
        report
            .outcomes
            .iter()
            .find(|(c, _)| c == id)
            .map(|(_, o)| o.clone())
            .expect("outcome recorded")
    };

    // LOAD-BEARING FIRST: the seeded chunk was addressed by the issues lane's
    // coordinate. A `NotFound` on that exact id is the double saying "you asked
    // me about owner/repo#7" — which is the whole point, and is what a real
    // board would resolve rather than reject.
    match outcome("seeded") {
        ProjectionOutcome::Rejected { reason } => assert!(
            reason.contains("owner/repo#7"),
            "the companion must address the seeded coordinate, got: {reason}"
        ),
        ProjectionOutcome::Patched { external_id } => {
            assert_eq!(external_id, "owner/repo#7");
        }
        other => panic!("expected the seeded coordinate to be addressed, got {other:?}"),
    }
    // THE PAIRED NEGATIVE, same double, same run: with nothing to inherit, the
    // projector mints an identity — exactly the behaviour a patch-only lane
    // cannot have, and the reason the seed exists.
    assert!(
        matches!(outcome("unseeded"), ProjectionOutcome::Created { .. }),
        "without a seed there is no identity to address, and the projector creates"
    );
}

/// A lane cannot be seeded from itself. The config refuses it at the door too,
/// but this function is public and the failure it prevents is silent: the source
/// lane's own links would be reset to an unwritten hash, forcing a needless
/// rewrite of every item.
#[test]
fn a_lane_cannot_be_seeded_from_itself() {
    let mut e = engine();
    add(&mut e, "c1");
    opt_in(&mut e, "c1", ISSUES);
    e.record_tracker_link("c1", ISSUES, "owner/repo#7", "issue-hash", None)
        .expect("issues link");

    let refusal = seed_links_from(&mut e, ISSUES, ISSUES).expect_err("must refuse");

    assert!(refusal.contains(ISSUES), "the refusal must name the lane");
    // LOAD-BEARING: the refusal cost nothing — the hash is untouched.
    assert_eq!(
        e.tracker_link("c1", ISSUES)
            .map(|l| l.our_last_write_hash.clone()),
        Some("issue-hash".to_string())
    );
    // Paired positive against the same engine: a DIFFERENT destination works,
    // so the refusal is about the two keys being equal and not about this engine.
    assert!(seed_links_from(&mut e, ISSUES, BOARD).is_ok());
}

/// A companion configured and then quietly ignored is a push that looks like it
/// worked and mirrored half of what was asked. Both unusable shapes are errors,
/// and a usable one is not.
#[test]
fn an_unusable_companion_lane_is_an_error_rather_than_a_silent_absence() {
    let base = TrackerConfig {
        enabled: true,
        provider: Some(ISSUES.to_string()),
        target: Some("owner/repo".to_string()),
        ..TrackerConfig::default()
    };

    // LOAD-BEARING FIRST: a lane naming the primary provider is refused, and the
    // refusal says which key.
    let same_key = TrackerConfig {
        companion: Some(CompanionLane {
            provider: ISSUES.to_string(),
            target: "owner/repo".to_string(),
        }),
        ..base.clone()
    };
    let refusal = companion_lane(&same_key).expect_err("a lane cannot be the primary");
    assert!(refusal.contains(ISSUES));

    let no_target = TrackerConfig {
        companion: Some(CompanionLane {
            provider: BOARD.to_string(),
            target: String::new(),
        }),
        ..base.clone()
    };
    assert!(companion_lane(&no_target).is_err());

    // THE PAIRED POSITIVES: no lane is Ok(None), and a usable lane is Ok(Some) —
    // so the errors above are about these two shapes and not about the function
    // refusing everything.
    assert!(companion_lane(&base).expect("no lane is fine").is_none());
    let usable = TrackerConfig {
        companion: Some(CompanionLane {
            provider: BOARD.to_string(),
            target: "orgs/acme/projects/12".to_string(),
        }),
        ..base
    };
    assert_eq!(
        companion_lane(&usable)
            .expect("usable")
            .map(|l| l.provider.as_str()),
        Some(BOARD)
    );
}
