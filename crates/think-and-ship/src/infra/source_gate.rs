//! Shared machinery for STRUCTURAL gates — tests that assert over the shape of
//! the source rather than the behaviour of the program.
//!
//! # Why this module exists at all
//!
//! Most defects change a value, and an assertion can see a changed value. Some
//! defects change only a PROBABILITY: delete a readiness wait at a call site,
//! or invert two statements whose order is the whole correctness argument, and
//! nothing returns a different answer — the program merely starts losing a race
//! it used to win. Measured instances in this codebase: a broadcast client that
//! lost frames 3 times in 300 idle and 279 times in 300 contended, and a cloud
//! push whose wait, when deleted, cost 18 assertions out of 120.
//!
//! Those are green on a developer's laptop and red only on a loaded CI runner,
//! which makes them worse than an untested unit — an untested unit at least
//! fails where you can watch it. A behavioural test cannot defend them, so the
//! defence has to be structural: assert that the call site still calls, or that
//! the two statements still appear in that order.
//!
//! # What a structural gate is honestly worth
//!
//! It proves a SHAPE, not a MEANING. That a wait is textually present does not
//! prove it waits for the right thing, and that two lines appear in order does
//! not prove no future refactor reintroduces the gap by other means. Claiming
//! more than that is how a gate starts failing open. Every gate built on this
//! module should say so in its own doc.
//!
//! # Two traps these helpers exist to close
//!
//! 1. **A gate that scans the file it lives in will count its own needles.**
//!    The first structural gate written here reported 6 sites against an
//!    expected 5 for exactly that reason. Build every needle with `concat!` so
//!    the gate's own source cannot satisfy it, and PIN the expected count —
//!    only the pin caught it.
//! 2. **A gate that cannot read its input covers nothing, silently.** Rename a
//!    scanned file and a naive gate happily checks the three that remain and
//!    passes. [`read_window`] panics instead.

#![cfg(test)]

use std::path::Path;

/// Read one file of a gate's window, or panic loudly.
///
/// The failure this closes is a gate that quietly narrows: a renamed or moved
/// file makes the gate cover less than it claims while still reporting green.
/// A gate that cannot read its input covers nothing, and should say so rather
/// than pass.
pub fn read_window(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "gate window unreadable: {} ({e}) — a source gate that cannot read \
             its input covers nothing",
            path.display(),
        )
    })
}

/// The name declared by a `fn` line, if the line declares one.
///
/// Deliberately textual: it strips the visibility and `async` prefixes this
/// codebase actually uses and takes the identifier up to `(`, `<` or a space.
/// It is not a parser and does not pretend to be one.
pub fn fn_name_of(line: &str) -> Option<&str> {
    let mut t = line.trim_start();
    for prefix in ["pub(crate) ", "pub ", "async "] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    // `async` can follow `pub`, so allow one more pass.
    t = t.strip_prefix("async ").unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    Some(rest.split(['(', '<', ' ']).next().unwrap_or(rest))
}

/// Split a source file into per-function blocks, keyed by name. Everything
/// before the first `fn` lands under `<file prologue>`.
///
/// Per-FUNCTION splitting is the point, not a convenience. A whole-file scan
/// lets a wait in a neighbouring test vouch for a call site in this one, which
/// is precisely the vouching a reachability gate exists to refuse.
pub fn fn_blocks(src: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut name = String::from("<file prologue>");
    let mut body = String::new();
    for line in src.lines() {
        if let Some(next) = fn_name_of(line) {
            blocks.push((std::mem::take(&mut name), std::mem::take(&mut body)));
            name = next.to_string();
        }
        body.push_str(line);
        body.push('\n');
    }
    blocks.push((name, body));
    blocks
}

/// A live line: not a comment, not a doc comment.
///
/// Without this, a gate is satisfied by the very comment that says the gate is
/// needed — and "a comment is not a gate" is the finding that produced this
/// module.
pub fn live(l: &&str) -> bool {
    !l.trim_start().starts_with("//")
}

/// Count the live lines of `block` containing `needle`.
pub fn count_live(block: &str, needle: &str) -> usize {
    block
        .lines()
        .filter(live)
        .filter(|l| l.contains(needle))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The splitter is load-bearing for every gate built on it, so it is itself
    /// tested rather than trusted.
    #[test]
    fn blocks_are_split_per_function_and_named() {
        let src = "\
use std::fmt;

fn alpha() {
    connect();
}

pub async fn beta(x: u32) -> bool {
    wait();
}
";
        let blocks = fn_blocks(src);
        let names: Vec<_> = blocks.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["<file prologue>", "alpha", "beta"]);

        // The whole reason for splitting: `beta`'s wait must not be visible
        // from `alpha`'s block.
        let alpha = &blocks[1].1;
        assert_eq!(count_live(alpha, "wait"), 0, "alpha borrowed beta's wait");
        assert_eq!(count_live(alpha, "connect"), 1);
    }

    #[test]
    fn commented_out_lines_are_not_live() {
        let block = "    // connect();\n    /// connect();\n    connect();\n";
        assert_eq!(
            count_live(block, "connect"),
            1,
            "a commented-out call satisfied a gate — a comment is not a gate",
        );
    }

    #[test]
    fn an_unreadable_window_panics_rather_than_covering_nothing() {
        let missing = Path::new("/nonexistent/gate/window.rs");
        let err = std::panic::catch_unwind(|| read_window(missing))
            .expect_err("reading a missing window must panic, not return empty");
        let msg = err
            .downcast_ref::<String>()
            .map_or_else(String::new, Clone::clone);
        assert!(
            msg.contains("covers nothing"),
            "the panic must say why an unreadable window is fatal, got {msg:?}",
        );
    }
}
