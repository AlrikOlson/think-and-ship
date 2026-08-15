//! End-to-end proof that a long gate stops holding the tool call open — and
//! that nothing changes for a client that never heard of tasks (roadmap chunk
//! `mcp-tasks-long-gates`, SEP-2663).
//!
//! Every test drives a REAL rmcp client over an in-memory duplex, cloning the
//! harness in `progress_notifications.rs`. Unlike `elicitation_consent.rs`, the
//! initiating side here is the CLIENT: it calls `tools/call`, receives a handle,
//! and polls `tasks/get`. So the peer under test is `Peer<RoleClient>` and the
//! ordinary `client.serve()` handle is enough — but the server is still spawned
//! rather than awaited, because `serve` completes the initialize handshake and
//! awaiting it before the client exists deadlocks with no output.
//!
//! # The claims, one test each
//!
//! 1. a tasks-declaring client gets a TASK back from a gate,
//! 2. a client that declared nothing gets the ordinary blocking result,
//! 3. the task's terminal result is the SAME envelope the inline call produces,
//! 4. a FAILING command still reports failure — `verified` derives from the real
//!    exit code, not from the task having completed,
//! 5. `tasks/cancel` settles the task terminal `cancelled`,
//! 6. and actually KILLS the child process,
//! 7. a cancelled gate records no check — neither passed nor silently present,
//! 8. every other tool stays inline even for a tasks-declaring client,
//! 9. the SERVER declares the extension, without which the handle in claim 1
//!    would be unpollable,
//! 10. a task-backed gate's ticks NAME the task — provable only if the heartbeat
//!     is the one started inside the spawned future,
//! 11. and there is exactly ONE ticker, which is the failure mode
//!     `mcp/progress.rs` singled out: a second emitter would double every
//!     notification.
//!
//! # Why each test drives set_objective → plan → start first
//!
//! `ship_check` needs an active task to attach to. Skipping that setup would
//! record only the error path, and every claim about `verified`/`exit_code`
//! would be a claim about an error envelope instead of a real gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities, ClientInfo,
        GetTaskParams, JsonObject, ProgressNotificationParam, TaskPayload, TaskStatus,
    },
    service::NotificationContext,
};
use serde_json::{Value, json};
use think_and_ship::mcp::UnifiedService;
use think_and_ship::mcp::progress::tick_message;
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// A client that declares tasks (or not) and remembers every progress frame.
///
/// The progress half exists for claims 10 and 11: the one thing `progress.rs`
/// forbade is a SECOND emitter, and only a count of what actually crossed the
/// wire can catch that.
#[derive(Clone)]
struct TaskClient {
    declares: bool,
    seen: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl TaskClient {
    fn new(declares: bool) -> Self {
        Self {
            declares,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ClientHandler for TaskClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        if self.declares {
            info.capabilities = ClientCapabilities::builder().enable_tasks().build();
        }
        info
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.seen.lock().unwrap().push(params);
    }
}

fn build_unified(project: &str) -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new(project.into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new(project.into()));
    let signal = SignalService::new(SignalEngine::new(project.into()));
    UnifiedService::new(think, ship, roadmap, signal)
}

type Client = rmcp::service::RunningService<RoleClient, TaskClient>;

/// Both halves must start concurrently: `serve` completes the initialize
/// handshake, so awaiting the server's before the client exists deadlocks.
async fn session(project: &str, declares_tasks: bool) -> Client {
    session_recording(project, declares_tasks).await.0
}

/// The same session, keeping a handle on every progress frame the server sent.
async fn session_recording(
    project: &str,
    declares_tasks: bool,
) -> (Client, Arc<Mutex<Vec<ProgressNotificationParam>>>) {
    let server = build_unified(project);
    let (server_tx, client_tx) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });
    let client = TaskClient::new(declares_tasks);
    let seen = client.seen.clone();
    (
        client.serve(client_tx).await.expect("client.serve failed"),
        seen,
    )
}

fn args(value: Value) -> JsonObject {
    value.as_object().expect("object literal").clone()
}

/// One `tools/call`, raw — `call_tool_once` is the only client entry point that
/// surfaces the `CallToolResponse` variant instead of collapsing it.
async fn call(client: &Client, tool: &str, arguments: Value) -> CallToolResponse {
    let mut params = CallToolRequestParams::new(tool.to_string());
    params.arguments = Some(args(arguments));
    client
        .peer()
        .call_tool_once(params)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

/// `ship_check` attaches to an ACTIVE task, so a bare check would only ever
/// exercise the error path. Drives the real tools, not the engine directly.
async fn arm_an_active_task(client: &Client) {
    call(
        client,
        "ship_set_objective",
        json!({"description": "prove the gate seam"}),
    )
    .await;
    call(
        client,
        "ship_plan",
        json!({"action": "add", "task_id": "gate", "title": "run a gate", "task_type": "test"}),
    )
    .await;
    call(client, "ship_start", json!({"task_id": "gate"})).await;
}

fn task_id_of(response: &CallToolResponse) -> String {
    match response {
        CallToolResponse::Task(created) => created.task.task_id.clone(),
        other => panic!("expected a task handle, got {other:?}"),
    }
}

/// The `structured_content` of a completed call.
///
/// A success carries the payload at the top level (`infra::tool_result::ok`);
/// only a soft error wraps it as `{ok:false, …}`. So "did it work?" is
/// `body["ok"] != false` — NEVER `is_error`, which this server deliberately
/// leaves `Some(false)` on every result it produces, failures included.
fn complete_body(response: &CallToolResponse) -> Value {
    match response {
        CallToolResponse::Complete(result) => serde_json::to_value(
            result
                .structured_content
                .clone()
                .expect("a structured envelope"),
        )
        .unwrap(),
        other => panic!("expected a completed result, got {other:?}"),
    }
}

fn assert_not_a_soft_error(body: &Value) {
    assert_ne!(
        body["ok"],
        json!(false),
        "the call reported a logical failure: {body}"
    );
}

/// Poll `tasks/get` until the task settles, or give up.
async fn await_terminal(client: &Client, task_id: &str, budget: Duration) -> TaskPayload {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let got = client
            .peer()
            .get_task(GetTaskParams::new(task_id.to_string()))
            .await
            .expect("tasks/get");
        match got.task.payload {
            TaskPayload::Working | TaskPayload::InputRequired { .. } => {}
            terminal => return terminal,
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {task_id} never settled within {budget:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A completed task carries the WHOLE `CallToolResult` as its `result` — this
/// digs out the same `structured_content` [`complete_body`] returns, so the two
/// paths are compared field-for-field rather than shape-to-shape.
fn completed_envelope(payload: &TaskPayload) -> Value {
    match payload {
        TaskPayload::Completed { result } => {
            serde_json::to_value(result).unwrap()["structuredContent"].clone()
        }
        other => panic!("expected a completed task, got {other:?}"),
    }
}

/// Claim 1: the gate no longer holds the call open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gate_returns_a_task_handle_to_a_client_that_declared_tasks() {
    let client = session("tasks-handle", true).await;
    arm_an_active_task(&client).await;

    let response = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "quick", "command": "true"}),
    )
    .await;

    assert!(
        matches!(response, CallToolResponse::Task(_)),
        "a gate with a command must come back as a task handle, got {response:?}"
    );
    let _ = client.cancel().await;
}

/// Claim 2: additive, not a migration. A client that declared nothing must see
/// byte-identical blocking behaviour — this is the half that would break every
/// existing caller if the capability check were dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_declared_nothing_still_gets_the_blocking_result() {
    let client = session("tasks-inline", false).await;
    arm_an_active_task(&client).await;

    let response = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "quick", "command": "true"}),
    )
    .await;

    let body = complete_body(&response);
    assert_not_a_soft_error(&body);
    assert_eq!(body["verified"], json!(true), "envelope: {body}");
    assert_eq!(body["exit_code"], json!(0));
    assert_eq!(body["passed"], json!(true));
    let _ = client.cancel().await;
}

/// Claim 3: THE criterion. The task's terminal result and the inline result are
/// the same envelope, because there is only one implementation of a check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tasks_terminal_result_is_the_same_envelope_as_the_inline_one() {
    let command = "exit 0";

    let inline = session("tasks-parity-inline", false).await;
    arm_an_active_task(&inline).await;
    let inline_body = complete_body(
        &call(
            &inline,
            "ship_check",
            json!({"type": "test", "name": "parity", "command": command}),
        )
        .await,
    );

    let tasked = session("tasks-parity-task", true).await;
    arm_an_active_task(&tasked).await;
    let handle = call(
        &tasked,
        "ship_check",
        json!({"type": "test", "name": "parity", "command": command}),
    )
    .await;
    let payload = await_terminal(&tasked, &task_id_of(&handle), Duration::from_secs(30)).await;
    let task_body = completed_envelope(&payload);

    // `timestamp` is wall-clock and legitimately differs; everything the check
    // MEANS must not.
    for field in [
        "passed",
        "verified",
        "exit_code",
        "command",
        "name",
        "type",
        "details",
    ] {
        assert_eq!(
            task_body[field], inline_body[field],
            "field `{field}` diverged between the task and inline paths\n\
             task:   {task_body}\ninline: {inline_body}"
        );
    }
    assert_not_a_soft_error(&task_body);
    assert_eq!(task_body["verified"], json!(true));
    assert_eq!(task_body["passed"], json!(true));

    let _ = inline.cancel().await;
    let _ = tasked.cancel().await;
}

/// Claim 4: a task that COMPLETED is not a gate that PASSED. The two are
/// different axes, and conflating them is exactly how a fabricated green check
/// would get through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_command_run_as_a_task_still_reports_failure() {
    let client = session("tasks-failing", true).await;
    arm_an_active_task(&client).await;

    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "red", "command": "exit 3"}),
    )
    .await;
    let payload = await_terminal(&client, &task_id_of(&handle), Duration::from_secs(30)).await;

    assert_eq!(
        payload.status(),
        TaskStatus::Completed,
        "the task itself ran fine; it is the GATE that failed"
    );
    let body = completed_envelope(&payload);
    assert_eq!(body["passed"], json!(false), "envelope: {body}");
    assert_eq!(body["exit_code"], json!(3));
    assert_eq!(body["verified"], json!(true));

    let _ = client.cancel().await;
}

/// Claim 5: cancellation reaches a terminal state, rather than leaving the task
/// working forever. rmcp's `cancel_task` only flips a watch channel — if the
/// operation never selects on it, this stays `working` until the TTL sweep.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_gate_settles_the_task_as_cancelled() {
    let client = session("tasks-cancel-status", true).await;
    arm_an_active_task(&client).await;

    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "slow", "command": "sleep 30"}),
    )
    .await;
    let id = task_id_of(&handle);

    tokio::time::sleep(Duration::from_millis(200)).await;
    client
        .peer()
        .cancel_task(CancelTaskParams::new(id.clone()))
        .await
        .expect("tasks/cancel");

    // A budget far below the command's own 30s: if this only settles because
    // `sleep 30` finished, the assertion could never be met.
    let payload = await_terminal(&client, &id, Duration::from_secs(5)).await;
    assert_eq!(payload.status(), TaskStatus::Cancelled, "got {payload:?}");

    let _ = client.cancel().await;
}

/// Claim 6: and the actual process dies. This is the claim a code reading cannot
/// make: an abandoned `tokio::select!` loser is only killed because
/// `run_check_command` builds its child with `.kill_on_drop(true)`.
///
/// The marker file is the whole test. A surviving child would create it, and a
/// child that merely stopped being AWAITED would still create it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_a_gate_kills_the_child_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("the-child-survived");
    // The child announces itself BEFORE it starts sleeping. Without this the
    // test fails open: a cancellation that arrived before the shell had even
    // spawned would leave the marker absent for the boring reason, and the
    // assertion below would pass without ever exercising kill_on_drop. Making
    // the premise observable is what turns "probably raced correctly" into a
    // fact.
    let started = dir.path().join("the-child-started");
    let command = format!(
        "touch {} && sleep 2 && touch {}",
        started.display(),
        marker.display()
    );

    let client = session("tasks-cancel-kill", true).await;
    arm_an_active_task(&client).await;

    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "killable", "command": command}),
    )
    .await;
    let id = task_id_of(&handle);

    // Wait for the child to EXIST rather than for a duration to elapse: the
    // cancellation must land on a running process or it proves nothing.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never started, so cancelling it could not prove anything",
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    client
        .peer()
        .cancel_task(CancelTaskParams::new(id.clone()))
        .await
        .expect("tasks/cancel");
    await_terminal(&client, &id, Duration::from_secs(5)).await;

    // Past when the child would have finished sleeping, had it survived.
    //
    // NOT the synchronisation smell: this proves an ABSENCE, and there is no
    // event to wait on when the expected outcome is "the marker never appears".
    // The failure direction is the safe one — too short gives a false PASS —
    // so a generous duration is the honest instrument. The wait ABOVE is the
    // one that had to become a condition, because there the awaited thing
    // (the child starting) does exist.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        !marker.exists(),
        "the child process outlived its cancellation and still ran to completion"
    );

    let _ = client.cancel().await;
}

/// Claim 7: a cancelled gate is neither passed nor silently missing. The
/// dangerous version of this feature records a check anyway; the other
/// dangerous version leaves a `ship_finalize` believing the gate is still
/// coming.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_gate_records_no_check_at_all() {
    let client = session("tasks-cancel-nocheck", true).await;
    arm_an_active_task(&client).await;

    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "abandoned", "command": "sleep 30"}),
    )
    .await;
    let id = task_id_of(&handle);
    tokio::time::sleep(Duration::from_millis(200)).await;
    client
        .peer()
        .cancel_task(CancelTaskParams::new(id.clone()))
        .await
        .expect("tasks/cancel");
    await_terminal(&client, &id, Duration::from_secs(5)).await;

    let status = complete_body(&call(&client, "ship_status", json!({})).await);
    let checks = status["checks"]
        .as_array()
        .unwrap_or_else(|| panic!("ship_status should carry a checks array: {status}"))
        .clone();
    assert!(
        checks.is_empty(),
        "a cancelled gate must leave no check behind, found {checks:?}"
    );

    let _ = client.cancel().await;
}

/// Claim 8: the other 47 tools return in milliseconds. Turning them into tasks
/// would make every trivial call a two-round-trip poll.
///
/// **This test is vacuous against half of what it appears to cover, and that is
/// deliberate.** No tool other than `ship_check` accepts a `command` argument,
/// so breaking the tool-NAME half of `wants_task` leaves this green — proven
/// by deliberately breaking it. The name half is carried by
/// `mcp::tasks::tests::every_other_tool_stays_inline_even_carrying_a_command_key`,
/// which can pass an argument no real call would. What this test does prove is
/// the wire-level half: that the real router, with a real tasks-declaring
/// client, does not convert an ordinary call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_tools_stay_inline_even_for_a_tasks_declaring_client() {
    let client = session("tasks-others-inline", true).await;

    for (tool, arguments) in [
        ("ship_status", json!({})),
        ("roadmap_status", json!({})),
        (
            "ship_set_objective",
            json!({"description": "not a long gate"}),
        ),
    ] {
        let response = call(&client, tool, arguments).await;
        assert!(
            matches!(response, CallToolResponse::Complete(_)),
            "{tool} must stay inline, got {response:?}"
        );
    }

    // And the twin that keeps this from passing vacuously: a self-reported
    // `ship_check` — same tool, no command, no subprocess — also stays inline.
    let response = call(
        &client,
        "ship_check",
        json!({"type": "manual", "name": "read it myself", "passed": true}),
    )
    .await;
    assert!(
        matches!(response, CallToolResponse::Complete(_)),
        "a ship_check with no command does no work and must stay inline, got {response:?}"
    );

    let _ = client.cancel().await;
}

/// Claim 10: the heartbeat that ticks for a task-backed gate is the one started
/// INSIDE the spawned future — provable because only that one knows the task id.
/// A heartbeat started at the seam could not name it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_backed_gates_ticks_name_the_task() {
    let (client, seen) = session_recording("tasks-tick-label", true).await;
    arm_an_active_task(&client).await;
    seen.lock().unwrap().clear();

    // Comfortably past FIRST_TICK (2s) with room for several ticks.
    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "ticking", "command": "sleep 6"}),
    )
    .await;
    let id = task_id_of(&handle);
    await_terminal(&client, &id, Duration::from_secs(30)).await;

    let frames = seen.lock().unwrap().clone();
    let gate_ticks: Vec<&ProgressNotificationParam> = frames
        .iter()
        .filter(|f| {
            f.message
                .as_deref()
                .is_some_and(|m| m.contains("ship_check"))
        })
        .collect();
    assert!(
        !gate_ticks.is_empty(),
        "a 6s task-backed gate must still tick; saw {frames:?}"
    );
    let expected = tick_message(&format!("ship_check (task {id})"), 1);
    assert_eq!(
        gate_ticks[0].message.as_deref(),
        Some(expected.as_str()),
        "the first tick must name the task by exact value; saw {:?}",
        gate_ticks[0].message
    );

    let _ = client.cancel().await;
}

/// Claim 11: THE failure mode `progress.rs` named. Two emitters against one
/// token would each count from 1, so the sequence would repeat a value. One
/// emitter is strictly increasing.
///
/// This is the assertion that a code reading cannot make: "I only see one
/// `Heartbeat::start` on this path" is exactly the reasoning that misses a
/// second mechanism added later.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_backed_gate_has_exactly_one_ticker() {
    let (client, seen) = session_recording("tasks-tick-single", true).await;
    arm_an_active_task(&client).await;
    seen.lock().unwrap().clear();

    let handle = call(
        &client,
        "ship_check",
        json!({"type": "test", "name": "ticking", "command": "sleep 8"}),
    )
    .await;
    let id = task_id_of(&handle);
    await_terminal(&client, &id, Duration::from_secs(30)).await;

    let progresses: Vec<u64> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|f| {
            f.message
                .as_deref()
                .is_some_and(|m| m.contains("ship_check"))
        })
        .map(|f| f.progress as u64)
        .collect();

    assert!(
        progresses.len() >= 2,
        "need at least two ticks for this to mean anything, got {progresses:?}"
    );
    assert_eq!(
        progresses,
        (1..=progresses.len() as u64).collect::<Vec<_>>(),
        "the tick counter must be a single strictly-increasing run from 1 — a \
         repeated value means a SECOND progress mechanism is emitting against \
         the same token, which is the one thing mcp/progress.rs forbade"
    );

    let _ = client.cancel().await;
}

/// Claim 9: the server's own declaration. Without it rmcp answers `tasks/get`
/// with `-32601`, so claim 1's handle would be one no client could ever poll —
/// and every test above would fail at the first `await_terminal`, which is
/// precisely why this is asserted directly rather than inferred.
#[test]
fn the_server_declares_the_tasks_extension() {
    use rmcp::ServerHandler;
    let info = build_unified("tasks-capability").get_info();
    assert!(
        info.capabilities.supports_tasks(),
        "the server must advertise the tasks extension: {:?}",
        info.capabilities
    );
    // The siblings it must not have displaced.
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());
}
