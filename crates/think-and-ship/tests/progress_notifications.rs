//! End-to-end proof that a long tool call is no longer silent (roadmap chunk
//! `mcp-progress-and-logging`).
//!
//! Pairs a real rmcp client with the unified server over an in-memory duplex —
//! the same harness as `think_and_ship_e2e.rs` — and records every
//! `notifications/progress` frame the server sends. Nothing here inspects our
//! own internals: the assertions are about what actually crossed the wire.
//!
//! Four claims, one test each, so a single regression cannot hide behind
//! another's failure:
//!
//! 1. a slow `ship_check` gate emits ticks against the caller's OWN token,
//! 2. those ticks are liveness signals — rising counter, no fabricated `total`,
//!    message by exact value,
//! 3. a call shorter than the first-tick window stays silent (claim 3a), and
//! 4. a millisecond-long call emits nothing at all (claim 3b) — which is what
//!    keeps the feature from becoming noise across a 48-tool surface.
//!
//! 3a and 3b are separate on purpose: deleting the first-tick delay left 3b
//! green, because a microsecond call's ticker is aborted before the runtime
//! ever schedules it. Only a sub-second-but-real call holds the constant honest.
//!
//! The fourth property — a caller that sends no `progressToken` gets an
//! identical result — cannot be produced here: rmcp's client sets a token on
//! every outbound request unconditionally (`service.rs:800`). It is proven at
//! the unit level in `mcp::progress`.

use std::sync::{Arc, Mutex};

use rmcp::{
    ClientHandler, ServiceExt,
    model::{
        CallToolRequest, CallToolRequestParams, ClientRequest, ProgressNotificationParam,
        ProgressToken, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions},
};
use serde_json::json;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::mcp::progress::{FIRST_TICK, tick_message};
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// A client that remembers every progress notification it was sent.
#[derive(Clone, Default)]
struct RecordingClient {
    seen: Arc<Mutex<Vec<ProgressNotificationParam>>>,
}

impl ClientHandler for RecordingClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<rmcp::RoleClient>,
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

async fn pair(
    project: &str,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, RecordingClient>,
    Arc<Mutex<Vec<ProgressNotificationParam>>>,
    tokio::task::JoinHandle<()>,
) {
    let server = build_unified(project);
    let (server_tx, client_tx) = tokio::io::duplex(4096);

    let server_handle = tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });

    let client = RecordingClient::default();
    let seen = client.seen.clone();
    let client_service = client.serve(client_tx).await.unwrap();
    (client_service, seen, server_handle)
}

fn args(value: serde_json::Value) -> Option<serde_json::Map<String, serde_json::Value>> {
    Some(value.as_object().unwrap().clone())
}

fn call(
    name: &'static str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> CallToolRequestParams {
    let mut req = CallToolRequestParams::new(name);
    req.arguments = arguments;
    req
}

/// Drive `ship_*` far enough that `ship_check` has an active task, so the slow
/// call under test returns a REAL result rather than an `invalid_state`
/// envelope. Progress that only shows up on an error path would prove nothing
/// about the working path.
async fn open_an_active_task(
    client: &rmcp::service::RunningService<rmcp::RoleClient, RecordingClient>,
) {
    let peer = client.peer();
    peer.call_tool(call(
        "ship_set_objective",
        args(json!({ "description": "progress e2e" })),
    ))
    .await
    .unwrap();
    peer.call_tool(call(
        "ship_plan",
        args(json!({
            "action": "add",
            "task_id": "gate",
            "title": "run a slow gate",
            "task_type": "test",
        })),
    ))
    .await
    .unwrap();
    peer.call_tool(call("ship_start", args(json!({ "task_id": "gate" }))))
        .await
        .unwrap();
}

/// Claim 1: a genuinely slow tool call reports liveness against the caller's
/// own progress token, and still returns its real result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_ship_check_emits_progress_against_the_callers_token() {
    let (client, seen, server_handle) = pair("test-progress-slow").await;
    open_an_active_task(&client).await;
    seen.lock().unwrap().clear();

    // Sent through the low-level path ON PURPOSE: `RequestHandle` exposes the
    // exact `progressToken` this client minted for this one request, so the
    // assertion below can name it. Going through `peer.call_tool` would leave
    // the token invisible and reduce the check to "some token was used" — which
    // a server inventing its own token would pass.
    //
    // 5s of real subprocess: comfortably past FIRST_TICK (2s) with room for at
    // least two ticks, and short enough not to drag the suite.
    let handle = client
        .peer()
        .send_request_with_option(
            ClientRequest::CallToolRequest(CallToolRequest::new(call(
                "ship_check",
                args(json!({
                    "type": "test",
                    "name": "slow gate",
                    "command": "sleep 5",
                })),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .expect("request should be accepted");
    let expected_token: ProgressToken = handle.progress_token.clone();
    let response = handle
        .await_response()
        .await
        .expect("ship_check should run");

    let ticks = seen.lock().unwrap().clone();
    assert!(
        ticks.len() >= 2,
        "a 5s call past a {}s first tick should report at least twice, got {}",
        FIRST_TICK.as_secs(),
        ticks.len()
    );

    // Every tick carries the token THIS request minted. A server that minted
    // its own would emit frames the caller cannot correlate with anything —
    // visually identical on the wire, functionally useless to a UI.
    for (i, tick) in ticks.iter().enumerate() {
        assert_eq!(
            tick.progress_token,
            expected_token,
            "tick {} used a token the caller never asked about",
            i + 1
        );
    }

    // The result is unaffected: the gate really ran and really passed.
    let ServerResult::CallToolResult(result) = response else {
        panic!("expected a CallToolResult");
    };
    let structured = result.structured_content.expect("structured result");
    assert_eq!(
        structured
            .pointer("/result/passed")
            .or_else(|| structured.pointer("/passed")),
        Some(&json!(true)),
        "the gate's real outcome must survive being narrated: {structured}"
    );

    let _ = client.cancel().await;
    server_handle.abort();
}

/// Claim 2: the ticks are honest liveness signals — a rising counter, NO
/// invented denominator, and a message that says which tool and how long.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ticks_rise_carry_no_fake_total_and_name_the_tool_and_elapsed_time() {
    let (client, seen, server_handle) = pair("test-progress-shape").await;
    open_an_active_task(&client).await;
    seen.lock().unwrap().clear();

    client
        .peer()
        .call_tool(call(
            "ship_check",
            args(json!({
                "type": "test",
                "name": "slow gate",
                "command": "sleep 5",
            })),
        ))
        .await
        .unwrap();

    let ticks = seen.lock().unwrap().clone();
    assert!(ticks.len() >= 2, "need at least two ticks to compare");

    for (i, tick) in ticks.iter().enumerate() {
        let n = (i + 1) as u64;
        assert_eq!(
            tick.progress, n as f64,
            "tick {n} must carry a monotonically rising counter"
        );
        assert_eq!(
            tick.total, None,
            "a shelled-out gate has no honest denominator; `total` must stay absent"
        );
        // Exact value, not "is_some()": an empty or generic message is a
        // notification a human learns nothing from, and would pass a shape check.
        assert_eq!(
            tick.message.as_deref(),
            Some(tick_message("ship_check", n).as_str()),
            "tick {n} message"
        );
    }
    assert_eq!(
        ticks[0].message.as_deref(),
        Some("ship_check still running (2s elapsed)")
    );
    assert_eq!(
        ticks[1].message.as_deref(),
        Some("ship_check still running (4s elapsed)")
    );

    let _ = client.cancel().await;
    server_handle.abort();
}

/// Claim 3a: a call that finishes INSIDE the first-tick window stays silent.
///
/// This test exists because claim 3b below turned out to pass for the wrong
/// reason. Deleting the first-tick delay entirely left 3b green: a
/// microsecond-long call is over before its spawned ticker is ever scheduled,
/// so abort-on-drop — not the delay — is what silences it. A sub-second but
/// non-trivial call (`roadmap_status` over a large plan, a quick `git` shell-out)
/// is the band the delay actually protects, and only a test in that band can
/// hold the constant honest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_shorter_than_the_first_tick_stays_silent() {
    let (client, seen, server_handle) = pair("test-progress-medium").await;
    open_an_active_task(&client).await;
    seen.lock().unwrap().clear();

    // 1s of real subprocess: long enough that the ticker is definitely running,
    // short enough that FIRST_TICK (2s) should still swallow every tick.
    client
        .peer()
        .call_tool(call(
            "ship_check",
            args(json!({
                "type": "test",
                "name": "quick gate",
                "command": "sleep 1",
            })),
        ))
        .await
        .unwrap();

    assert!(
        seen.lock().unwrap().is_empty(),
        "a 1s call is inside the {}s first-tick window and must stay silent, got {:?}",
        FIRST_TICK.as_secs(),
        seen.lock().unwrap()
    );

    let _ = client.cancel().await;
    server_handle.abort();
}

/// Claim 3b: the silence that makes this feature liveable across the surface.
/// Almost every one of the 48 tools returns in milliseconds; if those emitted
/// progress, the channel would be noise and a human would learn to ignore it.
/// This also catches a heartbeat that outlives its own call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fast_call_emits_no_progress_at_all() {
    let (client, seen, server_handle) = pair("test-progress-fast").await;

    client
        .peer()
        .call_tool(call("think_engine_status", args(json!({}))))
        .await
        .unwrap();

    // Wait well past the first-tick deadline: a heartbeat that leaked past the
    // end of its call would surface here rather than pass by being outrun.
    //
    // THIS SLEEP IS NOT THE SYNCHRONISATION SMELL, and a sweep for sleep-as-a-
    // wait will flag it again, so: this test proves an ABSENCE, and there is no
    // condition to wait on when the expected event count is zero. A readiness
    // wait can only resolve on something happening. The failure direction is
    // also the safe one to reason about — too short gives a false PASS, never a
    // false failure — so the honest instrument here is a generous duration, and
    // generous is what FIRST_TICK + 2s is. Contrast the cloud sync family, where
    // the awaited event does exist and a duration was simply a guess about it.
    tokio::time::sleep(FIRST_TICK + std::time::Duration::from_secs(2)).await;

    assert!(
        seen.lock().unwrap().is_empty(),
        "a millisecond-long call must emit nothing, got {:?}",
        seen.lock().unwrap()
    );

    let _ = client.cancel().await;
    server_handle.abort();
}
