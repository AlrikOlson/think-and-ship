//! `tracker-optin-never-grows`: a plan whose tracker scope was frozen on setup
//! day.
//!
//! Projection is opt-in per chunk. That set was populated once, by
//! `tracker setup`, and nothing in the workflow ever grew it — so every chunk
//! created afterwards was invisible to the tracker while `tracker status`
//! cheerfully reported the same reassuring "46 items included" every day.
//!
//! The fix reverses a documented default (silence), so these tests are written
//! in pairs: each one that proves the reversal WORKS has a sibling proving it
//! did not cost the guarantee the default was protecting. The load-bearing test
//! in this file is not the one where a chunk is included — it is
//! `a_project_that_never_decided_still_gets_silence`.

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::config::{
    ConfigSource, TrackerConfig, enable, inherited_opt_in, load,
};

fn armed_engine(provider: Option<&str>) -> RoadmapEngine {
    RoadmapEngine::new("proj".into()).with_opt_in_inheritance(provider.map(str::to_string))
}

fn add(e: &mut RoadmapEngine, id: &str, status: ChunkStatus) {
    e.add_chunk(
        id.into(),
        format!("Chunk {id}"),
        status,
        10,
        String::new(),
        Vec::new(),
        Vec::new(),
        false,
    )
    .expect("add chunk");
}

fn is_in_scope(e: &RoadmapEngine, id: &str, provider: &str) -> bool {
    e.chunks_opted_in(provider).iter().any(|c| c.id == id)
}

/// THE SYMPTOM, fixed: work created after setup day is in scope without anyone
/// re-consenting to it.
#[test]
fn a_chunk_born_after_setup_is_in_scope() {
    let mut e = armed_engine(Some("linear"));
    add(&mut e, "born-later", ChunkStatus::Pending);
    assert!(is_in_scope(&e, "born-later", "linear"));
}

/// THE GUARANTEE. An engine nobody armed — which is every engine belonging to a
/// project that never decided — still births silent chunks. If this test ever
/// goes red, upgrading the binary can fill a stranger's tracker.
#[test]
fn a_project_that_never_decided_still_gets_silence() {
    let mut e = armed_engine(None);
    add(&mut e, "born-silent", ChunkStatus::Pending);
    assert!(e.chunks_opted_in("linear").is_empty());
    assert!(e.chunks_opted_in("github").is_empty());
    assert_eq!(e.tracker_opt_in("born-silent", "linear"), None);
}

/// The whole reason a bulk top-up verb was declined: a sweep over the existing
/// plan would have minted ~20 issues nobody asked for. Arming inheritance must
/// leave every chunk that already exists exactly as it was.
#[test]
fn arming_inheritance_never_sweeps_the_chunks_that_already_exist() {
    let mut e = armed_engine(None);
    add(&mut e, "old-a", ChunkStatus::Pending);
    add(&mut e, "old-b", ChunkStatus::InProgress);
    assert!(e.chunks_opted_in("linear").is_empty());

    e.set_opt_in_inheritance(Some("linear".into()));
    // Nothing retroactive happened.
    assert!(e.chunks_opted_in("linear").is_empty());

    // And only what is born from here on joins.
    add(&mut e, "new-c", ChunkStatus::Pending);
    let scope: Vec<&str> = e
        .chunks_opted_in("linear")
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(scope, vec!["new-c"]);
}

/// An explicit "stop projecting this" is a decision, and a default must not
/// overrule one. Opting out is recorded rather than deleted precisely so it can
/// win — and re-adding the same id is impossible, so the only way inheritance
/// could reach an excluded chunk is a sweep. There is none.
#[test]
fn an_explicit_exclusion_survives_inheritance() {
    let mut e = armed_engine(Some("linear"));
    add(&mut e, "excluded", ChunkStatus::Pending);
    e.set_tracker_opt_in("excluded", "linear", false)
        .expect("exclude");
    assert!(!is_in_scope(&e, "excluded", "linear"));

    // More births, more mutations — the exclusion is not disturbed.
    add(&mut e, "other", ChunkStatus::Pending);
    assert!(!is_in_scope(&e, "excluded", "linear"));
    assert!(is_in_scope(&e, "other", "linear"));
}

/// A chunk born already finished is history, not work. Mirroring it would bury
/// a tracker in completed items — the same rule `tracker setup`'s bulk include
/// has always applied, now shared rather than duplicated.
#[test]
fn a_chunk_born_done_is_not_work_and_is_not_included() {
    let mut e = armed_engine(Some("linear"));
    add(&mut e, "already-done", ChunkStatus::Done);
    add(&mut e, "already-obsolete", ChunkStatus::Obsoleted);
    add(&mut e, "real-work", ChunkStatus::Backlog);
    let scope: Vec<&str> = e
        .chunks_opted_in("linear")
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(scope, vec!["real-work"]);
}

/// THE READOUT the gap needed. `included` only ever grows, so it could report a
/// frozen scope forever; the drift count is what a human or an agent can
/// actually notice.
#[test]
fn the_drift_is_countable_and_names_only_active_work() {
    let mut e = armed_engine(None);
    add(&mut e, "invisible-a", ChunkStatus::Pending);
    add(&mut e, "invisible-b", ChunkStatus::Blocked);
    add(&mut e, "finished", ChunkStatus::Done);

    let drift: Vec<&str> = e
        .chunks_not_opted_in("linear")
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    // The done chunk is out of scope on purpose, not drifting.
    assert_eq!(drift, vec!["invisible-a", "invisible-b"]);

    e.set_tracker_opt_in("invisible-a", "linear", true)
        .expect("include");
    assert_eq!(e.chunks_not_opted_in("linear").len(), 1);
    assert_eq!(e.chunks_opted_in("linear").len(), 1);
}

/// End to end through the REAL config file, because the decision and the engine
/// are wired together by a composition root and a test of each half separately
/// would not notice the wire being cut.
#[test]
fn an_explicit_tracker_on_is_what_arms_the_engine() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    enable(dir.path(), "proj", "linear", "THI", "2026-07-27T09:00:00Z").expect("enable");

    let armed = inherited_opt_in(&load(dir.path(), "proj")).map(str::to_string);
    let mut e = RoadmapEngine::new("proj".into()).with_opt_in_inheritance(armed);
    assert_eq!(e.opt_in_inheritance(), Some("linear"));

    add(&mut e, "after-setup", ChunkStatus::Pending);
    assert!(is_in_scope(&e, "after-setup", "linear"));
}

/// The sibling of the test above, and the one that matters more: a config file
/// with a provider and a destination but NO human decision behind it arms
/// nothing. This is the shape a migration, a template or a copied data dir
/// would produce.
#[test]
fn a_merely_configured_project_arms_nothing() {
    let cfg = TrackerConfig {
        enabled: true,
        provider: Some("linear".into()),
        target: Some("THI".into()),
        decided_at: None,
        source: ConfigSource::Default,
        ..TrackerConfig::default()
    };
    let armed = inherited_opt_in(&cfg).map(str::to_string);
    assert_eq!(armed, None);

    let mut e = RoadmapEngine::new("proj".into()).with_opt_in_inheritance(armed);
    add(&mut e, "should-be-silent", ChunkStatus::Pending);
    assert!(e.chunks_opted_in("linear").is_empty());
}

/// Provider keys fold to lowercase everywhere else in this seam; an engine
/// armed with `Linear` must not create a scope nothing can read back.
#[test]
fn the_armed_provider_key_is_normalised() {
    let mut e = armed_engine(Some("  LINEAR  "));
    assert_eq!(e.opt_in_inheritance(), Some("linear"));
    add(&mut e, "c", ChunkStatus::Pending);
    assert!(is_in_scope(&e, "c", "linear"));
}
