//! Smoke tests for the broadcast surface: when a Unix socket path is
//! configured, every mutation produces an NDJSON frame; when the
//! socket can't be bound, the server still runs but emits nothing.

use std::path::PathBuf;
use std::time::Duration;

use think_and_ship::think::config::DeliberateConfig;
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::think::domain::{DeliberateStep, NextAction};
use serde_json::Value;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

fn quiet_config_with_broadcast(path: PathBuf) -> DeliberateConfig {
    let mut c = DeliberateConfig::default();
    c.display.color_output = false;
    c.broadcast.path = Some(path);
    c
}

fn step(n: u32) -> DeliberateStep {
    DeliberateStep {
        step_number: n,
        estimated_total: 5,
        purpose: "analysis".into(),
        context: format!("ctx {n}"),
        thought: format!("thought {n}"),
        outcome: format!("outcome {n}"),
        next_action: NextAction::Text(format!("next {n}")),
        rationale: format!("rationale {n}"),
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
    }
}

/// Read up to `n` JSON lines from the socket, giving up after `total_timeout`.
async fn read_frames(stream: UnixStream, n: usize, total_timeout: Duration) -> Vec<Value> {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    let mut out: Vec<Value> = Vec::new();
    let deadline = tokio::time::Instant::now() + total_timeout;
    while out.len() < n {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    out.push(v);
                }
            }
            _ => break,
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_emits_step_appended_and_revised() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("deliberate.sock");

    let mut engine = ReasoningServer::new(quiet_config_with_broadcast(sock_path.clone()));

    // Give the accept loop a moment to spin up before we connect.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let stream = UnixStream::connect(&sock_path).await.expect("connect");

    // Drive two appends — the second revises the first — plus a pin and a
    // clear. That covers four of the six frame variants in one test.
    engine.process_step(step(1)).expect("step 1");

    let mut revision = step(2);
    revision.revises_step = Some(1);
    revision.revision_reason = Some("typo".into());
    engine.process_step(revision).expect("step 2 / revision");

    engine.pin_step(2, true).expect("pin");
    engine.clear_history();

    let frames = read_frames(stream, 5, Duration::from_secs(2)).await;

    // Expected sequence: appended(1), appended(2), revised{1 by 2}, pin(2 true), cleared.
    let types: Vec<&str> = frames
        .iter()
        .filter_map(|v| v.get("type").and_then(Value::as_str))
        .collect();
    assert_eq!(
        types,
        vec![
            "step_appended",
            "step_appended",
            "step_revised",
            "pin_changed",
            "cleared",
        ],
        "frame sequence: got {types:?} from {frames:#?}",
    );

    // Spot-check the payload of the second append and the revision pointer.
    assert_eq!(
        frames[1]["step"]["step_number"].as_u64(),
        Some(2),
        "second append carries step 2",
    );
    assert_eq!(frames[2]["revised_step"].as_u64(), Some(1));
    assert_eq!(frames[2]["by_step"].as_u64(), Some(2));
    assert_eq!(frames[3]["step_number"].as_u64(), Some(2));
    assert_eq!(frames[3]["pinned"].as_bool(), Some(true));

    // Every frame carries the family tag so a shared viewer can interleave
    // think_* and ship_* events on one timeline.
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame["family"].as_str(),
            Some("think"),
            "frame {i} should carry family=\"think\", got {frame:#?}",
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_bind_failure_does_not_block_step() {
    // Bind on top of an existing regular file: not a socket, so the
    // pre-bind cleanup leaves it in place, and bind(2) fails. The server
    // must still construct and accept process_step calls.
    let dir = tempdir().unwrap();
    let path = dir.path().join("not-a-socket");
    std::fs::write(&path, "blocker").unwrap();

    let mut engine = ReasoningServer::new(quiet_config_with_broadcast(path));
    // No panic, no error — the engine just runs unobserved.
    let result = engine.process_step(step(1));
    assert!(
        result.is_ok(),
        "process_step succeeded without a broadcaster"
    );
}
