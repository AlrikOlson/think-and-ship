//! Phase 23b2 e2e: the THINK engine, wired with a `RepoSink`, mirrors each
//! recorded step into the repo's `.think-and-ship/` as Agent Trace JSONL and
//! commits the session on close. Exercises the real engine + real git.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;
use think_and_ship::infra::RepoSink;
use think_and_ship::think::config::DeliberateConfig;
use think_and_ship::think::domain::DeliberateStep;
use think_and_ship::think::engine::core::ReasoningServer;

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

fn quiet_config() -> DeliberateConfig {
    let mut c = DeliberateConfig::default();
    c.display.color_output = false;
    c
}

/// A minimal valid step, built via serde so we don't restate ~20 fields.
fn step(n: u32, final_step: bool) -> DeliberateStep {
    let mut v = json!({
        "step_number": n,
        "estimated_total": 3,
        "purpose": "analysis",
        "context": "ctx",
        "thought": "thought",
        "outcome": "outcome",
        "next_action": "next",
        "rationale": "rationale",
    });
    if final_step {
        v["is_final_step"] = json!(true);
    }
    serde_json::from_value(v).expect("valid step json")
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
fn shared_steps_mirror_as_agent_trace_and_commit_on_close() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    let mut server = ReasoningServer::new(quiet_config()).with_repo_sink(
        RepoSink::new(tmp.path()),
        true, // shared → committed sessions/ partition
    );

    assert!(server.process_step(step(1, false)).is_ok());
    assert!(server.process_step(step(2, false)).is_ok());
    assert!(server.process_step(step(3, true)).is_ok()); // closes the session

    // Exactly one session file with three valid Agent Trace records.
    let sessions = tmp.path().join(".think-and-ship/sessions");
    let files = jsonl_files(&sessions);
    assert_eq!(files.len(), 1, "one session JSONL file: {files:?}");
    let body = std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(body.lines().count(), 3, "one record per step");
    for line in body.lines() {
        let v: Value = serde_json::from_str(line).expect("each line is valid JSON");
        assert_eq!(v["version"], "0.1.0");
        assert_eq!(v["tool"]["name"], "think-and-ship");
        assert_eq!(v["vcs"]["type"], "git");
        let ext = &v["metadata"]["dev.thinkandship"];
        assert_eq!(ext["family"], "think");
        assert_eq!(ext["kind"], "step");
        assert_eq!(ext["shared"], true);
        assert!(ext["record"]["step_number"].is_number());
    }

    // The close (is_final_step on step 3) produced exactly one commit.
    let log = git(tmp.path(), &["log", "--oneline", "--", ".think-and-ship/"]);
    let commits = String::from_utf8_lossy(&log.stdout);
    assert_eq!(
        commits.lines().count(),
        1,
        "one commit per session: {commits}"
    );

    // The committed file is tracked; the partition gitignore excludes local/.
    let tracked = git(tmp.path(), &["ls-files", ".think-and-ship/"]);
    let tracked = String::from_utf8_lossy(&tracked.stdout);
    assert!(
        tracked.contains("sessions/"),
        "session file tracked: {tracked}"
    );
}

#[test]
fn local_default_writes_but_never_commits() {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());

    // shared = false → records land in the gitignored local/ partition.
    let mut server =
        ReasoningServer::new(quiet_config()).with_repo_sink(RepoSink::new(tmp.path()), false);
    server.process_step(step(1, true)).unwrap();

    let local = tmp.path().join(".think-and-ship/local");
    assert_eq!(jsonl_files(&local).len(), 1, "local file written");
    assert!(
        !tmp.path().join(".think-and-ship/sessions").exists()
            || jsonl_files(&tmp.path().join(".think-and-ship/sessions")).is_empty(),
        "nothing in the shared partition"
    );

    // No commits: local is gitignored and commit_session only runs for shared.
    let log = git(tmp.path(), &["log", "--oneline"]);
    let empty = !log.status.success() || String::from_utf8_lossy(&log.stdout).trim().is_empty();
    assert!(empty, "local-only traces must not be committed");
}

#[test]
fn no_sink_preserves_local_behaviour() {
    // Without a sink, the engine works exactly as before and touches no repo.
    let mut server = ReasoningServer::new(quiet_config());
    assert!(server.process_step(step(1, true)).is_ok());
}
