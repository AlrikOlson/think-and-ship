use think_and_ship::ship::domain::action::ActionType;
use think_and_ship::ship::domain::task::TaskType;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::ship::persistence::{Persistence, PersistenceConfig};

fn tmp_persistence() -> (tempfile::TempDir, Persistence) {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = PersistenceConfig {
        enabled: true,
        data_dir: tmp.path().to_path_buf(),
    };
    let p = Persistence::new(&cfg);
    (tmp, p)
}

#[test]
fn round_trip_objective_and_tasks() {
    let (_tmp, persistence) = tmp_persistence();
    let project_id = "test-proj-abc123".to_string();

    {
        let mut engine =
            ShipEngine::new(project_id.clone()).with_persistence(persistence.clone());
        engine.set_objective(
            "Build auth".into(),
            vec!["JWT works".into()],
            vec![],
            "src/".into(),
        );
        engine.add_task(
            "t1".into(),
            "Implement".into(),
            TaskType::Implement,
            Some("medium".into()),
            Some("alt-1".into()),
        );
        engine.start_task("t1").unwrap();
        engine
            .record_action(
                None,
                ActionType::Code,
                "wrote code".into(),
                vec!["auth.rs".into()],
                vec![],
                "ok".into(),
                Some(5),
            )
            .unwrap();
    }

    let engine2 = ShipEngine::new(project_id).with_persistence(persistence);
    assert!(engine2.objective.is_some());
    assert_eq!(
        engine2.objective.as_ref().unwrap().description,
        "Build auth"
    );
    assert_eq!(engine2.tasks.len(), 1);
    assert_eq!(engine2.tasks[0].actions.len(), 1);
    assert_eq!(engine2.tasks[0].actions[0].deliberate_step, Some(5));
    assert_eq!(engine2.tasks[0].deliberate_branch, Some("alt-1".into()));
}

#[test]
fn persistence_disabled_writes_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = PersistenceConfig {
        enabled: false,
        data_dir: tmp.path().to_path_buf(),
    };
    let persistence = Persistence::new(&cfg);

    let mut engine = ShipEngine::new("test".into()).with_persistence(persistence.clone());
    engine.set_objective("Goal".into(), vec![], vec![], String::new());

    let sessions_dir = tmp.path().join("ship").join("sessions");
    assert!(
        !sessions_dir.exists()
            || std::fs::read_dir(&sessions_dir)
                .map(|d| d.count() == 0)
                .unwrap_or(true),
        "no files should be written when persistence is disabled"
    );
}

#[test]
fn reset_removes_disk_file() {
    let (_tmp, persistence) = tmp_persistence();
    let project_id = "test-reset-abc".to_string();

    let mut engine = ShipEngine::new(project_id.clone()).with_persistence(persistence.clone());
    engine.set_objective("Goal".into(), vec![], vec![], String::new());

    let path = _tmp
        .path()
        .join("ship")
        .join("sessions")
        .join(format!("{project_id}.json"));
    assert!(path.exists(), "file should exist after set_objective");

    engine.reset();
    assert!(!path.exists(), "file should be removed after reset");
}

#[test]
fn action_ids_survive_restart() {
    let (_tmp, persistence) = tmp_persistence();
    let project_id = "test-ids".to_string();

    {
        let mut engine =
            ShipEngine::new(project_id.clone()).with_persistence(persistence.clone());
        engine.set_objective("Goal".into(), vec![], vec![], String::new());
        engine.add_task("t1".into(), "Task".into(), TaskType::Implement, None, None);
        engine.start_task("t1").unwrap();
        engine
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
        engine
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
    }

    let mut engine2 = ShipEngine::new(project_id).with_persistence(persistence);
    let action = engine2
        .record_action(
            Some("t1"),
            ActionType::Code,
            "c".into(),
            vec![],
            vec![],
            "".into(),
            None,
        )
        .unwrap();
    assert_eq!(
        action.id, 3,
        "action id should continue from where it left off"
    );
}
