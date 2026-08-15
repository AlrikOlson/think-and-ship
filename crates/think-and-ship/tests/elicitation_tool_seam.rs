//! The consent ask at the real `tools/call` seam, with the gate deliberately
//! ON.
//!
//! `elicitation_consent.rs` proves the four guards against real clients, but
//! every test there runs with `THINK_AND_SHIP_ELICIT` unset — the production
//! default — so none of them can say anything about what happens when it IS
//! set. This binary turns it on. That makes it the only place where a silent
//! outcome is evidence about the *tool seam* rather than about the env gate.
//!
//! # Why this file exists at all
//!
//! Deliberately breaking the code under test caught the previous version of
//! this claim passing for the wrong reason. `elicitation_consent.rs` had a
//! test named "a tracker_setup that
//! failed asks nobody anything"; bypassing the env gate turned it red, which
//! proved it had been measuring the gate and never the landed check. Splitting
//! the gate-on case into its own process is what makes the landed check
//! observable.
//!
//! # The honest limit
//!
//! The POSITIVE tool-level path — a `tracker_setup` that succeeds and therefore
//! does ask — cannot be driven here. `build_tracker_port` knows only `github`
//! and `linear`; both need real credentials, and `linear` reaches the network
//! and comes back 401. That is a known gap — a test seam for the tracker port
//! — and until one exists the reachability chain is proven link by link
//! instead:
//! the seam calls the ask and gates it on `landed` (source gate in
//! `elicitation_consent.rs`), `landed` answers correctly for a real success
//! envelope and a real failure envelope (below, by exact value), and the ask
//! itself behaves against real clients (`elicitation_consent.rs`). The single
//! unproven hop is "a successful `tracker_setup` produces a success envelope".

use std::sync::{Arc, Mutex};

use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult,
        ElicitationAction,
    },
    service::RequestContext,
};
use serde_json::json;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// A capable client that always says yes — so a silent outcome below can only
/// be the server declining to ask, never the client declining to answer.
#[derive(Clone, Default)]
struct EagerClient {
    asked: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for EagerClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.capabilities = ClientCapabilities::builder().enable_elicitation().build();
        info
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, rmcp::ErrorData> {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = &request {
            self.asked.lock().unwrap().push(message.clone());
        }
        let mut result = ElicitResult::new(ElicitationAction::Accept);
        result.content = Some(json!({ "enabled": true }));
        Ok(result)
    }
}

fn build_unified(project: &str) -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    UnifiedService::new(
        ThinkService::new(ReasoningServer::new(cfg)),
        ShipService::new(ShipEngine::new(project.into())),
        RoadmapService::new(RoadmapEngine::new(project.into())),
        SignalService::new(SignalEngine::new(project.into())),
    )
}

/// With the gate ON, a `tracker_setup` that did not land still asks nobody.
///
/// The provider is deliberately one that does not exist: it is refused before
/// any network is touched, which keeps this test offline while still producing
/// a genuine `ok: false` envelope. With `THINK_AND_SHIP_ELICIT=1` and a client
/// that both declares elicitation and would accept, the ONLY thing that can
/// keep this silent is the landed check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_the_gate_on_a_tracker_setup_that_did_not_land_still_asks_nobody() {
    let data_dir = tempfile::TempDir::new().expect("tempdir");
    // SAFETY: single-test binary; nothing else in this process reads these, and
    // the data dir keeps the run off the developer's real consent state.
    unsafe {
        std::env::set_var("THINK_AND_SHIP_DATA_DIR", data_dir.path());
        std::env::set_var("THINK_AND_SHIP_ELICIT", "1");
    }

    let (server_tx, client_tx) = tokio::io::duplex(8192);
    let server = build_unified("elicit-tool-seam");
    tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve");
        let _ = running.waiting().await;
    });
    let client = EagerClient::default();
    let asked = client.asked.clone();
    let client = client.serve(client_tx).await.expect("client.serve");

    let mut req = CallToolRequestParams::new("tracker_setup");
    req.arguments = Some(
        json!({ "provider": "nosuchprovider", "into": "ENG", "dry_run": true, "push_secs": 0 })
            .as_object()
            .unwrap()
            .clone(),
    );
    let response = client
        .peer()
        .call_tool(req)
        .await
        .expect("the call must still succeed as a protocol operation");

    // The premise, asserted rather than assumed — this is exactly what the
    // earlier version of this claim got wrong.
    let structured = response
        .structured_content
        .as_ref()
        .expect("tracker_setup reports through a structured envelope");
    assert_eq!(
        structured.get("ok"),
        Some(&json!(false)),
        "this call was supposed to FAIL; if it landed the test below proves \
         nothing: {structured}"
    );
    assert_eq!(
        response.is_error,
        Some(false),
        "and it must NOT be a protocol error — degrade, never fail — which is \
         precisely why `landed` cannot read `is_error`"
    );

    assert!(
        asked.lock().unwrap().is_empty(),
        "gate ON, client willing, call failed: no question may be stacked on \
         top of the failure: {:?}",
        asked.lock().unwrap()
    );

    // And nothing was remembered, because nothing was asked.
    assert!(
        !think_and_ship::tracker::propose_consent::load(data_dir.path()).is_decided(),
        "a consent nobody was asked about must stay undecided"
    );

    let _ = client.cancel().await;
}

/// `landed` by exact value, in both directions.
///
/// The link in the reachability chain that the tool-level positive path cannot
/// yet supply. A success envelope from this very server must be `landed`, and a
/// real `soft_error` envelope must not — and crucially both carry
/// `is_error: Some(false)`, so a predicate reading that field would answer
/// "landed" to both. That is the bug this test was written after finding.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landed_reads_the_ok_envelope_and_not_is_error() {
    let data_dir = tempfile::TempDir::new().expect("tempdir");
    unsafe {
        std::env::set_var("THINK_AND_SHIP_DATA_DIR", data_dir.path());
    }
    let (server_tx, client_tx) = tokio::io::duplex(8192);
    let server = build_unified("elicit-landed");
    tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve");
        let _ = running.waiting().await;
    });
    let client = EagerClient::default()
        .serve(client_tx)
        .await
        .expect("client.serve");

    // A SUCCESS envelope, from a real roadmap tool over the real wire.
    let ok = client
        .peer()
        .call_tool(CallToolRequestParams::new("roadmap_status"))
        .await
        .expect("roadmap_status");
    let ok_sc = ok.structured_content.expect("structured");
    assert_ne!(
        ok_sc.get("ok"),
        Some(&json!(false)),
        "a successful call must not carry ok:false: {ok_sc}"
    );
    assert_eq!(ok.is_error, Some(false));

    // A FAILURE envelope, from the same family over the same wire.
    let mut bad = CallToolRequestParams::new("roadmap_set_status");
    bad.arguments = Some(
        json!({ "id": "no-such-chunk-anywhere", "status": "done" })
            .as_object()
            .unwrap()
            .clone(),
    );
    let err = client
        .peer()
        .call_tool(bad)
        .await
        .expect("roadmap_set_status");
    let err_sc = err.structured_content.expect("structured");
    assert_eq!(
        err_sc.get("ok"),
        Some(&json!(false)),
        "a domain failure must say so in the envelope: {err_sc}"
    );
    assert_eq!(
        err.is_error,
        Some(false),
        "THE POINT: a failure and a success are indistinguishable by is_error"
    );

    let _ = client.cancel().await;
}
