//! The blocker write path, driven from BOTH surfaces against ONE store.
//!
//! # What this file is for
//!
//! `blocked-by-set-and-cleared` carries an acceptance criterion that no
//! single-surface test can express: *the MCP tool and `think-and-ship roadmap`
//! never disagree about what a chunk says*. A test of the MCP handler proves
//! the handler; a test of the CLI proves the CLI; neither proves they mean the
//! same thing by "blocked". The only way to assert agreement is to make one
//! surface write and the OTHER read, through a real persisted store — which is
//! what every test below does.
//!
//! # Why the CLI functions and not a spawned binary
//!
//! `cli::roadmap_block` / `cli::roadmap_unblock` are the entire body of the
//! `roadmap block` / `roadmap unblock` subcommands — `main.rs` dispatches
//! straight into them with the parsed arguments and does nothing else. Calling
//! them directly runs the shipped code path rather than a re-creation of it,
//! and `command_grammar_reaches_both_verbs` covers the one thing that call
//! skips: that the argv a human types actually reaches them.
//!
//! # The scratch data dir
//!
//! Same construction, and same reason, as `tests/tracker_command_seam.rs`:
//! `PersistenceConfig::from_env()` reads process-global environment, so the
//! variables are set exactly once per test binary. Without this, the CLI half
//! would resolve the developer's REAL roadmap and start blocking chunks in it.

use think_and_ship::cli;
use think_and_ship::infra::{Domain, Persistence, PersistenceConfig};
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;

const PROJECT_ID: &str = "blocker-command-seam";

/// The scratch data dir both surfaces share, created once for this binary.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    SCRATCH
        .get_or_init(|| {
            let dir = tempfile::TempDir::new().expect("tempdir");
            // SAFETY: set exactly once, before any test in this binary reads
            // them, and this binary runs nothing else.
            unsafe {
                std::env::set_var("THINK_AND_SHIP_DATA_DIR", dir.path());
                std::env::set_var("THINK_AND_SHIP_PERSIST", "true");
                std::env::set_var("THINK_AND_SHIP_PROJECT_NAME", PROJECT_ID);
            }
            dir
        })
        .path()
}

/// A persistence-backed engine on the scratch store — the same construction
/// `cli::load_roadmap_engine` performs, which is what makes this the OTHER
/// reader of the very bytes the CLI wrote.
fn engine() -> RoadmapEngine {
    scratch();
    let project_id = think_and_ship::infra::resolve_project_id(None);
    RoadmapEngine::new(project_id).with_persistence(Persistence::new(
        &PersistenceConfig::from_env(),
        Domain::Roadmap,
    ))
}

/// Seed a chunk and return its id. Ids are per-test so the tests stay
/// order-independent while sharing one store.
fn seeded(id: &str) -> String {
    let mut e = engine();
    e.add_chunk(
        id.to_string(),
        "A chunk".into(),
        ChunkStatus::Pending,
        10,
        String::new(),
        vec![],
        vec![],
        false,
    )
    .expect("chunk seeded");
    id.to_string()
}

/// Read a chunk back through a freshly loaded engine — never the handle that
/// wrote it, so the assertion is about what reached DISK.
fn reload(id: &str) -> think_and_ship::roadmap::domain::Chunk {
    engine()
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("chunk '{id}' is not in the reloaded store"))
}

#[test]
fn the_cli_writes_a_blocker_the_engine_reads_back_identically() {
    let id = seeded("cli-writes");
    cli::roadmap_block(
        &id,
        "premise_refuted",
        "  the premise was re-tested and failed  ".into(),
        Some("think:42".into()),
    )
    .expect("block succeeds");

    let b = reload(&id).blocked_by.expect("a blocker reached disk");
    assert_eq!(b.kind.as_wire(), "premise_refuted");
    // Trimmed by the SAME validator the MCP seam calls — the two surfaces
    // agreeing on the stored value is the point, not merely both storing one.
    assert_eq!(b.reason, "the premise was re-tested and failed");
    assert_eq!(b.evidence.as_deref(), Some("think:42"));
    assert!(!b.blocked_at.is_empty(), "a blocker must be stamped");
}

#[tokio::test]
async fn a_blocker_set_by_the_cli_is_cleared_by_the_mcp_tool() {
    // THE AGREEMENT PROOF, and the reason this file exists. One surface writes,
    // the other retracts, and the store afterwards must look like neither ever
    // touched it. If the two disagreed about where a blocker lives or what
    // clearing means, this is the test that could not pass.
    use rmcp::handler::server::wrapper::Parameters;
    use think_and_ship::roadmap::mcp::RoadmapService;

    let id = seeded("cli-writes-mcp-clears");
    cli::roadmap_block(&id, "awaiting_human", "needs a decision".into(), None)
        .expect("block succeeds");
    assert!(
        reload(&id).blocked_by.is_some(),
        "the CLI's write must be on disk before the MCP tool is asked to undo it"
    );

    let svc = RoadmapService::new(engine());
    let args = serde_json::from_value(serde_json::json!({
        "id": id,
        "clear_blocked_by": true,
    }))
    .expect("wire deserializes");
    let out = svc
        .roadmap_update_chunk(Parameters(args))
        .await
        .expect("handler returns")
        .structured_content
        .expect("structured content");
    assert_ne!(out["ok"], false, "the MCP clear reported failure: {out}");

    assert!(
        reload(&id).blocked_by.is_none(),
        "the MCP tool did not clear the blocker the CLI wrote"
    );
}

#[test]
fn the_cli_clear_is_loud_when_there_is_nothing_to_clear() {
    let id = seeded("cli-clears-nothing");
    let err = cli::roadmap_unblock(&id).expect_err("clearing nothing must fail");
    assert!(
        err.to_string().contains("no blocker"),
        "the CLI must say what was missing, got: {err}"
    );
}

#[test]
fn the_cli_refuses_a_kind_outside_the_vocabulary_and_names_all_of_it() {
    // Derived from the vocabulary rather than listed, so a fifth kind is
    // covered here without anyone remembering to come back.
    use think_and_ship::roadmap::domain::BlockerKind;

    let id = seeded("cli-bad-kind");
    let err = cli::roadmap_block(&id, "vibes", "r".into(), None)
        .expect_err("an unknown kind must be refused");
    let msg = err.to_string();
    for kind in BlockerKind::ALL {
        assert!(
            msg.contains(kind.as_wire()),
            "the CLI error must name '{}', got: {msg}",
            kind.as_wire()
        );
    }
    assert!(
        reload(&id).blocked_by.is_none(),
        "a refused kind must write nothing"
    );
}

#[test]
fn the_cli_refuses_a_blocker_with_no_reason() {
    let id = seeded("cli-blank-reason");
    let err = cli::roadmap_block(&id, "external", "   ".into(), None)
        .expect_err("a blank reason must be refused");
    assert!(
        err.to_string().contains("reason"),
        "the refusal must name the missing field, got: {err}"
    );
    assert!(reload(&id).blocked_by.is_none());
}

#[test]
fn command_grammar_reaches_both_verbs() {
    // The trap this closes is the one `windows-binary-distribution` hit: a
    // capability that is fully authored and simply never reachable. Every test
    // above calls the functions directly, so none of them would notice if the
    // subcommands were absent from the grammar or wired to the wrong fields.
    use clap::Parser;
    use think_and_ship::cli::args::{Cli, Command, RoadmapAction};

    let cli = Cli::try_parse_from([
        "think-and-ship",
        "roadmap",
        "block",
        "--id",
        "c",
        "--kind",
        "external",
        "--reason",
        "waiting on a vendor",
        "--evidence",
        "think:42",
    ])
    .expect("`roadmap block` must parse");
    match cli.command {
        Command::Roadmap {
            action:
                RoadmapAction::Block {
                    id,
                    kind,
                    reason,
                    evidence,
                },
        } => {
            assert_eq!(id, "c");
            assert_eq!(kind, "external");
            assert_eq!(reason, "waiting on a vendor");
            assert_eq!(evidence.as_deref(), Some("think:42"));
        }
        other => panic!("`roadmap block` parsed to the wrong command: {other:?}"),
    }

    let cli = Cli::try_parse_from(["think-and-ship", "roadmap", "unblock", "--id", "c"])
        .expect("`roadmap unblock` must parse");
    match cli.command {
        Command::Roadmap {
            action: RoadmapAction::Unblock { id },
        } => assert_eq!(id, "c"),
        other => panic!("`roadmap unblock` parsed to the wrong command: {other:?}"),
    }

    // `--reason` is not optional: a blocker nobody wrote a reason for is the
    // prose-in-the-title problem wearing a struct, and the grammar says so
    // before the engine has to.
    Cli::try_parse_from([
        "think-and-ship",
        "roadmap",
        "block",
        "--id",
        "c",
        "--kind",
        "external",
    ])
    .expect_err("`roadmap block` must require a reason");
}
