//! Phase 23b3 e2e: the SHIP engine, wired with a `RepoSink`, mirrors each
//! mutation into the repo's `.think-and-ship/` as Agent Trace JSONL and commits
//! the session on `ship` (objective shipped). Exercises the real engine + git.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use think_and_ship::infra::RepoSink;
use think_and_ship::ship::domain::action::ActionType;
use think_and_ship::ship::domain::check::CheckType;
use think_and_ship::ship::domain::task::TaskType;
use think_and_ship::ship::engine::ShipEngine;

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap()
}

fn init_repo(repo: &Path) {
    assert!(git(repo, &["init", "-q"]).status.success());
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn jsonl_files(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn ship_lifecycle_mirrors_records_and_commits_on_ship() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    let mut engine = ShipEngine::new("proj-abc".to_string())
        .with_repo_sink(RepoSink::new(tmp.path()), true /* shared */);

    // Full lifecycle: objective → plan → start → record(code) → check → ship.
    engine.set_objective(
        "Build it".into(),
        vec!["works".into()],
        vec![],
        "src/".into(),
    );
    engine.add_task(
        "impl".into(),
        "Implement".into(),
        TaskType::Implement,
        None,
        None,
    );
    engine.start_task("impl").unwrap();
    engine
        .record_action(
            Some("impl"),
            ActionType::Code,
            "edit lib.rs".into(),
            vec!["src/lib.rs".into()],
            vec!["Edit".into()],
            "done".into(),
            None,
        )
        .unwrap();
    engine
        .record_check(
            Some("impl"),
            CheckType::Test,
            "cargo test".into(),
            true,
            "all pass".into(),
            true,
        )
        .unwrap();
    engine.ship(vec![], Some("done".into())); // closes the session

    // One session file with the full set of kinds, in order.
    let sessions = tmp.path().join(".think-and-ship/sessions");
    let files = jsonl_files(&sessions);
    assert_eq!(files.len(), 1, "one ship session file: {files:?}");
    let body = std::fs::read_to_string(&files[0]).unwrap();
    let recs: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let kinds: Vec<&str> = recs
        .iter()
        .map(|r| r["metadata"]["dev.thinkandship"]["kind"].as_str().unwrap())
        .collect();
    // objective (set), task (added), task (started), action, check, objective (shipped)
    assert_eq!(
        kinds,
        vec!["objective", "task", "task", "action", "check", "objective"]
    );

    for r in &recs {
        assert_eq!(r["version"], "0.1.0");
        assert_eq!(r["metadata"]["dev.thinkandship"]["family"], "ship");
        assert_eq!(r["tool"]["name"], "think-and-ship");
    }

    // The code action carries Agent Trace files[] attribution.
    let action = recs
        .iter()
        .find(|r| r["metadata"]["dev.thinkandship"]["kind"] == "action")
        .unwrap();
    assert_eq!(action["files"][0]["path"], "src/lib.rs");
    assert_eq!(
        action["files"][0]["conversations"][0]["contributor"]["type"],
        "ai"
    );

    // Exactly one commit, produced on ship().
    let log = git(tmp.path(), &["log", "--oneline", "--", ".think-and-ship/"]);
    let commits = String::from_utf8_lossy(&log.stdout);
    assert_eq!(
        commits.lines().count(),
        1,
        "one commit per session: {commits}"
    );
}

#[test]
fn ship_local_default_writes_but_never_commits() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    let mut engine =
        ShipEngine::new("proj-abc".to_string()).with_repo_sink(RepoSink::new(tmp.path()), false);
    engine.set_objective("g".into(), vec![], vec![], String::new());
    engine.ship(vec![], None);

    let local = tmp.path().join(".think-and-ship/local");
    assert_eq!(jsonl_files(&local).len(), 1, "local file written");
    let log = git(tmp.path(), &["log", "--oneline"]);
    let empty = !log.status.success() || String::from_utf8_lossy(&log.stdout).trim().is_empty();
    assert!(empty, "local-only ship traces must not be committed");
}

#[test]
fn ship_no_sink_preserves_behaviour() {
    let mut engine = ShipEngine::new("proj-abc".to_string());
    engine.set_objective("g".into(), vec![], vec![], String::new());
    engine.add_task("t".into(), "T".into(), TaskType::Implement, None, None);
    // No panic, no repo touched — works exactly as before.
    let report = engine.ship(vec![], None);
    assert!(report.is_object());
}
