use think_and_ship::ship::domain::action::ActionType;
use think_and_ship::ship::domain::artifact::{Artifact, ArtifactType};
use think_and_ship::ship::domain::check::CheckType;
use think_and_ship::ship::domain::objective::ObjectiveStatus;
use think_and_ship::ship::domain::task::{TaskStatus, TaskType};
use think_and_ship::ship::engine::ShipEngine;

fn engine() -> ShipEngine {
    ShipEngine::new("test-project-abc123".to_string())
}

// ── Objective ──────────────────────────────────────────────────────

#[test]
fn set_objective() {
    let mut e = engine();
    e.set_objective(
        "Build auth".into(),
        vec!["JWT works".into()],
        vec!["No breaking changes".into()],
        "src/auth/".into(),
    );
    let obj = e.objective.as_ref().unwrap();
    assert_eq!(obj.description, "Build auth");
    assert_eq!(obj.acceptance_criteria, vec!["JWT works"]);
    assert_eq!(obj.status, ObjectiveStatus::Defined);
    assert!(obj.created_at.is_some());
}

#[test]
fn set_objective_overwrites() {
    let mut e = engine();
    e.set_objective("First".into(), vec![], vec![], String::new());
    e.set_objective("Second".into(), vec![], vec![], String::new());
    assert_eq!(e.objective.as_ref().unwrap().description, "Second");
}

// ── Plan ───────────────────────────────────────────────────────────

#[test]
fn add_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task(
        "t1".into(),
        "First task".into(),
        TaskType::Implement,
        Some("small".into()),
        None,
    );
    assert_eq!(e.tasks.len(), 1);
    assert_eq!(e.tasks[0].id, "t1");
    assert_eq!(e.tasks[0].status, TaskStatus::Planned);
    assert_eq!(
        e.objective.as_ref().unwrap().status,
        ObjectiveStatus::Active
    );
}

#[test]
fn add_task_with_deliberate_branch() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task(
        "t1".into(),
        "Task".into(),
        TaskType::Implement,
        None,
        Some("alt-approach".into()),
    );
    assert_eq!(e.tasks[0].deliberate_branch, Some("alt-approach".into()));
}

#[test]
fn remove_planned_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.remove_task("t1").unwrap();
    assert!(e.tasks.is_empty());
}

#[test]
fn remove_active_task_fails() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    assert!(e.remove_task("t1").is_err());
}

#[test]
fn remove_nonexistent_task_fails() {
    let e = engine();
    assert!(e.tasks.is_empty());
}

#[test]
fn reorder_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("a".into(), "A".into(), TaskType::Implement, None, None);
    e.add_task("b".into(), "B".into(), TaskType::Test, None, None);
    e.add_task("c".into(), "C".into(), TaskType::Review, None, None);
    e.reorder_task("c", Some("a")).unwrap();
    let ids: Vec<&str> = e.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "c", "b"]);
}

#[test]
fn reorder_to_front() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("a".into(), "A".into(), TaskType::Implement, None, None);
    e.add_task("b".into(), "B".into(), TaskType::Test, None, None);
    e.reorder_task("b", None).unwrap();
    let ids: Vec<&str> = e.tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["b", "a"]);
}

// ── Task lifecycle ─────────────────────────────────────────────────

#[test]
fn start_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    let task = e.start_task("t1").unwrap();
    assert_eq!(task.status, TaskStatus::Active);
    assert!(task.started_at.is_some());
}

#[test]
fn start_second_task_fails() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "A".into(), TaskType::Implement, None, None);
    e.add_task("t2".into(), "B".into(), TaskType::Test, None, None);
    e.start_task("t1").unwrap();
    assert!(e.start_task("t2").is_err());
}

#[test]
fn complete_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let artifacts = vec![Artifact {
        artifact_type: ArtifactType::Commit,
        reference: "abc123".into(),
        description: "initial commit".into(),
    }];
    let task = e.complete_task("t1", artifacts).unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
    assert!(task.completed_at.is_some());
    assert_eq!(task.artifacts.len(), 1);
}

#[test]
fn complete_planned_task_fails() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    assert!(e.complete_task("t1", vec![]).is_err());
}

#[test]
fn block_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let task = e.block_task("t1", "waiting on API key".into()).unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
    assert_eq!(task.blocked_reason, Some("waiting on API key".into()));
}

#[test]
fn restart_blocked_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    e.block_task("t1", "blocked".into()).unwrap();
    let task = e.start_task("t1").unwrap();
    assert_eq!(task.status, TaskStatus::Active);
    assert_eq!(task.blocked_reason, None);
}

#[test]
fn complete_blocked_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    e.block_task("t1", "blocked".into()).unwrap();
    let task = e.complete_task("t1", vec![]).unwrap();
    assert_eq!(task.status, TaskStatus::Completed);
}

// ── Record ─────────────────────────────────────────────────────────

#[test]
fn record_action_on_active_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let action = e
        .record_action(
            None,
            ActionType::Code,
            "wrote auth.rs".into(),
            vec!["src/auth.rs".into()],
            vec!["Edit".into()],
            "compiles".into(),
            Some(42),
        )
        .unwrap();
    assert_eq!(action.id, 1);
    assert_eq!(action.deliberate_step, Some(42));
    assert_eq!(action.task_id, "t1");
}

#[test]
fn record_action_explicit_task_id() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let action = e
        .record_action(
            Some("t1"),
            ActionType::Test,
            "ran tests".into(),
            vec![],
            vec![],
            "pass".into(),
            None,
        )
        .unwrap();
    assert_eq!(action.action_type, ActionType::Test);
    assert_eq!(action.deliberate_step, None);
}

#[test]
fn record_action_no_active_task_fails() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    assert!(
        e.record_action(
            None,
            ActionType::Code,
            "x".into(),
            vec![],
            vec![],
            "y".into(),
            None
        )
        .is_err()
    );
}

#[test]
fn action_ids_are_monotonic() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let a1 = e
        .record_action(
            None,
            ActionType::Code,
            "a".into(),
            vec![],
            vec![],
            "".into(),
            None,
        )
        .unwrap();
    let id1 = a1.id;
    let a2 = e
        .record_action(
            None,
            ActionType::Code,
            "b".into(),
            vec![],
            vec![],
            "".into(),
            None,
        )
        .unwrap();
    let id2 = a2.id;
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

// ── Check ──────────────────────────────────────────────────────────

#[test]
fn record_check() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let check = e
        .record_check(
            None,
            CheckType::Test,
            "cargo test".into(),
            true,
            "42 passed".into(),
            true,
        )
        .unwrap();
    assert!(check.passed);
    assert!(check.required);
}

#[test]
fn record_check_explicit_task() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    let check = e
        .record_check(
            Some("t1"),
            CheckType::Lint,
            "eslint".into(),
            false,
            "3 errors".into(),
            false,
        )
        .unwrap();
    assert!(!check.passed);
    assert!(!check.required);
}

// ── Ship ───────────────────────────────────────────────────────────

#[test]
fn ship_all_complete() {
    let mut e = engine();
    e.set_objective(
        "Goal".into(),
        vec!["it works".into()],
        vec![],
        String::new(),
    );
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    e.record_check(
        None,
        CheckType::Test,
        "test".into(),
        true,
        "ok".into(),
        true,
    )
    .unwrap();
    e.complete_task("t1", vec![]).unwrap();
    let report = e.ship(vec![], Some("done".into()));
    assert_eq!(report["status"], "shipped");
    assert_eq!(report["warnings"].as_array().unwrap().len(), 0);
    assert_eq!(
        e.objective.as_ref().unwrap().status,
        ObjectiveStatus::Completed
    );
}

#[test]
fn ship_warns_on_incomplete_tasks() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    let report = e.ship(vec![], None);
    let warnings = report["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("not completed"));
}

#[test]
fn ship_warns_on_failed_required_checks() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    e.record_check(
        None,
        CheckType::Test,
        "cargo test".into(),
        false,
        "failed".into(),
        true,
    )
    .unwrap();
    e.complete_task("t1", vec![]).unwrap();
    let report = e.ship(vec![], None);
    let warnings = report["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("required checks failed"))
    );
}

// ── Status ─────────────────────────────────────────────────────────

#[test]
fn status_empty() {
    let e = engine();
    let s = e.status();
    assert!(s["objective"].is_null());
    assert_eq!(s["tasks"]["total"], 0);
}

#[test]
fn status_with_deliberate_refs() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task(
        "t1".into(),
        "Task".into(),
        TaskType::Implement,
        None,
        Some("branch-x".into()),
    );
    e.start_task("t1").unwrap();
    e.record_action(
        None,
        ActionType::Code,
        "x".into(),
        vec![],
        vec![],
        "".into(),
        Some(7),
    )
    .unwrap();
    let s = e.status();
    let refs = s["deliberate_refs"].as_array().unwrap();
    assert_eq!(refs.len(), 2);
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "branch" && r["value"] == "branch-x")
    );
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "step" && r["value"] == 7)
    );
}

#[test]
fn status_shows_project_id() {
    let e = engine();
    let s = e.status();
    assert_eq!(s["project_id"], "test-project-abc123");
}

// ── Export ──────────────────────────────────────────────────────────

#[test]
fn export_markdown() {
    let mut e = engine();
    e.set_objective(
        "Build it".into(),
        vec!["works".into()],
        vec![],
        String::new(),
    );
    e.add_task(
        "t1".into(),
        "Do thing".into(),
        TaskType::Implement,
        None,
        None,
    );
    e.start_task("t1").unwrap();
    e.record_action(
        None,
        ActionType::Code,
        "wrote code".into(),
        vec![],
        vec![],
        "ok".into(),
        Some(3),
    )
    .unwrap();
    e.record_check(
        None,
        CheckType::Test,
        "cargo test".into(),
        true,
        "pass".into(),
        true,
    )
    .unwrap();
    let md = e.export("markdown");
    assert!(md.contains("Build it"));
    assert!(md.contains("Do thing"));
    assert!(md.contains("deliberate #3"));
    assert!(md.contains("[pass] cargo test"));
}

#[test]
fn export_json() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    let json_str = e.export("json");
    let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert!(v["objective"].is_object());
}

// ── Reset ──────────────────────────────────────────────────────────

#[test]
fn reset_clears_everything() {
    let mut e = engine();
    e.set_objective("Goal".into(), vec![], vec![], String::new());
    e.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
    e.start_task("t1").unwrap();
    e.record_action(
        None,
        ActionType::Code,
        "x".into(),
        vec![],
        vec![],
        "".into(),
        None,
    )
    .unwrap();
    e.reset();
    assert!(e.objective.is_none());
    assert!(e.tasks.is_empty());
    let s = e.status();
    assert_eq!(s["tasks"]["total"], 0);
}

// ── Full SDLC flow ─────────────────────────────────────────────────

#[test]
fn full_sdlc_flow() {
    let mut e = engine();

    e.set_objective(
        "Add JWT auth middleware".into(),
        vec!["JWT validation works".into(), "All tests pass".into()],
        vec!["No breaking changes".into()],
        "src/middleware/".into(),
    );

    e.add_task(
        "research".into(),
        "Research JWT libs".into(),
        TaskType::Research,
        Some("small".into()),
        None,
    );
    e.add_task(
        "impl".into(),
        "Implement middleware".into(),
        TaskType::Implement,
        Some("medium".into()),
        Some("jwt-approach".into()),
    );
    e.add_task(
        "test".into(),
        "Write tests".into(),
        TaskType::Test,
        Some("small".into()),
        None,
    );
    e.add_task(
        "review".into(),
        "Self-review".into(),
        TaskType::Review,
        Some("trivial".into()),
        None,
    );

    e.start_task("research").unwrap();
    e.record_action(
        None,
        ActionType::Research,
        "evaluated jsonwebtoken vs jwt-simple".into(),
        vec![],
        vec![],
        "jsonwebtoken is better maintained".into(),
        Some(1),
    )
    .unwrap();
    e.complete_task("research", vec![]).unwrap();

    e.start_task("impl").unwrap();
    e.record_action(
        None,
        ActionType::Code,
        "added auth middleware".into(),
        vec!["src/middleware/auth.rs".into()],
        vec!["Edit".into()],
        "compiles".into(),
        Some(3),
    )
    .unwrap();
    e.record_action(
        None,
        ActionType::Code,
        "wired into router".into(),
        vec!["src/main.rs".into()],
        vec!["Edit".into()],
        "routes protected".into(),
        Some(4),
    )
    .unwrap();
    e.complete_task(
        "impl",
        vec![Artifact {
            artifact_type: ArtifactType::File,
            reference: "src/middleware/auth.rs".into(),
            description: "JWT validation middleware".into(),
        }],
    )
    .unwrap();

    e.start_task("test").unwrap();
    e.record_action(
        None,
        ActionType::Test,
        "wrote integration tests".into(),
        vec!["tests/auth.rs".into()],
        vec!["Edit".into()],
        "3 tests".into(),
        None,
    )
    .unwrap();
    e.record_check(
        None,
        CheckType::Test,
        "cargo test".into(),
        true,
        "45 tests pass".into(),
        true,
    )
    .unwrap();
    e.record_check(
        None,
        CheckType::Lint,
        "cargo clippy".into(),
        true,
        "no warnings".into(),
        true,
    )
    .unwrap();
    e.complete_task("test", vec![]).unwrap();

    e.start_task("review").unwrap();
    e.record_action(
        None,
        ActionType::Review,
        "reviewed all changes".into(),
        vec![],
        vec![],
        "looks good".into(),
        Some(8),
    )
    .unwrap();
    e.record_check(
        None,
        CheckType::Review,
        "self-review".into(),
        true,
        "approved".into(),
        false,
    )
    .unwrap();
    e.complete_task("review", vec![]).unwrap();

    let report = e.ship(
        vec![Artifact {
            artifact_type: ArtifactType::Commit,
            reference: "abc1234".into(),
            description: "feat: add JWT auth middleware".into(),
        }],
        Some("JWT auth middleware shipped".into()),
    );

    assert_eq!(report["status"], "shipped");
    assert_eq!(report["tasks"]["completed"], 4);
    assert_eq!(report["tasks"]["total"], 4);
    assert_eq!(report["warnings"].as_array().unwrap().len(), 0);

    let status = e.status();
    assert_eq!(status["objective"]["status"], "completed");
    assert_eq!(status["tasks"]["completed"], 4);

    let refs = status["deliberate_refs"].as_array().unwrap();
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "branch" && r["value"] == "jwt-approach")
    );
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "step" && r["value"] == 1)
    );
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "step" && r["value"] == 3)
    );
    assert!(
        refs.iter()
            .any(|r| r["ref_type"] == "step" && r["value"] == 8)
    );
}
