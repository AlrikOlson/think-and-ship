//! The boundary between what we PRINT and what we SEND.
//!
//! The OTLP log lane shipped in `e533fd1` forwards `tracing` events to a
//! backend. An `eprintln!` never reaches a `tracing` subscriber, so the spelling
//! of a diagnostic decides whether it stays on this machine or leaves it. That
//! makes the split load-bearing in a way it never was before, and these tests
//! are the only thing holding it.
//!
//! # The rule, stated once so it is checkable instead of tasteful
//!
//! > Convert an `eprintln!` to `tracing::warn!` **if and only if** it is a
//! > FAILURE REPORT: it is emitted on a branch reached because one of *our own*
//! > side effects did not happen, or because an input was refused. Everything
//! > else stays `eprintln!`.
//!
//! It is a rule about what the line REPORTS, not about where the line LIVES. A
//! module-path rule ("everything outside `src/cli/` is operational") was tried
//! first and is simply false here — see below.
//!
//! # The three populations — one more than the obvious split
//!
//! **(A) Failure reports.** `"Failed to persist session {id}"`, `"could not
//! create data dir"`, `"Refusing to persist session with unsafe id"`,
//! `"Skipping {} — schema version"`. 34 sites. These are what the log lane
//! exists for, and they are now `tracing::warn!`.
//!
//! **(B) Success narration.** `"🌿 Created branch"`, `"📐 estimated_total
//! revised"`, `"🔄 Deliberation history cleared"`, `"loaded {n} signal(s) from
//! disk"`, and `process.rs`'s `eprintln!("{formatted}")` — the whole rendered
//! step. These sit in the SAME engine modules as (A), interleaved line by line,
//! which is what kills the module-path rule.
//!
//! **(C) CLI output for a human.** `src/cli/`, `src/main.rs`.
//!
//! # Why (B) stays `eprintln!` — the trap that is specific to this crate
//!
//! Narration is INFO-shaped, so the obvious move is `tracing::info!`. That
//! would DELETE it twice. `init_tracing` installs
//! `EnvFilter::try_from_default_env().unwrap_or(EnvFilter::new("warn"))`, so an
//! `info!` does not reach stderr unless the operator set `RUST_LOG`; and
//! [`passes_fence`] admits WARN and above, so it does not reach the backend
//! either. A line that prints unconditionally today would print nowhere at all
//! — the operator loses the diagnostic they had and gains nothing. Promoting it
//! to `warn!` instead would lie about severity and fill a warning stream with
//! `"loaded 3 signal(s) from disk"`. Both options are worse than leaving it, so
//! narration keeps its `eprintln!` and `narration_is_never_demoted_…` holds
//! that decision down.
//!
//! # Why there is no "src/cli contains no tracing::warn!" test
//!
//! Because it would have been red before this conversion touched anything.
//! `cli/mod.rs` already carried four `tracing::warn!` calls — a failed v0.1.x
//! migration and three `Err(e) =>` arms — beside its 26 `eprintln!` calls. The
//! rule above is not imposed by this conversion; it is the convention the one
//! revisited module already followed, and the engines simply predate it. So the
//! CLI side is pinned by a FLOOR on its `eprintln!` count instead: conversion
//! only ever removes an `eprintln!`, so a floor goes red the moment user output
//! is converted, while leaving new CLI output free to be added.
//!
//! [`passes_fence`]: think_and_ship::otel_logs::passes_fence

use std::path::{Path, PathBuf};

fn src(rel: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Lines that are genuinely a `macro!(` CALL, not a mention inside a string or
/// a doc comment. Matching WITH the leading indentation is the whole point: a
/// source-text gate that also matches prose fails open on the case it exists to
/// catch.
fn call_lines(text: &str, macro_name: &str) -> Vec<usize> {
    let call = format!("{macro_name}!");
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            t.starts_with(&call) || t.contains(&format!("=> {call}"))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Every module that holds the operational population. `src/cli/` and
/// `src/main.rs` are deliberately absent: they are population (C).
const OPERATIONAL: &[&str] = &[
    "infra/persistence.rs",
    "think/persistence.rs",
    "think/config.rs",
    "think/engine/revisions.rs",
    "think/engine/validation.rs",
    "think/engine/branching.rs",
    "think/engine/sessions.rs",
    "think/engine/process.rs",
    "think/engine/mutations.rs",
    "think/engine/numbering.rs",
    "ship/persistence.rs",
    "ship/engine/mod.rs",
    "roadmap/engine/mod.rs",
    "signal/engine.rs",
    "mcp/elicit.rs",
    "mcp/tasks.rs",
];

/// Words that only appear when a line is reporting that something did not work.
/// Chosen from the actual converted set, not invented.
const FAILURE_MARKERS: &[&str] = &[
    "Failed to",
    "failed:",
    "failed to",
    "could not",
    "Cannot ",
    "Refusing",
    "Skipping",
    "Invalid ",
    "Ignoring",
];

/// The completeness half: no failure report is left behind on `eprintln!`,
/// where the log lane is structurally blind to it.
#[test]
fn every_failure_report_outside_the_cli_reaches_the_subscriber() {
    let mut stranded = Vec::new();
    for rel in OPERATIONAL {
        let text = src(rel);
        let lines: Vec<&str> = text.lines().collect();
        for i in call_lines(&text, "eprintln") {
            // A macro call can span lines; the format string is within the next
            // few of them.
            let site = lines[i..(i + 4).min(lines.len())].join(" ");
            if let Some(marker) = FAILURE_MARKERS.iter().find(|m| site.contains(**m)) {
                stranded.push(format!("{rel}:{} (matched {marker:?})", i + 1));
            }
        }
    }
    assert!(
        stranded.is_empty(),
        "these sites report a failure but still use eprintln!, so the OTLP log \
         lane cannot see them — convert them to tracing::warn! or, if one is \
         genuinely not a failure, reword it:\n  {}",
        stranded.join("\n  ")
    );
}

/// The containment half: user-facing output is never quietly turned into
/// something that leaves the machine. A FLOOR, not an equality — see the module
/// docs for why.
#[test]
fn user_facing_cli_output_is_never_converted_to_a_log() {
    // Measured at the close of `otel-eprintln-to-tracing`. Raising these when
    // CLI output is ADDED is expected and harmless; lowering one means a line a
    // human reads at a terminal is now being POSTed to a backend.
    for (rel, floor) in [("cli/mod.rs", 26), ("cli/connect.rs", 9), ("main.rs", 1)] {
        let found = call_lines(&src(rel), "eprintln").len();
        assert!(
            found >= floor,
            "{rel} has {found} eprintln! call(s), below the floor of {floor}. \
             Converting CLI output to tracing means it is sent to whatever OTLP \
             endpoint the operator configured. If a line really was operational, \
             lower the floor in the same commit and say so."
        );
    }
}

/// The trap half: narration must not be demoted to a level that BOTH the
/// default `EnvFilter("warn")` and the OTLP fence discard. Named messages
/// rather than a blanket ban, so ordinary `debug!` logging stays available.
#[test]
fn narration_is_never_demoted_to_a_level_the_default_filter_deletes() {
    for (rel, needle) in [
        ("think/engine/branching.rs", "Created branch"),
        ("think/engine/mutations.rs", "Deliberation history cleared"),
        ("think/engine/mutations.rs", "estimated_total revised"),
        ("think/engine/sessions.rs", "expired and removed"),
        ("think/engine/process.rs", "History trimmed to"),
        ("think/engine/numbering.rs", "Renumbered"),
        ("roadmap/engine/mod.rs", "loaded roadmap with"),
        ("signal/engine.rs", "signal(s) from disk"),
        ("ship/engine/mod.rs", "task(s) from disk"),
        ("think/persistence.rs", "from the legacy _default history"),
    ] {
        let text = src(rel);
        let lines: Vec<&str> = text.lines().collect();
        // Skip comments: `numbering.rs` documents "Renumbered clones" in a doc
        // comment 300 lines above the call that actually emits it, and matching
        // the prose made this test red for the wrong reason.
        let at = lines
            .iter()
            .position(|l| l.contains(needle) && !l.trim_start().starts_with("//"))
            .unwrap_or_else(|| panic!("{rel} no longer contains {needle:?} outside a comment"));
        // The macro is on this line or just above it (multi-line call).
        let site = lines[at.saturating_sub(2)..=at].join(" ");
        assert!(
            site.contains("eprintln!"),
            "{rel} emits {needle:?} through something other than eprintln!. If it \
             became info!/debug! it now prints NOWHERE: below EnvFilter(\"warn\") \
             on stderr and below the WARN+ fence for OTLP. If it became warn! it \
             is being reported to the operator as a problem."
        );
    }
}

/// The behavioural half, so the whole file is not source text: the target a
/// converted site emits under really does clear the fence, and the level a
/// narration site would have used really does not.
#[test]
fn the_fence_admits_a_converted_site_and_rejects_the_level_narration_would_use() {
    use think_and_ship::otel_logs::passes_fence;
    use tracing::Level;

    // `tracing`'s default target is the module path, so a warn! inside the crate
    // arrives as `think_and_ship::…` with no explicit target argument needed.
    for target in [
        "think_and_ship::think::persistence",
        "think_and_ship::ship::persistence",
        "think_and_ship::signal::engine",
        "think_and_ship::mcp::elicit",
    ] {
        assert!(
            passes_fence(target, &Level::WARN),
            "{target} at WARN must reach the lane — every converted site is WARN"
        );
        assert!(
            !passes_fence(target, &Level::INFO),
            "{target} at INFO must NOT reach the lane; this is exactly why the \
             narration population was left on eprintln!"
        );
    }
    assert!(
        !passes_fence("hyper::client", &Level::WARN),
        "the fence must stay closed to dependencies' warnings"
    );
}
