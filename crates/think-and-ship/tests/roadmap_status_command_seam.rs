//! `think-and-ship roadmap status` — the blocker tally, on the surface a
//! person actually reads.
//!
//! # What this file is for
//!
//! `blocked-by-counted-not-buried` carries the criterion *a reader can tell a
//! disproven chunk from an undecided one*. The engine computes the tally and
//! the agent surface receives it in `counts.blocked_by` — but a count nobody at
//! a terminal can see is not a count they have. The CLI renders `status()` into
//! its own text, so it is free to compute the tally correctly and print none of
//! it, and every engine-level test would stay green while it did.
//!
//! That is not hypothetical here: its sibling `roadmap_next_command_seam.rs`
//! exists because giving `cli::roadmap_next` its own filter once left all 47
//! engine tests green while the CLI handed out a blocked chunk. Selection and
//! rendering fail the same way, so they are gated the same way — by running the
//! real binary and reading what it printed.
//!
//! # Why a spawned binary
//!
//! Same reason as its sibling: `roadmap status` returns `()` and its entire
//! product is stdout, so there is nothing to read back from a store. Spawning
//! the shipped binary also proves the argv a human types reaches the render.
//!
//! # The table
//!
//! Foreign, and a third domain — a rail-depot backlog, distinct from the
//! greenhouse table the engine tests use and the harbour one next door. Every
//! expected number is DERIVED from it; none is written down.

use std::process::Command;

use think_and_ship::cli;
use think_and_ship::infra::{Domain, Persistence, PersistenceConfig};
use think_and_ship::roadmap::domain::{BlockerKind, ChunkStatus};
use think_and_ship::roadmap::engine::RoadmapEngine;

const PROJECT_ID: &str = "status-command-seam";

/// The binary cargo built for this integration test.
const BIN: &str = env!("CARGO_BIN_EXE_think-and-ship");

/// `(id, priority, kind)` — a rail depot's backlog. `external` is deliberately
/// unused, so the render can be checked for the difference between a kind that
/// counted zero and a kind that was silently dropped.
const DEPOT: &[(&str, u32, Option<BlockerKind>)] = &[
    (
        "turntable-drifts-off-centre",
        30,
        Some(BlockerKind::PremiseRefuted),
    ),
    (
        "shed-doors-ice-up-below-freezing",
        12,
        Some(BlockerKind::AwaitingHuman),
    ),
    (
        "sanding-gear-clogs-on-wet-days",
        45,
        Some(BlockerKind::AwaitingHuman),
    ),
    (
        "wheel-lathe-booking-clashes",
        20,
        Some(BlockerKind::PremiseUnmet),
    ),
    ("coach-cleaning-runs-over-nightly", 55, None),
];

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
/// `cli::load_roadmap_engine` performs, so it reads the very bytes the spawned
/// binary will read.
fn engine() -> RoadmapEngine {
    scratch();
    let project_id = think_and_ship::infra::resolve_project_id(None);
    RoadmapEngine::new(project_id).with_persistence(Persistence::new(
        &PersistenceConfig::from_env(),
        Domain::Roadmap,
    ))
}

/// Seed the board, writing blockers through the shipped `roadmap block` verb so
/// what the render has to notice is what a human could have typed.
fn seed() {
    let mut e = engine();
    for (id, priority, _) in DEPOT {
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
    for (id, _, kind) in DEPOT {
        if let Some(k) = kind {
            cli::roadmap_block(id, k.as_wire(), format!("{id} is held up"), None)
                .expect("the blocker is written by the shipped verb");
        }
    }
}

fn roadmap_status_stdout() -> String {
    let out = Command::new(BIN)
        .args(["roadmap", "status"])
        .env("THINK_AND_SHIP_DATA_DIR", scratch())
        .env("THINK_AND_SHIP_PERSIST", "true")
        .env("THINK_AND_SHIP_PROJECT_NAME", PROJECT_ID)
        .output()
        .expect("the built binary runs");
    assert!(
        out.status.success(),
        "`roadmap status` exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// How many rows of the table carry each kind — derived, so the assertions
/// below cannot be satisfied by a render that hardcodes this board.
fn expected(kind: BlockerKind) -> usize {
    DEPOT.iter().filter(|(_, _, k)| *k == Some(kind)).count()
}

#[test]
fn the_printed_status_says_how_much_of_the_board_is_stuck_and_on_what() {
    seed();

    let blocked_rows = DEPOT.iter().filter(|(_, _, k)| k.is_some()).count();

    // The premises, before anything about the output. Two ways this could pass
    // while proving nothing: if every kind had the same tally, a render that
    // printed one number four times would satisfy it; and if no kind were
    // unused, "a kind that counted zero is omitted" could never be told apart
    // from "a kind that was dropped".
    let mut tallies: Vec<usize> = BlockerKind::ALL.iter().map(|k| expected(*k)).collect();
    let unused: Vec<BlockerKind> = BlockerKind::ALL
        .into_iter()
        .filter(|k| expected(*k) == 0)
        .collect();
    assert!(
        !unused.is_empty(),
        "the table must leave a kind unused, or the omission below is untested"
    );
    tallies.sort_unstable();
    tallies.dedup();
    assert!(
        tallies.len() >= 3,
        "the kinds must have distinguishable tallies, or one number printed \
         for all of them would pass: {tallies:?}"
    );

    let stdout = roadmap_status_stdout();

    let line = stdout
        .lines()
        .find(|l| l.contains("blocked by"))
        .unwrap_or_else(|| panic!("no blocker line was printed at all:\n{stdout}"));

    // The total, asserted ON THE LINE rather than anywhere in the output: the
    // status run above is full of small numbers, and a bare `contains` against
    // the whole of stdout would match one of them by coincidence.
    assert!(
        line.contains(&format!("{blocked_rows}")),
        "the blocker line must say how many chunks are blocked by something \
         that is not a dependency: {line}"
    );

    // And the breakdown, kind by kind, derived from the table.
    for kind in BlockerKind::ALL {
        let n = expected(kind);
        if n == 0 {
            assert!(
                !line.contains(kind.as_wire()),
                "'{}' has no chunks and must not be printed — a render that \
                 lists every kind at zero buries the ones that matter: {line}",
                kind.as_wire()
            );
            continue;
        }
        assert!(
            line.contains(&format!("{} {n}", kind.as_wire())),
            "'{}' must be printed with its count of {n}, or a reader cannot \
             tell a disproven chunk from one waiting on a person: {line}",
            kind.as_wire()
        );
    }

    // Cross-cutting, not a bucket taken from the others: every blocked chunk is
    // still counted as pending in the run above.
    let pending = DEPOT.len();
    assert!(
        stdout.contains(&format!("pending {pending}")),
        "a blocked chunk keeps its status — nothing was moved out of `pending` \
         to earn a place in the tally; got:\n{stdout}"
    );
}
