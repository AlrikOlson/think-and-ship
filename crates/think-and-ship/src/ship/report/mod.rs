//! Structured test-report parsing for `ship_check` — the format registry and
//! the degradation contract.
//!
//! # What this module is for
//!
//! A `ship_check` recorded with a `command` keeps the real exit code, and the
//! exit code stays the single source of truth for `verified`. That one bit is
//! honest but blind: an agent handed "failed" cannot see WHICH test failed
//! without re-running the whole suite, a flaky test is indistinguishable from
//! a solid one, and a red gate cannot be tied to a specific assertion.
//!
//! This module adds the missing detail WITHOUT adding a way to lie. The caller
//! asks the runner for a machine-readable report file (`report: {format,
//! path}`), the server reads that file after the command has run, and the
//! parsed summary is stored NEXT TO the exit code — never in place of it.
//!
//! # The degradation contract, in priority order
//!
//! 1. The exit code remains the source of truth for `passed`/`verified`.
//!    Nothing parsed here can flip either.
//! 2. Any report problem — unknown format, missing file, oversized file,
//!    malformed content — degrades to exactly the pre-report behaviour: the
//!    check records that parsing did not happen ([`ReportRecord::parsed`] =
//!    `false` plus an `error`) and carries on. A report failure NEVER fails
//!    the check.
//! 3. No scraping of human-readable stdout, ever. The runner must be asked
//!    for a machine-readable report and this module reads that file. Regexing
//!    console output silently rots when a tool reformats; a named report file
//!    keeps the maintenance bounded.
//!
//! # The registry
//!
//! One adapter per format, registered the way [`crate::tracker::registry`]
//! registers providers: an explicit table each adapter's own file declares its
//! entry in, so "add a format" is one line here and the refusal for an unknown
//! format renders its list from the same table the lookup walks. The only
//! registered format is `junit` — JUnit XML is the baseline because almost
//! every runner in every language can be asked to emit it (verified against
//! cargo-nextest's JUnit support page, <https://nexte.st/docs/machine-readable/junit/>,
//! and pytest's `--junit-xml` flag docs, both read 2026-08-15). Native formats
//! (`go test -json`, Jest `--json`, dotnet `trx`) go behind this same
//! interface when they are verified per the `docs/HARNESSES.md` rule.
//!
//! # What is stored, and what is deliberately not
//!
//! The summary counts always; per-test detail only for FAILURES, bounded to
//! [`MAX_FAILURES`] entries of [`FAILURE_MESSAGE_CHARS`] chars each. Failure
//! messages routinely interpolate env values and fixture data, so every
//! stored message passes through [`crate::otel_logs::redact`] before it
//! reaches the record. A run where everything passed keeps just the counts.

pub mod junit;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Max failing tests whose name/message are retained on the record. A large
/// suite is thousands of test records per run and the local ship store only
/// holds the current cycle — the summary is stored, the long tail is not.
pub const MAX_FAILURES: usize = 20;

/// Max chars of one failure message retained (after redaction).
pub const FAILURE_MESSAGE_CHARS: usize = 400;

/// Refuse report files larger than this. A report is a summary artifact; a
/// multi-hundred-MB file is either the wrong file or an attack on memory.
pub const MAX_REPORT_BYTES: u64 = 10 * 1024 * 1024;

// Stored on the check so the record says explicitly whether parsing
// happened, per the degradation contract in the module doc. Only the
// caller-facing lines are `///` — they ride the outputSchema wire.
/// What happened with the requested report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct ReportRecord {
    /// The format key as requested.
    pub format: String,
    /// The report file path as requested.
    pub path: String,
    /// True when the file was read and parsed.
    pub parsed: bool,
    /// Why parsing did not happen, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// Additive and separately fallible: it never decides `passed` or `verified`.
/// The summary parsed from a machine-readable report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct TestResults {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    /// Runner-reported duration, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    // Bounded to [`MAX_FAILURES`] entries; messages are redacted and
    // truncated to [`FAILURE_MESSAGE_CHARS`] before storage.
    /// Failing tests (bounded; messages redacted + truncated).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<TestFailure>,
}

// The addressable thing a red gate can finally point at.
/// One failing test.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct TestFailure {
    /// `classname::name` when the report carries both, else the bare name.
    pub name: String,
    /// The failure message; absent when the report carried none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One format's entry in the registry, declared in that format's own file.
pub struct FormatRegistration {
    /// The string an agent passes as `report.format` and the string stored on
    /// the record.
    pub key: &'static str,
    /// Parse the report file's CONTENT into a summary. Pure: file access is
    /// the registry's job so every adapter shares the same missing-file and
    /// size-cap behaviour.
    pub parse: fn(&str) -> Result<TestResults, String>,
}

/// The one place a report format is registered. The module doc's "the only
/// registered format is `junit`" sentence is gated by a test that reads this
/// table rather than trusting the sentence.
pub const FORMATS: &[&FormatRegistration] = &[&junit::REGISTRATION];

/// The supported-format list, rendered from the table the lookup walks so the
/// refusal message can never drift from what is registered.
#[must_use]
pub fn known_list() -> String {
    FORMATS.iter().map(|f| f.key).collect::<Vec<_>>().join(", ")
}

/// Read and parse a requested report. Total: every failure mode returns a
/// `ReportRecord` with `parsed: false` and an `error` — this function cannot
/// fail the check that called it.
#[must_use]
pub fn read_report(format: &str, path: &str) -> (ReportRecord, Option<TestResults>) {
    let record = |parsed: bool, error: Option<String>| ReportRecord {
        format: format.to_string(),
        path: path.to_string(),
        parsed,
        error,
    };

    let Some(registration) = FORMATS.iter().find(|f| f.key == format) else {
        return (
            record(
                false,
                Some(format!(
                    "unknown report format '{format}' — supported: {}",
                    known_list()
                )),
            ),
            None,
        );
    };

    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_REPORT_BYTES => {
            return (
                record(
                    false,
                    Some(format!(
                        "report file is {} bytes; refusing to read more than {MAX_REPORT_BYTES}",
                        meta.len()
                    )),
                ),
                None,
            );
        }
        Ok(_) => {}
        Err(e) => {
            return (
                record(false, Some(format!("report file not found: {path} ({e})"))),
                None,
            );
        }
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return (
                record(false, Some(format!("could not read report file: {e}"))),
                None,
            );
        }
    };

    match (registration.parse)(&content) {
        Ok(results) => (record(true, None), Some(results)),
        Err(e) => (
            record(false, Some(format!("report did not parse: {e}"))),
            None,
        ),
    }
}

/// Redact then truncate one failure message. Redaction FIRST: truncation can
/// otherwise cut a token in half and leave the readable half in the record.
#[must_use]
pub(crate) fn scrub_message(raw: &str) -> String {
    let redacted = crate::otel_logs::redact(raw.trim());
    let n = redacted.chars().count();
    if n <= FAILURE_MESSAGE_CHARS {
        return redacted;
    }
    let head: String = redacted.chars().take(FAILURE_MESSAGE_CHARS).collect();
    format!("{head}…(truncated {} chars)", n - FAILURE_MESSAGE_CHARS)
}
