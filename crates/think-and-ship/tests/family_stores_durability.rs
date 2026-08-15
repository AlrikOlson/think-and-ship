//! Durability of the ship/roadmap/signal stores under concurrent server
//! processes — the family-stores extension of `think_trace_durability.rs`.
//!
//! Residue of the 2026-06-09 think incident: these three engines still did
//! load-once-at-startup + whole-file-overwrite of their per-project
//! `<project_id>.json`, so two live engines in the SAME project could clobber
//! each other's acked mutations. The `*_clobber*` tests reproduce that loss
//! mechanism and MUST fail on the pre-fix unlocked overwrite persistence.

use tempfile::TempDir;

use think_and_ship::infra::{Domain, Persistence, PersistenceConfig};
use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::ship::domain::task::TaskType;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::ship::persistence as ship_persistence;
use think_and_ship::signal::SignalEngine;
use think_and_ship::signal::domain::{SignalKind, SignalStatus};

fn infra_cfg(tmp: &TempDir) -> PersistenceConfig {
    PersistenceConfig::from_env()
        .with_data_dir(tmp.path().to_path_buf())
        .enabled(true)
}

fn roadmap_engine(tmp: &TempDir, project: &str) -> RoadmapEngine {
    RoadmapEngine::new(project.into())
        .with_persistence(Persistence::new(&infra_cfg(tmp), Domain::Roadmap))
}

fn signal_engine(tmp: &TempDir, project: &str) -> SignalEngine {
    SignalEngine::new(project.into())
        .with_persistence(Persistence::new(&infra_cfg(tmp), Domain::Signal))
}

fn ship_engine(tmp: &TempDir, project: &str) -> ShipEngine {
    let cfg = ship_persistence::PersistenceConfig {
        enabled: true,
        data_dir: tmp.path().to_path_buf(),
    };
    ShipEngine::new(project.into()).with_persistence(ship_persistence::Persistence::new(&cfg))
}

fn add_chunk(e: &mut RoadmapEngine, id: &str) {
    e.add_chunk(
        id.into(),
        format!("Chunk {id}"),
        ChunkStatus::Pending,
        10,
        String::new(),
        Vec::new(),
        Vec::new(),
        false,
    )
    .unwrap();
}

// ── roadmap ────────────────────────────────────────────────────────────────

/// THE INCIDENT SHAPE, on the roadmap store: two live engines share one data
/// dir and one project. B adds a chunk (acked + persisted). A — which loaded
/// before B's write and knows nothing of it — then adds its own chunk. A's
/// save must not erase B's acked chunk from disk.
#[test]
fn concurrent_roadmap_writer_does_not_clobber_acked_chunks() {
    let tmp = TempDir::new().unwrap();
    let mut a = roadmap_engine(&tmp, "proj");
    let mut b = roadmap_engine(&tmp, "proj");

    add_chunk(&mut b, "from-b");
    add_chunk(&mut a, "from-a");

    let reloaded = roadmap_engine(&tmp, "proj");
    let ids: Vec<&str> = reloaded
        .roadmap()
        .chunks
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        ids.contains(&"from-b"),
        "chunk from-b was acked to B but a stale concurrent writer erased it; survived: {ids:?}"
    );
    assert!(
        ids.contains(&"from-a"),
        "chunk from-a lost; survived: {ids:?}"
    );
}

/// Per-chunk conflicts resolve by recency: a stale writer that still holds an
/// old copy of chunk X must not roll back the status another process advanced.
#[test]
fn stale_roadmap_writer_does_not_roll_back_chunk_status() {
    let tmp = TempDir::new().unwrap();
    {
        let mut seed = roadmap_engine(&tmp, "proj");
        add_chunk(&mut seed, "x");
    }

    let mut a = roadmap_engine(&tmp, "proj"); // both load x@Pending
    let mut b = roadmap_engine(&tmp, "proj");

    b.set_status("x", ChunkStatus::InProgress).unwrap(); // B advances x
    add_chunk(&mut a, "y"); // A persists with a STALE x@Pending

    let reloaded = roadmap_engine(&tmp, "proj");
    let x = reloaded
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "x")
        .expect("x must survive");
    assert_eq!(
        x.status,
        ChunkStatus::InProgress,
        "stale writer rolled back x's acked status transition"
    );
    assert!(reloaded.roadmap().chunks.iter().any(|c| c.id == "y"));
}

// ── signal ─────────────────────────────────────────────────────────────────

/// Same mechanism on the signal store: a signal captured (and acked) by B must
/// survive a save from stale A.
#[test]
fn concurrent_signal_writer_does_not_clobber_acked_signals() {
    let tmp = TempDir::new().unwrap();
    let mut a = signal_engine(&tmp, "proj");
    let mut b = signal_engine(&tmp, "proj");

    let b_id = b
        .capture(SignalKind::Bug, "b@example.com".into(), "from b".into())
        .id
        .clone();
    let a_id = a
        .capture(SignalKind::Idea, "a@example.com".into(), "from a".into())
        .id
        .clone();

    let reloaded = signal_engine(&tmp, "proj");
    let ids: Vec<&str> = reloaded
        .signals()
        .signals
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert!(
        ids.contains(&b_id.as_str()),
        "signal from B was acked but a stale concurrent writer erased it; survived: {ids:?}"
    );
    assert!(ids.contains(&a_id.as_str()), "signal from A lost: {ids:?}");
}

/// The signal lifecycle is forward-only, so a stale writer must never roll a
/// signal's status backwards on disk.
#[test]
fn stale_signal_writer_does_not_roll_back_lifecycle_progress() {
    let tmp = TempDir::new().unwrap();
    let seeded_id = {
        let mut seed = signal_engine(&tmp, "proj");
        seed.capture(SignalKind::Question, "q@example.com".into(), "seed".into())
            .id
            .clone()
    };

    let mut a = signal_engine(&tmp, "proj"); // both load seeded@New
    let mut b = signal_engine(&tmp, "proj");

    b.set_status(&seeded_id, SignalStatus::Triaged).unwrap(); // B advances
    a.capture(SignalKind::Feedback, "a@example.com".into(), "new".into()); // A persists stale copy

    let reloaded = signal_engine(&tmp, "proj");
    let seeded = reloaded
        .signals()
        .signals
        .iter()
        .find(|s| s.id == seeded_id)
        .expect("seeded signal must survive");
    assert_eq!(
        seeded.status,
        SignalStatus::Triaged,
        "stale writer rolled back an acked lifecycle transition"
    );
    assert_eq!(reloaded.signals().signals.len(), 2);
}

// ── ship ───────────────────────────────────────────────────────────────────

/// Two engines working the SAME cycle (B loaded the objective A persisted):
/// a task acked to either process must survive the other's saves.
#[test]
fn concurrent_ship_writers_union_tasks_within_one_cycle() {
    let tmp = TempDir::new().unwrap();
    let mut a = ship_engine(&tmp, "proj");
    a.set_objective("Shared cycle".into(), vec![], vec![], String::new());

    // B loads the persisted objective — same cycle as A.
    let mut b = ship_engine(&tmp, "proj");
    assert!(b.objective.is_some(), "B must join A's cycle");

    a.add_task(
        "a-task".into(),
        "A's task".into(),
        TaskType::Implement,
        None,
        None,
    );
    // B never saw a-task; its save must not erase it.
    b.add_task(
        "b-task".into(),
        "B's task".into(),
        TaskType::Test,
        None,
        None,
    );

    let reloaded = ship_engine(&tmp, "proj");
    let ids: Vec<&str> = reloaded.tasks.iter().map(|t| t.id.as_str()).collect();
    assert!(
        ids.contains(&"a-task"),
        "task acked to A erased by stale writer B; survived: {ids:?}"
    );
    assert!(ids.contains(&"b-task"), "task from B lost: {ids:?}");
}

/// A stale process still holding a PRIOR cycle must not clobber the new cycle
/// another process started after a reset.
#[test]
fn stale_ship_cycle_does_not_clobber_a_newer_cycle() {
    let tmp = TempDir::new().unwrap();
    let mut a = ship_engine(&tmp, "proj");
    a.set_objective("Old cycle".into(), vec![], vec![], String::new());

    let mut b = ship_engine(&tmp, "proj"); // B holds the old cycle
    assert_eq!(b.objective.as_ref().unwrap().description, "Old cycle");

    a.reset();
    a.set_objective("New cycle".into(), vec![], vec![], String::new());

    // B (stale, old cycle) saves — it must not resurrect the old cycle.
    b.add_task(
        "zombie".into(),
        "stale".into(),
        TaskType::Implement,
        None,
        None,
    );

    let reloaded = ship_engine(&tmp, "proj");
    assert_eq!(
        reloaded.objective.as_ref().map(|o| o.description.as_str()),
        Some("New cycle"),
        "a stale writer resurrected a reset-away cycle"
    );
    assert!(
        reloaded.tasks.iter().all(|t| t.id != "zombie"),
        "a stale cycle's task leaked into the new cycle"
    );
}

/// Reset stays a reset for the resetting process itself: its own next cycle
/// must not re-merge the pre-reset tasks. (Guards the merge-on-save change —
/// this already passes pre-fix and must keep passing.)
#[test]
fn ship_reset_is_not_resurrected_by_later_saves() {
    let tmp = TempDir::new().unwrap();
    let mut a = ship_engine(&tmp, "proj");
    a.set_objective("Cycle 1".into(), vec![], vec![], String::new());
    a.add_task(
        "old".into(),
        "pre-reset".into(),
        TaskType::Implement,
        None,
        None,
    );

    a.reset();
    a.set_objective("Cycle 2".into(), vec![], vec![], String::new());
    a.add_task(
        "new".into(),
        "post-reset".into(),
        TaskType::Implement,
        None,
        None,
    );

    let reloaded = ship_engine(&tmp, "proj");
    assert_eq!(
        reloaded.objective.as_ref().map(|o| o.description.as_str()),
        Some("Cycle 2")
    );
    let ids: Vec<&str> = reloaded.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["new"], "reset-away task resurrected: {ids:?}");
}
