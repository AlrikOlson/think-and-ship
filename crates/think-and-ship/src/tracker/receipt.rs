//! When the unattended push last actually SUCCEEDED.
//!
//! # The absence this closes
//!
//! `CliTrackerPusher` logged success at `debug!` and failure at `warn!`, and
//! `init_tracing` installs a subscriber at `warn` on stderr — which, for an MCP
//! server, is the client's log file. Nothing anywhere recorded that a push had
//! worked. So a cadence that had been failing for two days was indistinguishable
//! from one that was working, and the only way to answer "is the passive push
//! running?" was to write to a live tracker and watch.
//!
//! A receipt is the cheapest possible fix: one stamp per provider per successful
//! run, written after the run completes.
//!
//! # Why it is not the sweep watermark
//!
//! The shape is deliberately copied from [`crate::tracker::sweep::Watermarks`]
//! — same per-provider keying, same locked merge, same read-as-empty on a
//! missing or corrupt file — but the meaning is opposite. A watermark is an
//! INPUT: it tells the next sweep what to ask for, so losing it costs a wasted
//! refetch. A receipt is only ever an OUTPUT, read by humans. Nothing branches
//! on it, which is exactly why it can be reported honestly without any risk of
//! a stale stamp changing behaviour.
//!
//! # The absent receipt is the interesting one
//!
//! [`Receipts::last`] returning `None` is not an error state to hide. "This has
//! never succeeded" is the single most useful thing this file can say, and it is
//! the thing the old `debug!` line could never say at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What one successful push did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushReceipt {
    /// RFC-3339 instant the run finished.
    pub at: String,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
}

impl PushReceipt {
    /// The one-line summary a human reads in `tracker status`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} — {} created, {} updated, {} unchanged",
            self.at, self.created, self.updated, self.unchanged
        )
    }
}

/// Every provider's most recent successful push, for one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Receipts {
    #[serde(default)]
    pub by_provider: BTreeMap<String, PushReceipt>,
}

impl Receipts {
    /// The last successful push to `provider`, or `None` when there has never
    /// been one. The `None` is load-bearing — see the module docs.
    #[must_use]
    pub fn last(&self, provider: &str) -> Option<&PushReceipt> {
        self.by_provider.get(provider)
    }
}

fn receipt_path(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir
        .join("tracker")
        .join("receipts")
        .join(format!("{project_id}.json"))
}

/// Read a project's receipts. A missing or corrupt file reads as empty, so the
/// report degrades to "never succeeded" rather than to a crash — and nothing
/// branches on the value, so reading empty can only ever cost a pessimistic
/// line of output.
#[must_use]
pub fn load(data_dir: &Path, project_id: &str) -> Receipts {
    std::fs::read_to_string(receipt_path(data_dir, project_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Record one provider's successful push.
///
/// The merge keeps whichever receipt is LATER, compared as instants rather than
/// bytes, so two processes pushing the same project concurrently cannot move
/// the stamp backwards and make a working cadence look stale.
pub fn record(
    data_dir: &Path,
    project_id: &str,
    provider: &str,
    receipt: &PushReceipt,
) -> std::io::Result<Receipts> {
    let mut state = load(data_dir, project_id);
    state
        .by_provider
        .insert(provider.to_string(), receipt.clone());

    crate::infra::persistence::locked_merge_write(
        &receipt_path(data_dir, project_id),
        &state,
        |ours: &Receipts, disk: Receipts| {
            let mut merged = disk;
            for (provider, ours_receipt) in &ours.by_provider {
                let keep = match merged.by_provider.get(provider) {
                    Some(theirs) => {
                        if crate::roadmap::domain::rfc3339_newer(&ours_receipt.at, &theirs.at) {
                            ours_receipt.clone()
                        } else {
                            theirs.clone()
                        }
                    }
                    None => ours_receipt.clone(),
                };
                merged.by_provider.insert(provider.clone(), keep);
            }
            merged
        },
    )?;
    // Re-read rather than return `state`: the merge may have kept a concurrent
    // process's later receipt, and the caller should see what is actually on
    // disk rather than what this process proposed.
    Ok(load(data_dir, project_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(at: &str, updated: usize) -> PushReceipt {
        PushReceipt {
            at: at.to_string(),
            created: 0,
            updated,
            unchanged: 0,
        }
    }

    #[test]
    fn a_project_that_never_pushed_reports_nothing_rather_than_a_zero() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let receipts = load(dir.path(), "proj");
        // Not `Some(PushReceipt::default())` — a zeroed receipt would read as
        // "pushed, and did nothing", which is the confusion this file exists to
        // remove.
        assert!(receipts.last("linear").is_none());
    }

    #[test]
    fn a_recorded_push_is_readable_back() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        record(
            dir.path(),
            "proj",
            "linear",
            &receipt("2026-07-28T04:00:00+00:00", 3),
        )
        .expect("record");
        let got = load(dir.path(), "proj");
        assert_eq!(got.last("linear").map(|r| r.updated), Some(3));
        // Scoped per provider: recording linear says nothing about github.
        assert!(got.last("github").is_none());
    }

    #[test]
    fn a_later_run_wins_and_an_earlier_one_cannot_rewind_it() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        record(
            dir.path(),
            "proj",
            "linear",
            &receipt("2026-07-28T04:00:00+00:00", 3),
        )
        .expect("record");
        // An older run finishing late must not make a working cadence look
        // stale.
        record(
            dir.path(),
            "proj",
            "linear",
            &receipt("2026-07-28T03:00:00+00:00", 1),
        )
        .expect("record");
        let got = load(dir.path(), "proj");
        assert_eq!(
            got.last("linear").map(|r| r.at.as_str()),
            Some("2026-07-28T04:00:00+00:00")
        );
    }

    #[test]
    fn a_corrupt_file_reads_as_never_succeeded() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = receipt_path(dir.path(), "proj");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{ not json").expect("write");
        assert!(load(dir.path(), "proj").last("linear").is_none());
    }
}
