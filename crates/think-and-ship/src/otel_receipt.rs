//! When this project's trace was last actually SENT, and to where.
//!
//! # The two absences this closes
//!
//! Both were observed in a real Jaeger rather than reasoned about.
//!
//! FIRST, the hole in the middle of the tree. Adopting a caller's
//! context does not only change the export — from that moment every outbound
//! tracker and cloud request carries a `traceparent` naming
//! `span_id("workspace", project)` as its parent, and that span exists only if
//! someone later runs `think-and-ship otel send`. Between those two events the
//! system publishes references to a span that is not there. Read back from the
//! backend, the downstream leg DETACHES INTO A SECOND ROOT: two disconnected
//! fragments where there was one causal chain. Nothing fails — 200 from the
//! collector, a rendered UI, and a per-span warning about clock-skew math
//! rather than about the severed tree.
//!
//! SECOND, silent duplication. Span ids are sha256-deterministic
//! and OTLP has no upsert, so a collector APPENDS. Running `otel send` twice
//! puts every span in the backend twice; three runs of a 587-span export read
//! back as 1759 spans and four roots. The operator sees their root repeated
//! with nothing anywhere explaining why.
//!
//! One datum answers both: WHEN was an export last sent, and WHERE TO. Keyed by
//! endpoint, because "already in your local Jaeger" and "already in Honeycomb"
//! are different answers to the same question.
//!
//! # Why live emission did not make this unnecessary
//!
//! The live lane ([`crate::otel_live`]) does publish the workspace span itself,
//! which closes the first hole — for operators who opted in. It is OFF BY
//! DEFAULT on purpose: an MCP server nobody configured must not open network
//! connections. So the offline lane is the only lane on an unconfigured
//! machine, the hole is the DEFAULT state, and there is now a second way to get
//! this wrong — "did I configure live emission, or am I relying on running a
//! command?".
//!
//! # Output only
//!
//! The shape is copied from [`crate::tracker::receipt`], including its central
//! argument: a receipt is only ever an OUTPUT, read by humans. NOTHING branches
//! on it. That is exactly why it can be reported honestly — a stale, missing or
//! corrupt stamp can only ever cost a pessimistic line of output, never a
//! changed decision. And the absent receipt is the interesting one: "this has
//! never been sent anywhere" is the single most useful thing this file can say.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What one successful `otel send` did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReceipt {
    /// RFC-3339 instant the POST succeeded.
    pub at: String,
    /// How many spans went.
    pub spans: usize,
    /// The trace id they carried — the caller's when joined, ours otherwise.
    /// Recorded so a report can say whether the sent export was the joined one.
    #[serde(default)]
    pub trace_id: String,
}

/// Every endpoint this project's trace has been sent to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendReceipts {
    #[serde(default)]
    pub by_endpoint: BTreeMap<String, SendReceipt>,
}

impl SendReceipts {
    /// The last successful send to `endpoint`, or `None` when there has never
    /// been one. The `None` is load-bearing — see the module docs.
    #[must_use]
    pub fn last(&self, endpoint: &str) -> Option<&SendReceipt> {
        self.by_endpoint.get(endpoint)
    }

    /// Has this project's trace ever been sent ANYWHERE?
    ///
    /// Distinct from [`Self::last`] on purpose: the "your caller's tree has a
    /// second root" warning is about the trace existing in *some* backend, not
    /// about the endpoint you happen to be pointed at right now.
    #[must_use]
    pub fn ever_sent(&self) -> Option<(&String, &SendReceipt)> {
        self.by_endpoint.iter().max_by(|a, b| a.1.at.cmp(&b.1.at))
    }
}

fn receipt_path(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir
        .join("otel")
        .join("receipts")
        .join(format!("{project_id}.json"))
}

/// Read a project's send receipts. Missing or corrupt reads as empty, which
/// degrades the report to "never sent" rather than to a crash.
#[must_use]
pub fn load(data_dir: &Path, project_id: &str) -> SendReceipts {
    std::fs::read_to_string(receipt_path(data_dir, project_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Record one successful send.
///
/// The merge keeps whichever receipt is LATER, compared as instants rather than
/// bytes, so two processes sending the same project concurrently cannot move
/// the stamp backwards and make a sent trace look unsent.
pub fn record(
    data_dir: &Path,
    project_id: &str,
    endpoint: &str,
    receipt: &SendReceipt,
) -> std::io::Result<()> {
    let mut state = load(data_dir, project_id);
    state
        .by_endpoint
        .insert(endpoint.to_string(), receipt.clone());
    crate::infra::persistence::locked_merge_write(
        &receipt_path(data_dir, project_id),
        &state,
        |ours: &SendReceipts, disk: SendReceipts| {
            let mut merged = disk;
            for (endpoint, ours_receipt) in &ours.by_endpoint {
                let keep = match merged.by_endpoint.get(endpoint) {
                    Some(theirs)
                        if !crate::roadmap::domain::rfc3339_newer(&ours_receipt.at, &theirs.at) =>
                    {
                        theirs.clone()
                    }
                    _ => ours_receipt.clone(),
                };
                merged.by_endpoint.insert(endpoint.clone(), keep);
            }
            merged
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(at: &str, spans: usize) -> SendReceipt {
        SendReceipt {
            at: at.into(),
            spans,
            trace_id: "t".into(),
        }
    }

    /// The absent receipt is the interesting one: a project that has never sent
    /// must read as never-sent, not as an error.
    #[test]
    fn a_project_that_never_sent_reports_nothing_rather_than_a_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = load(dir.path(), "proj");
        assert!(state.last("http://localhost:4318/v1/traces").is_none());
        assert!(state.ever_sent().is_none());
    }

    /// A corrupt file must degrade to "never sent" rather than crash the whole
    /// `otel status` report — nothing branches on this value, so pessimism is
    /// free and a panic is not.
    #[test]
    fn a_corrupt_file_reads_as_never_sent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = receipt_path(dir.path(), "proj");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{{{ not json").expect("write");
        assert!(load(dir.path(), "proj").ever_sent().is_none());
    }

    /// Keyed by ENDPOINT, because "already in your local Jaeger" and "already
    /// in Honeycomb" are different answers to the duplicate-span question.
    #[test]
    fn endpoints_are_recorded_separately() {
        let dir = tempfile::tempdir().expect("tempdir");
        record(
            dir.path(),
            "proj",
            "http://a/v1/traces",
            &receipt("2026-01-01T00:00:00Z", 5),
        )
        .expect("record a");
        record(
            dir.path(),
            "proj",
            "http://b/v1/traces",
            &receipt("2026-01-02T00:00:00Z", 9),
        )
        .expect("record b");
        let state = load(dir.path(), "proj");
        assert_eq!(state.last("http://a/v1/traces").expect("a").spans, 5);
        assert_eq!(state.last("http://b/v1/traces").expect("b").spans, 9);
        // ever_sent picks the most recent across endpoints.
        assert_eq!(state.ever_sent().expect("some").0, "http://b/v1/traces");
    }

    /// Two processes sending concurrently must not rewind the stamp and make a
    /// sent trace look unsent.
    #[test]
    fn an_earlier_run_cannot_rewind_a_later_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ep = "http://a/v1/traces";
        record(
            dir.path(),
            "proj",
            ep,
            &receipt("2026-05-05T12:00:00Z", 100),
        )
        .expect("late");
        record(dir.path(), "proj", ep, &receipt("2026-01-01T00:00:00Z", 1)).expect("early");
        let state = load(dir.path(), "proj");
        assert_eq!(state.last(ep).expect("some").at, "2026-05-05T12:00:00Z");
        assert_eq!(state.last(ep).expect("some").spans, 100);
    }
}
