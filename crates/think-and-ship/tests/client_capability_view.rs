//! End-to-end proof that `think_engine_status` reports what the client on the
//! other end of the request actually declared (roadmap chunk
//! `mcp-stderr-observability-gap`).
//!
//! # Why this file has to exist at all
//!
//! Claude Code captures a server's stderr exactly once, at connect time, and
//! discards everything after. The two permanent capability lines
//! shipped in `mcp/tasks.rs` and `mcp/elicit.rs` are therefore unreadable
//! through the only host we use, and "is this capability live?" was answerable
//! only by timing a six-second gate against a control. This is the pull channel
//! that replaces the trick — and a channel nobody can read is exactly the
//! failure mode being fixed, so the claims below are asserted at the WIRE, not
//! against the pure function.
//!
//! The harness clones `task_long_gates.rs`: a real rmcp client over an
//! in-memory duplex, with the server SPAWNED rather than awaited — `serve`
//! completes the initialize handshake, so awaiting it before the client exists
//! deadlocks with no output at all.
//!
//! # The claims, one test each
//!
//! 1. a tasks-declaring client is reported as declaring tasks, and the
//!    extension id it declared is listed,
//! 2. a client that declared nothing is reported as declaring nothing — the
//!    discriminator, without which claim 1 could pass on a hardcoded `true`,
//! 3. an elicitation-declaring client is reported as such AND its usable mode
//!    is listed, which is the gate `elicit.rs` really consults,
//! 4. a non-declaring client has no elicitation and no modes,
//! 5. `capability_source` names where the declaration was read from,
//! 6. the client's own name and version cross the wire,
//! 7. the pre-existing engine fields are untouched by the block (this carries
//!    the claim that used to live in `think_mcp.rs`'s
//!    `engine_status_call_returns_structured_content`),
//! 8. and the block is present on the tool's declared outputSchema, so a client
//!    reading only `tools/list` learns the field exists.

use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitationCapability,
        Implementation, TASKS_EXTENSION_ID,
    },
};
use serde_json::Value;
use think_and_ship::mcp::UnifiedService;
use think_and_ship::mcp::client_view::{SOURCE_INITIALIZE, SOURCE_NONE};
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// What a client is willing to say about itself. Each flag maps to one
/// declaration so a test can turn exactly one thing on.
#[derive(Clone, Copy)]
struct Declares {
    tasks: bool,
    elicitation: bool,
}

impl Declares {
    const NOTHING: Self = Self {
        tasks: false,
        elicitation: false,
    };
    const EVERYTHING: Self = Self {
        tasks: true,
        elicitation: true,
    };
}

#[derive(Clone)]
struct DeclaringClient {
    declares: Declares,
}

impl ClientHandler for DeclaringClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        let mut builder = ClientCapabilities::builder();
        if self.declares.tasks {
            builder = builder.enable_tasks();
        }
        let mut capabilities = builder.build();
        if self.declares.elicitation {
            // `#[non_exhaustive]` — default then assign, never a struct literal.
            capabilities.elicitation = Some(ElicitationCapability::default());
        }
        info.capabilities = capabilities;
        info.client_info = Implementation::new("test-harness-client", "9.9.9");
        info
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

type Client = rmcp::service::RunningService<RoleClient, DeclaringClient>;

/// Both halves start concurrently: `serve` completes the initialize handshake,
/// so awaiting the server's before the client exists deadlocks.
async fn session(project: &str, declares: Declares) -> Client {
    let server = build_unified(project);
    let (server_tx, client_tx) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let running = server.serve(server_tx).await.expect("server.serve failed");
        let _ = running.waiting().await;
    });
    DeclaringClient { declares }
        .serve(client_tx)
        .await
        .expect("client.serve failed")
}

/// The whole `think_engine_status` envelope, as the client sees it.
async fn status(client: &Client) -> Value {
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new(
            "think_engine_status".to_string(),
        ))
        .await
        .expect("think_engine_status failed");
    serde_json::to_value(
        result
            .structured_content
            .expect("think_engine_status must return structuredContent"),
    )
    .expect("structuredContent must be JSON")
}

fn c_name(d: Declares) -> &'static str {
    if d.tasks || d.elicitation {
        "declaring"
    } else {
        "silent"
    }
}

async fn block_for(declares: Declares) -> Value {
    let c = session(c_name(declares), declares).await;
    let block = status(&c).await["client"].clone();
    assert!(
        !block.is_null(),
        "a wire call always has a client, so the block must never be absent"
    );
    block
}

#[tokio::test]
async fn a_tasks_declaring_client_is_reported_as_declaring_tasks() {
    let block = block_for(Declares {
        tasks: true,
        elicitation: false,
    })
    .await;
    assert_eq!(
        block["declares_tasks"], true,
        "a client that enabled the SEP-2663 extension must read back as declaring it"
    );
    assert_eq!(
        block["extensions"],
        serde_json::json!([TASKS_EXTENSION_ID]),
        "the extension id itself is listed, not just the derived boolean"
    );
}

/// The discriminator. Without it, claim 1 is satisfied by a hardcoded `true` —
/// which is precisely the shape of bug that made the stderr lines useless: a
/// report nobody could contradict.
#[tokio::test]
async fn a_client_that_declared_nothing_is_reported_as_declaring_nothing() {
    let block = block_for(Declares::NOTHING).await;
    assert_eq!(block["declares_tasks"], false);
    assert_eq!(block["declares_elicitation"], false);
    assert_eq!(
        block["extensions"],
        serde_json::json!([]),
        "no declaration means an empty list, not a missing field"
    );
}

#[tokio::test]
async fn an_elicitation_declaring_client_reports_the_mode_the_gate_actually_uses() {
    let block = block_for(Declares {
        tasks: false,
        elicitation: true,
    })
    .await;
    assert_eq!(block["declares_elicitation"], true);
    // `elicit.rs` gates on `ElicitationMode::Form` being present, NOT on the
    // declaration — so the answer to "would this client be asked?" lives here.
    assert_eq!(
        block["elicitation_modes"],
        serde_json::json!(["form"]),
        "a bare elicitation declaration means form mode, and the report must say so"
    );
}

#[tokio::test]
async fn a_non_declaring_client_has_no_elicitation_mode_to_be_asked_through() {
    let block = block_for(Declares::NOTHING).await;
    assert_eq!(block["declares_elicitation"], false);
    assert_eq!(
        block["elicitation_modes"],
        serde_json::json!([]),
        "no declaration must not be reported as a usable mode"
    );
}

/// A capability report that does not say where it read the capability from
/// repeats the failure mode of the stderr lines: authoritative-looking and
/// unlocatable.
#[tokio::test]
async fn the_report_names_where_the_declaration_was_read_from() {
    let declared = block_for(Declares::EVERYTHING).await;
    assert_eq!(
        declared["capability_source"], SOURCE_INITIALIZE,
        "a handshake declaration must be attributed to the handshake"
    );
    // A client that declared NOTHING still completed a handshake and still sent
    // a (empty) capabilities object, so its source is `initialize` too — and
    // every capability below it is false. That distinction is the point of the
    // field: "declared nothing" and "no declaration was visible" are different
    // answers, and only `SOURCE_NONE` means the second one. Asserting they
    // agree here would have hidden it.
    let silent = block_for(Declares::NOTHING).await;
    assert_eq!(
        silent["capability_source"], SOURCE_INITIALIZE,
        "an empty declaration is still a declaration, made at the handshake"
    );
    assert_ne!(
        silent["capability_source"], SOURCE_NONE,
        "SOURCE_NONE is reserved for 'nothing was visible', which a completed \
         handshake is not"
    );
}

#[tokio::test]
async fn the_clients_own_name_and_version_cross_the_wire() {
    let block = block_for(Declares::EVERYTHING).await;
    assert_eq!(block["name"], "test-harness-client");
    assert_eq!(block["version"], "9.9.9");
    assert!(
        block["protocol_version"].is_string(),
        "the negotiated protocol version is part of 'what am I talking to'"
    );
}

/// Carries the claim that used to live in `think_mcp.rs`'s
/// `engine_status_call_returns_structured_content`, which called the handler
/// directly and can no longer do so.
#[tokio::test]
async fn the_engine_fields_are_untouched_by_the_client_block() {
    let s = status(&session("untouched", Declares::EVERYTHING).await).await;
    assert!(s["version"].is_string());
    assert!(s["persistence_enabled"].is_boolean());
    assert!(s["sessions_enabled"].is_boolean());
    assert!(
        s["total_steps"].is_number(),
        "merging the client block must not displace the engine snapshot"
    );
}

/// The block is useless if a client cannot learn it exists without calling the
/// tool. `tools/list` must declare it.
#[tokio::test]
async fn the_client_block_is_declared_on_the_tools_output_schema() {
    let svc = build_unified("schema");
    let tools = svc.list_tools_view();
    let status = tools
        .iter()
        .find(|t| t.name == "think_engine_status")
        .expect("think_engine_status should be served");
    let schema = status
        .output_schema
        .as_ref()
        .expect("think_engine_status declares an outputSchema");
    let json = serde_json::to_string(schema).expect("schema serializes");
    assert!(
        json.contains("\"client\""),
        "the outputSchema must declare the client property"
    );
    for field in [
        "declares_tasks",
        "declares_elicitation",
        "elicitation_modes",
        "capability_source",
        "asking_enabled",
    ] {
        assert!(
            json.contains(field),
            "the outputSchema must describe {field}, or a reader cannot know to look for it"
        );
    }
}
