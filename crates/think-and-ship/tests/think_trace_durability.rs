//! Durability of the think trace under concurrent server processes and
//! restarts — regression suite for the 2026-06-09 incident where steps
//! recorded (and acked) by one server process were clobbered from disk by a
//! second, longer-lived process holding a stale in-memory history.
//!
//! The two `*_clobber*` / `*_outlives_trim*` tests reproduce the loss
//! mechanism and MUST fail on the pre-fix whole-file-overwrite persistence.

use std::collections::HashSet;
use std::path::Path;

use tempfile::TempDir;
use think_and_ship::infra::project_id_for_path;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::domain::{NextAction, ThinkHistory, ThinkStep};
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::think::persistence::Persistence;

fn quiet_config() -> ThinkConfig {
    let mut c = ThinkConfig::default();
    c.display.color_output = false;
    c
}

fn persisting_config(tmp: &TempDir) -> ThinkConfig {
    let mut c = quiet_config();
    c.persistence.enabled = true;
    c.persistence.data_dir = tmp.path().to_path_buf();
    c
}

fn step_n(n: u32) -> ThinkStep {
    ThinkStep {
        step_number: n,
        estimated_total: 100,
        purpose: format!("step {n}"),
        context: "durability test".into(),
        thought: format!("thought {n}"),
        outcome: format!("outcome {n}"),
        next_action: NextAction::Text("next".into()),
        rationale: "rationale".into(),
        confidence: None,
        uncertainty_notes: None,
        revises_step: None,
        revision_reason: None,
        revised_by: None,
        is_final_step: None,
        branch_from: None,
        branch_id: None,
        branch_name: None,
        tools_used: None,
        dependencies: None,
        timestamp: None,
        duration_ms: None,
        session_id: None,
        pinned: None,
        cwd: None,
        execution_ref: None,
        record_id: None,
    }
}

fn persisted_step_numbers(tmp: &TempDir) -> HashSet<u32> {
    // A fresh engine on the same data dir is "a restart": whatever it can see
    // is what actually survived on disk.
    let s = ReasoningServer::new(persisting_config(tmp));
    s.history().steps.iter().map(|st| st.step_number).collect()
}

/// THE INCIDENT, reduced: two live server processes share one data dir (and
/// one project). B records a step (acked + persisted). A — which loaded
/// before B's write and knows nothing of it — then records its own step.
/// A's save must not erase B's acked step from disk.
#[test]
fn concurrent_writer_does_not_clobber_acked_steps() {
    let tmp = TempDir::new().unwrap();

    let mut a = ReasoningServer::new(persisting_config(&tmp)); // loads (empty) state
    let mut b = ReasoningServer::new(persisting_config(&tmp)); // loads (empty) state

    let _ = b.process_step(step_n(1)); // B: record + persist step 1
    let _ = a.process_step(step_n(2)); // A: stale memory, record + persist step 2

    let survived = persisted_step_numbers(&tmp);
    assert!(
        survived.contains(&1),
        "step 1 was acked to B but a stale concurrent writer erased it from disk; survived: {survived:?}"
    );
    assert!(survived.contains(&2), "step 2 lost; survived: {survived:?}");
}

/// Same mechanism, interleaved both ways: every acked step from either
/// process must survive.
#[test]
fn interleaved_writers_union_on_disk() {
    let tmp = TempDir::new().unwrap();

    let mut a = ReasoningServer::new(persisting_config(&tmp));
    let mut b = ReasoningServer::new(persisting_config(&tmp));

    let _ = a.process_step(step_n(1));
    let _ = b.process_step(step_n(2));
    let _ = a.process_step(step_n(3));
    let _ = b.process_step(step_n(4));

    let survived = persisted_step_numbers(&tmp);
    for n in 1..=4 {
        assert!(
            survived.contains(&n),
            "step {n} lost; survived: {survived:?}"
        );
    }
}

/// The in-memory window is capped at `max_history_size`, but the DISK copy is
/// the durable record: steps that fall out of the window must still be on
/// disk after a restart (pre-fix, every save truncated the file to the
/// window).
#[test]
fn disk_archive_outlives_the_in_memory_trim_window() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = persisting_config(&tmp);
    cfg.system.max_history_size = 3;

    let mut s = ReasoningServer::new(cfg);
    for n in 1..=5 {
        let _ = s.process_step(step_n(n));
    }
    assert_eq!(s.history().steps.len(), 3, "memory window should be capped");

    let survived = persisted_step_numbers(&tmp);
    for n in 1..=5 {
        assert!(
            survived.contains(&n),
            "step {n} fell out of the memory window AND off disk; survived: {survived:?}"
        );
    }
}

/// Restart durability (should already hold): record, drop, reload.
#[test]
fn steps_survive_a_process_restart() {
    let tmp = TempDir::new().unwrap();
    {
        let mut s = ReasoningServer::new(persisting_config(&tmp));
        let _ = s.process_step(step_n(1));
        let _ = s.process_step(step_n(2));
    }
    let survived = persisted_step_numbers(&tmp);
    assert!(survived.contains(&1) && survived.contains(&2));
}

/// Different projects sharing one data dir write different default files and
/// never see (or disturb) each other's steps — the cross-project half of the
/// incident (the surviving trace mixed tessera/kitbash/think-and-ship steps).
#[test]
fn projects_are_isolated_on_one_data_dir() {
    let tmp = TempDir::new().unwrap();
    let p1 = "proj-one-aaaaaa".to_string();
    let p2 = "proj-two-bbbbbb".to_string();

    let mut e1 = ReasoningServer::new_for_project(persisting_config(&tmp), p1.clone());
    let mut e2 = ReasoningServer::new_for_project(persisting_config(&tmp), p2.clone());
    let _ = e1.process_step(step_n(1));
    let _ = e2.process_step(step_n(1)); // same number in another project is fine

    let sessions = tmp.path().join("think").join("sessions");
    assert!(sessions.join(format!("{p1}.json")).exists());
    assert!(sessions.join(format!("{p2}.json")).exists());

    let r1 = ReasoningServer::new_for_project(persisting_config(&tmp), p1);
    assert_eq!(
        r1.history().steps.len(),
        1,
        "project one must reload exactly its own step"
    );
    assert_eq!(r1.history().steps[0].thought, "thought 1");
}

/// First project-scoped load adopts this project's steps (attributed by the
/// cwd each step recorded) out of the legacy global `_default.json`, and
/// leaves the legacy file in place for other projects' migrations.
#[test]
fn legacy_global_default_migrates_steps_by_cwd() {
    let tmp = TempDir::new().unwrap();
    let cfg = persisting_config(&tmp);

    let mine_path = "/tmp/durability-mine";
    let mine = project_id_for_path(Path::new(mine_path));

    let mut s1 = step_n(1);
    s1.cwd = Some(mine_path.to_string());
    let mut s2 = step_n(2);
    s2.cwd = Some("/tmp/durability-other".to_string());
    let s3 = step_n(3); // no cwd — not attributable, stays legacy-only

    let legacy_history = ThinkHistory {
        steps: vec![s1, s2, s3],
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    // A legacy (un-scoped) handle writes the global _default.json.
    Persistence::new(&cfg.persistence).save_default(&legacy_history);

    let engine = ReasoningServer::new_for_project(persisting_config(&tmp), mine.clone());
    let nums: HashSet<u32> = engine
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert!(nums.contains(&1), "this project's step must be adopted");
    assert!(
        !nums.contains(&2) && !nums.contains(&3),
        "other/unattributable steps must not be adopted: {nums:?}"
    );

    let sessions = tmp.path().join("think").join("sessions");
    assert!(
        sessions.join("_default.json").exists(),
        "legacy file must stay for other projects' migrations"
    );
    assert!(sessions.join(format!("{mine}.json")).exists());
}

fn history_with(steps: Vec<ThinkStep>) -> ThinkHistory {
    ThinkHistory {
        steps,
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    }
}

/// ERRATA (found by post-deploy verification): a May-era bare-project session
/// file can already exist at exactly `<project_id>.json` — its presence must
/// NOT shadow the adoption of this project's steps from the legacy
/// `_default.json`. Adoption is keyed on a persisted marker, not on the
/// project file's existence.
#[test]
fn preexisting_project_file_does_not_shadow_legacy_adoption() {
    let tmp = TempDir::new().unwrap();
    let cfg = persisting_config(&tmp);
    let mine_path = "/tmp/durability-mine";
    let mine = project_id_for_path(Path::new(mine_path));

    // A stale bare-project session file already occupies <mine>.json (step 5).
    let mut old = step_n(5);
    old.cwd = Some(mine_path.to_string());
    Persistence::for_project(&cfg.persistence, &mine).save_default(&history_with(vec![old]));

    // The legacy global file holds this project's newer steps (10, 11).
    let mut s10 = step_n(10);
    s10.cwd = Some(mine_path.to_string());
    let mut s11 = step_n(11);
    s11.cwd = Some(mine_path.to_string());
    Persistence::new(&cfg.persistence).save_default(&history_with(vec![s10, s11]));

    let engine = ReasoningServer::new_for_project(persisting_config(&tmp), mine);
    let nums: HashSet<u32> = engine
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert!(nums.contains(&5), "stale project-file step kept: {nums:?}");
    assert!(
        nums.contains(&10) && nums.contains(&11),
        "legacy steps must be adopted despite the pre-existing project file: {nums:?}"
    );
}

/// The legacy adoption runs exactly once: after the marker is persisted,
/// later changes to the legacy file are not re-adopted.
#[test]
fn legacy_adoption_is_one_time() {
    let tmp = TempDir::new().unwrap();
    let cfg = persisting_config(&tmp);
    let mine_path = "/tmp/durability-mine";
    let mine = project_id_for_path(Path::new(mine_path));

    let mut s1 = step_n(1);
    s1.cwd = Some(mine_path.to_string());
    Persistence::new(&cfg.persistence).save_default(&history_with(vec![s1.clone()]));

    // First load adopts step 1 (and persists the marker).
    let first = ReasoningServer::new_for_project(persisting_config(&tmp), mine.clone());
    assert_eq!(first.history().steps.len(), 1);
    drop(first);

    // The legacy file later gains step 99 for this project (e.g. a stale
    // old-binary writer) — it must NOT be re-adopted.
    let mut s99 = step_n(99);
    s99.cwd = Some(mine_path.to_string());
    Persistence::new(&cfg.persistence).save_default(&history_with(vec![s1, s99]));

    let second = ReasoningServer::new_for_project(persisting_config(&tmp), mine);
    let nums: HashSet<u32> = second
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert!(nums.contains(&1));
    assert!(
        !nums.contains(&99),
        "adoption must be one-time (marker), got: {nums:?}"
    );
}

/// Wipe followed by a RESTART must stay a wipe: the legacy file (left in
/// place for other projects) must not be re-adopted into the wiped project.
#[test]
fn wipe_survives_a_restart_without_legacy_resurrection() {
    let tmp = TempDir::new().unwrap();
    let cfg = persisting_config(&tmp);
    let mine_path = "/tmp/durability-mine";
    let mine = project_id_for_path(Path::new(mine_path));

    let mut s1 = step_n(1);
    s1.cwd = Some(mine_path.to_string());
    Persistence::new(&cfg.persistence).save_default(&history_with(vec![s1]));

    let mut engine = ReasoningServer::new_for_project(persisting_config(&tmp), mine.clone());
    assert_eq!(engine.history().steps.len(), 1, "adopted before wipe");
    engine.clear_history();
    drop(engine);

    let reloaded = ReasoningServer::new_for_project(persisting_config(&tmp), mine);
    assert!(
        reloaded.history().steps.is_empty(),
        "wipe + restart resurrected legacy steps: {:?}",
        reloaded
            .history()
            .steps
            .iter()
            .map(|s| s.step_number)
            .collect::<Vec<_>>()
    );
}

/// An explicit wipe must stay a wipe — the merge-on-save discipline must not
/// resurrect deleted steps from a pre-wipe file.
#[test]
fn wipe_is_not_resurrected_by_later_saves() {
    let tmp = TempDir::new().unwrap();
    let mut s = ReasoningServer::new(persisting_config(&tmp));
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));

    s.clear_history();
    let _ = s.process_step(step_n(10));

    let survived = persisted_step_numbers(&tmp);
    assert!(
        !survived.contains(&1) && !survived.contains(&2),
        "wiped steps resurrected: {survived:?}"
    );
    assert!(survived.contains(&10));
}

/// A file written under a schema this build does not know is not interpreted —
/// neither on read nor by the merge a later save performs. The refusal used to
/// live in the read the save did for itself; it now lives in the merge policy
/// the shared lock calls, so it needs its own proof that a future-schema file
/// cannot leak steps into a trace through the back door.
#[test]
fn a_future_schema_file_is_neither_read_nor_merged() {
    let tmp = TempDir::new().unwrap();
    let project = "schema-probe-project".to_string();

    let sessions = tmp.path().join("think").join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join(format!("{project}.json"));
    let future = serde_json::json!({
        "schema_version": 999,
        "history": history_with(vec![step_n(7)]),
    });
    std::fs::write(&path, serde_json::to_string(&future).unwrap()).unwrap();

    let mut engine = ReasoningServer::new_for_project(persisting_config(&tmp), project.clone());
    assert!(
        engine.history().steps.is_empty(),
        "a future-schema file must not be read into the trace"
    );

    let _ = engine.process_step(step_n(1));
    drop(engine);

    let reloaded = ReasoningServer::new_for_project(persisting_config(&tmp), project);
    let nums: HashSet<u32> = reloaded
        .history()
        .steps
        .iter()
        .map(|s| s.step_number)
        .collect();
    assert!(nums.contains(&1), "the real step must persist: {nums:?}");
    assert!(
        !nums.contains(&7),
        "a save merged a step out of a file written under an unknown schema: {nums:?}"
    );
}
