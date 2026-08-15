//! Tool-call telemetry proven at the wire: a tool called N times reads N —
//! asserted against a **driven sequence over the real wire**, not against a
//! store someone eyeballed.
//!
//! Driving a real rmcp client is the point. This suite exists because
//! `tools_used` — a number produced by asking the caller what it did — was
//! wrong by ~3,000x on the hottest verb in the system. A test that called
//! `CallCounter::record` directly would be the same category of evidence: it
//! would prove the counter can add, and prove nothing about whether the
//! dispatcher reaches it. Only a `tools/call` that traverses
//! `UnifiedService::route` can fail when the wiring is cut.
//!
//! What each test pins down:
//!
//!  - errors count (a verb that is always misused is data, not absence),
//!  - refused families count (the datum a selection decision needs),
//!  - unknown names do not mint keys,
//!  - and a *later, separate* reader — as the `calls` CLI is — sees all of it.
//!
//! The last test covers soak behaviour: it proves the counter refuses to let
//! a day of traffic be read as a verdict.

use rmcp::{
    ClientHandler, ServiceExt,
    model::{CallToolRequestParams, ErrorCode},
};
use think_and_ship::infra::{Domain, Persistence, PersistenceConfig};
use think_and_ship::mcp::UnifiedService;
use think_and_ship::mcp::unified::{Family, FamilySelection};
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::usage::{CallCounter, CallCounts, UNRECOGNIZED, Verdict};

const PROJECT: &str = "proj-callcount";

#[derive(Clone)]
struct TestClient;
impl ClientHandler for TestClient {}

/// A scratch data dir unique to this test, so counts from one test cannot be
/// read as another's.
fn scratch(name: &str) -> PersistenceConfig {
    let dir = std::env::temp_dir().join(format!(
        "tas-callcounts-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    PersistenceConfig::from_env()
        .enabled(true)
        .with_data_dir(dir)
}

fn build_unified(cfg: &PersistenceConfig, families: FamilySelection) -> UnifiedService {
    let mut dcfg = ThinkConfig::default();
    dcfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(dcfg));
    let ship = ShipService::new(ShipEngine::new("test-abc123".into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new("test-abc123".into()));
    let signal = SignalService::new(SignalEngine::new("test-abc123".into()));

    // Every registered name, taken the same way the service does — so the
    // counter under test knows exactly the names a real deployment knows.
    let svc = UnifiedService::new(think, ship, roadmap, signal);
    let known: Vec<String> = svc
        .list_tools_view()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    svc.with_families(families)
        .with_call_counter(CallCounter::new(
            Persistence::new(cfg, Domain::Usage),
            PROJECT,
            known,
            true,
        ))
}

async fn pair(
    svc: UnifiedService,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    tokio::task::JoinHandle<()>,
) {
    let (server_tx, client_tx) = tokio::io::duplex(4096);
    let handle = tokio::spawn(async move {
        let running = svc.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });
    let client = TestClient.serve(client_tx).await.unwrap();
    (client, handle)
}

/// Read the counts the way the `calls` CLI does: a fresh handle over the
/// partition, with no server involved.
fn read_as_the_cli_would(cfg: &PersistenceConfig) -> CallCounts {
    let reader = Persistence::new(cfg, Domain::Usage);
    reader
        .load::<CallCounts>(PROJECT)
        .expect("usage partition reads")
        .unwrap_or_default()
}

/// THE WIRING PROOF. Cut the `self.calls.record(...)` line out of
/// `UnifiedService::route` and this test goes red: the counts come back empty.
/// Move it below the family guard and `refused_families_still_count` goes red.
/// Move it after dispatch and `errors_count` goes red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn n_calls_over_the_wire_reads_n() {
    let cfg = scratch("exact");
    let (client, server) = pair(build_unified(&cfg, FamilySelection::all())).await;

    // 5 successful roadmap_status calls and 2 think_engine_status calls.
    for _ in 0..5 {
        client
            .peer()
            .call_tool(CallToolRequestParams::new("roadmap_status"))
            .await
            .expect("roadmap_status succeeds");
    }
    for _ in 0..2 {
        client
            .peer()
            .call_tool(CallToolRequestParams::new("think_engine_status"))
            .await
            .expect("think_engine_status succeeds");
    }

    let got = read_as_the_cli_would(&cfg);
    assert_eq!(
        got.counts.get("roadmap_status"),
        Some(&5),
        "5 calls must read 5, not 'roughly 5' and not 0 — counts: {:?}",
        got.counts
    );
    assert_eq!(got.counts.get("think_engine_status"), Some(&2));
    assert_eq!(got.total(), 7, "no phantom increments: {:?}", got.counts);
    assert!(
        !got.updated_at.is_empty(),
        "the last-call stamp must be written"
    );

    client.cancel().await.ok();
    server.abort();
}

/// A call that ERRORS is still a call. This is the criterion that decides
/// where the increment goes: after dispatch, a verb nobody can use correctly
/// would read as a verb nobody uses — and get retired for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_count() {
    let cfg = scratch("errors");
    let (client, server) = pair(build_unified(&cfg, FamilySelection::all())).await;

    // `roadmap_start_chunk` requires an `id`; omitting it is a real, rejected
    // call — the shape of "this verb is always misused".
    for _ in 0..3 {
        let outcome = client
            .peer()
            .call_tool(CallToolRequestParams::new("roadmap_start_chunk"))
            .await;
        assert!(
            outcome.is_err() || outcome.is_ok_and(|r| r.is_error.unwrap_or(false)),
            "a missing required arg must not be treated as a clean call"
        );
    }

    let got = read_as_the_cli_would(&cfg);
    assert_eq!(
        got.counts.get("roadmap_start_chunk"),
        Some(&3),
        "3 failed calls must read 3: {:?}",
        got.counts
    );

    client.cancel().await.ok();
    server.abort();
}

/// A tool a deployment REFUSES is counted. Family selection is the decision
/// this data feeds, so "how often did someone reach for the family we turned
/// off?" has to be answerable — which means counting in front of the guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_families_still_count() {
    let cfg = scratch("refused");
    let only_roadmap = FamilySelection::parse("roadmap").expect("roadmap is a family");
    assert!(!only_roadmap.contains(Family::Signal));
    let (client, server) = pair(build_unified(&cfg, only_roadmap)).await;

    let err = client
        .peer()
        .call_tool(CallToolRequestParams::new("signal_status"))
        .await
        .expect_err("a deselected family must be refused");
    let refusal = format!("{err:?}");
    assert!(
        refusal.contains("signal") || refusal.contains(&format!("{:?}", ErrorCode::INVALID_PARAMS)),
        "the refusal should name the family: {refusal}"
    );

    let got = read_as_the_cli_would(&cfg);
    assert_eq!(
        got.counts.get("signal_status"),
        Some(&1),
        "a refused call is still a call: {:?}",
        got.counts
    );

    client.cancel().await.ok();
    server.abort();
}

/// An unrecognized name buckets rather than minting a key. Without this the
/// file's key space is whatever a client types.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_names_do_not_mint_keys() {
    let cfg = scratch("bounded");
    let (client, server) = pair(build_unified(&cfg, FamilySelection::all())).await;

    for i in 0..6 {
        let _ = client
            .peer()
            .call_tool(CallToolRequestParams::new(format!(
                "think_not_a_real_tool_{i}"
            )))
            .await;
    }

    let got = read_as_the_cli_would(&cfg);
    assert_eq!(
        got.counts.len(),
        1,
        "6 invented names must collapse to one bucket: {:?}",
        got.counts
    );
    assert_eq!(got.counts.get(UNRECOGNIZED), Some(&6));

    client.cancel().await.ok();
    server.abort();
}

/// THE SOAK PROOF, and the one that makes `call-counts-soak` real: a real
/// session over the real wire produces a real reading, and that reading may
/// STILL not be used to call anything cold.
///
/// This is the day-one state exactly — a fresh install, one burst of traffic,
/// every other verb at zero. If [`CallCounts::verdict`] ever returns `Cold`
/// here, the analysis that reads it will retire tools nobody has had the
/// chance to use, which is the failure the whole module exists to prevent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_days_traffic_is_not_a_soak_and_licenses_no_retirement() {
    let cfg = scratch("soak");
    let (client, server) = pair(build_unified(&cfg, FamilySelection::all())).await;

    for _ in 0..12 {
        client
            .peer()
            .call_tool(CallToolRequestParams::new("roadmap_status"))
            .await
            .expect("roadmap_status succeeds");
    }

    let got = read_as_the_cli_would(&cfg);
    assert_eq!(got.total(), 12, "precondition: the traffic landed");
    assert_eq!(
        got.active_days.len(),
        1,
        "one burst is one active day: {:?}",
        got.active_days
    );

    let soak = got.soak();
    assert!(
        !soak.met,
        "12 calls in one day must never qualify as an observation window"
    );
    assert_eq!(
        soak.missing.len(),
        2,
        "both the call and day thresholds are unmet: {:?}",
        soak.missing
    );

    // The verb the parent chunk actually wants an answer about. It reads zero
    // here, and zero is not yet an answer.
    match got.verdict("signal_research") {
        Verdict::SoakTooShort(_) => {}
        other => panic!(
            "a day-one zero must be unreadable as evidence, got {other:?} \
             — this is the exact misreading the soak exists to block"
        ),
    }
    // And a verb that DID run reads as used regardless of the window.
    assert_eq!(got.verdict("roadmap_status"), Verdict::Used(12));

    client.cancel().await.ok();
    server.abort();
}
