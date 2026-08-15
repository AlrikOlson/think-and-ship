//! E2e: the ROADMAP engine, wired with a `RepoSink`, mirrors each
//! mutation into the repo's `.think-and-ship/` as roadmap-family Agent Trace
//! JSONL and commits the session on chunk completion/obsoletion. Exercises the
//! real engine + git.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use think_and_ship::infra::RepoSink;
use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::roadmap::domain::ChunkStatus;

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

fn add(e: &mut RoadmapEngine, id: &str) {
    e.add_chunk(
        id.into(),
        format!("Chunk {id}"),
        ChunkStatus::Pending,
        1,
        String::new(),
        vec![],
        vec![],
        false,
    )
    .unwrap();
}

#[test]
fn roadmap_lifecycle_mirrors_records_and_commits_on_complete() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    let mut engine = RoadmapEngine::new("proj-abc".to_string())
        .with_repo_sink(RepoSink::new(tmp.path()), true /* shared */);

    // add → start → record_refresh → complete (the terminal/commit event).
    add(&mut engine, "phase-1");
    engine.start_chunk("phase-1").unwrap();
    engine.record_refresh("revisited".into(), vec![8]);
    engine
        .complete_chunk("phase-1", Some("task:build-it"))
        .unwrap();
    // The mirror is asynchronous; wait for the worker to drain before asserting.
    engine.flush_mirror();

    let sessions = tmp.path().join(".think-and-ship/sessions");
    let files = jsonl_files(&sessions);
    assert_eq!(files.len(), 1, "one roadmap session file: {files:?}");
    let body = std::fs::read_to_string(&files[0]).unwrap();
    let recs: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // Every record is a valid roadmap-family Agent Trace envelope.
    for r in &recs {
        assert_eq!(r["version"], "0.1.0");
        assert_eq!(r["metadata"]["dev.thinkandship"]["family"], "roadmap");
        assert_eq!(r["tool"]["name"], "think-and-ship");
    }

    let kinds: Vec<&str> = recs
        .iter()
        .map(|r| r["metadata"]["dev.thinkandship"]["kind"].as_str().unwrap())
        .collect();
    // add(chunk) start(chunk) refresh complete[changed+completed]→ chunk,chunk,refresh,chunk,chunk
    assert!(
        kinds.contains(&"refresh"),
        "refresh record present: {kinds:?}"
    );
    assert!(
        kinds.iter().filter(|k| **k == "chunk").count() >= 3,
        "chunk records present: {kinds:?}"
    );

    // Exactly one commit, produced on complete_chunk.
    let log = git(tmp.path(), &["log", "--oneline", "--", ".think-and-ship/"]);
    let commits = String::from_utf8_lossy(&log.stdout);
    assert_eq!(
        commits.lines().count(),
        1,
        "one commit on chunk completion: {commits}"
    );
}

#[test]
fn roadmap_local_default_writes_but_never_commits() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    let mut engine =
        RoadmapEngine::new("proj-abc".to_string()).with_repo_sink(RepoSink::new(tmp.path()), false);
    add(&mut engine, "phase-1");
    engine.start_chunk("phase-1").unwrap();
    engine.complete_chunk("phase-1", None).unwrap();
    engine.flush_mirror();

    let local = tmp.path().join(".think-and-ship/local");
    assert_eq!(jsonl_files(&local).len(), 1, "local file written");
    let log = git(tmp.path(), &["log", "--oneline"]);
    let empty = !log.status.success() || String::from_utf8_lossy(&log.stdout).trim().is_empty();
    assert!(empty, "local-only roadmap traces must not be committed");
}

#[test]
fn roadmap_no_sink_preserves_behaviour() {
    let mut engine = RoadmapEngine::new("proj-abc".to_string());
    add(&mut engine, "phase-1");
    // No panic, no repo touched — works exactly as before.
    let status = engine.status();
    assert_eq!(status["counts"]["total"], 1);
}
