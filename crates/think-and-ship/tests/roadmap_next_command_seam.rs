//! `think-and-ship roadmap next` and the engine, asked the same question.
//!
//! # What this file is for
//!
//! `roadmap-next-skips-blocked-by` carries the criterion *the CLI and the
//! agent never disagree about what is next*. Both answers come from
//! `RoadmapEngine::next`, so agreement is structural — but structure is not
//! proof. A refactor that gave the CLI its own filter would leave every
//! engine-level test green while the two surfaces quietly diverged, and the
//! one thing that catches that is asking the CLI and reading its answer.
//!
//! # Why a spawned binary here, and CLI functions in its sibling
//!
//! `tests/roadmap_blocker_command_seam.rs` calls `cli::roadmap_block` and
//! friends directly, and says why: they return a `Result` and mutate a store
//! you can read back, so calling them runs the shipped code path rather than a
//! re-creation of it. `roadmap next` is the opposite shape — its entire
//! product is what it prints, and it returns `()`. There is nothing to read
//! back. So this file runs the real binary and reads its stdout, which as a
//! bonus proves the argv a human types reaches the selection at all.
//!
//! # The table
//!
//! Foreign, and different from the greenhouse table the engine tests use — a
//! port-operations backlog this project has never had. The chunk the CLI must
//! name is DERIVED from it rather than written down, and the premise that
//! makes the derivation meaningful (that the board's top chunk is a blocked
//! one) is asserted before anything else.

use std::process::Command;

use think_and_ship::cli;
use think_and_ship::infra::{Domain, Persistence, PersistenceConfig};
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;

const PROJECT_ID: &str = "next-command-seam";

/// The binary cargo built for this integration test — the same one `cargo
/// install` ships, not a re-creation of its main().
const BIN: &str = env!("CARGO_BIN_EXE_think-and-ship");

/// `(id, priority, blocked)`.
const HARBOUR: &[(&str, u32, bool)] = &[
    ("crane-slews-past-its-limit", 30, false),
    ("tide-gauge-reports-in-feet", 7, true),
    ("berth-lights-fail-at-dusk", 55, false),
    ("pilot-roster-double-books", 18, true),
];

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
/// `cli::load_roadmap_engine` performs, which is what makes it a reader of the
/// very bytes the spawned binary will read.
fn engine() -> RoadmapEngine {
    scratch();
    let project_id = think_and_ship::infra::resolve_project_id(None);
    RoadmapEngine::new(project_id).with_persistence(Persistence::new(
        &PersistenceConfig::from_env(),
        Domain::Roadmap,
    ))
}

/// Seed the board from the table, blocking rows through the shipped
/// `roadmap block` verb rather than by writing the field directly — so the
/// blocker the selection has to notice is one a human could have written.
fn seed(table: &[(&str, u32, bool)]) {
    let mut e = engine();
    for (id, priority, _) in table {
        e.add_chunk(
            (*id).to_string(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            *priority,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .expect("chunk seeded");
    }
    for (id, _, blocked) in table.iter().filter(|(_, _, b)| *b) {
        assert!(blocked);
        cli::roadmap_block(id, "external", format!("{id} waits on somebody else"), None)
            .expect("the blocker is written by the shipped verb");
    }
}

/// What a scheduler blind to blockers would answer, and what a seeing one
/// must. Both derived; neither named.
fn blind_and_seeing<'a>(table: &'a [(&'a str, u32, bool)]) -> (&'a str, &'a str) {
    let blind = table
        .iter()
        .min_by_key(|(_, priority, _)| *priority)
        .expect("a non-empty table");
    let seeing = table
        .iter()
        .filter(|(_, _, blocked)| !blocked)
        .min_by_key(|(_, priority, _)| *priority)
        .expect("a table with somewhere to go");
    assert!(
        blind.2,
        "the highest-priority chunk in the table must be a blocked one, or \
         this file cannot tell a seeing CLI from a blind one; got '{}'",
        blind.0
    );
    assert_ne!(blind.0, seeing.0, "the two answers must be distinguishable");
    (blind.0, seeing.0)
}

fn roadmap_next_stdout() -> String {
    let out = Command::new(BIN)
        .args(["roadmap", "next"])
        .env("THINK_AND_SHIP_DATA_DIR", scratch())
        .env("THINK_AND_SHIP_PERSIST", "true")
        .env("THINK_AND_SHIP_PROJECT_NAME", PROJECT_ID)
        .output()
        .expect("the built binary runs");
    assert!(
        out.status.success(),
        "`roadmap next` exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_cli_and_the_engine_name_the_same_chunk_and_it_is_not_the_blocked_one() {
    seed(HARBOUR);
    let (blind, seeing) = blind_and_seeing(HARBOUR);

    // The engine's answer, on the same store the binary is about to read.
    assert_eq!(
        engine().next().map(|c| c.id.clone()).as_deref(),
        Some(seeing),
        "the engine must step over the blockers above '{seeing}'"
    );

    let stdout = roadmap_next_stdout();
    let named = stdout
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();

    assert_eq!(
        named, seeing,
        "the CLI must hand out the chunk the engine chose, not its own \
         reading of the board; full output:\n{stdout}"
    );
    assert!(
        !stdout.contains(blind),
        "the CLI offered '{blind}', which carries a blocker and outranks \
         everything takeable; full output:\n{stdout}"
    );

    // Second half, in the same test rather than a second one: these tests share
    // a store, and blocking the rest of the board is a mutation that would
    // race a sibling running in parallel. Sequential here, deterministic.
    //
    // An empty answer has two causes now, and naming the wrong one sends a
    // reader hunting a dependency that is not the problem.
    for (id, _, blocked) in HARBOUR.iter().filter(|(_, _, b)| !b) {
        assert!(!blocked);
        cli::roadmap_block(id, "awaiting_human", format!("{id} needs a person"), None)
            .expect("the blocker is written by the shipped verb");
    }
    assert_eq!(
        engine().next(),
        None,
        "with every chunk blocked there is nothing to hand out"
    );

    let empty = roadmap_next_stdout();
    assert!(
        empty.contains("blocker"),
        "an empty answer must say a blocker held the board back, not blame \
         dependencies that are all satisfied; got:\n{empty}"
    );
    assert!(
        !empty.contains("dependencies"),
        "the dependency wording is the wrong reason here and would send a \
         reader looking in the wrong place; got:\n{empty}"
    );
}
