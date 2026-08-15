//! The reconcile sweep — the backstop for a webhook that never arrives.
//!
//! # Why a sweep exists at all
//!
//! Every provider documents webhook delivery as best-effort. GitHub, Linear and
//! Jira all drop events under load or during an outage, and none of them will
//! tell you which ones. A sync whose only inbound path is webhooks diverges
//! silently and never notices — the failure mode is not an error, it is a
//! quietly stale plan.
//!
//! So the sweep asks the provider directly: what changed since the last time we
//! looked? Anything a webhook lost turns up here.
//!
//! # What it deliberately does NOT do
//!
//! It does not apply anything. Whether a remote retitle should win, whether a
//! closed ticket becomes a proposal on the chunk, which fields we own and which
//! the team owns — that is the conflict policy, and it is its own subsystem.
//! A sweep that applied changes would be that policy written badly, in the
//! wrong place, by a component whose job is detection. [`SweepReport`] is the
//! deliverable; deciding is somebody else's.
//!
//! # The watermark rule that is easy to get wrong
//!
//! The obvious implementation stores the newest `updated_at` it saw and asks
//! for everything after that next time. It loses data. Between the moment the
//! provider computed its response and the moment the batch finishes processing,
//! anything written falls into a gap: it is older than the newest record we
//! saw, so the next sweep never asks for it, and no webhook is coming because
//! that is the situation this exists to survive.
//!
//! So the watermark is the instant the run STARTED, captured before the fetch
//! and persisted only after the whole batch is processed. A record written
//! during the sweep is newer than the run start, so the next sweep collects it —
//! possibly a second time, which costs one classification and is why the whole
//! pipeline is idempotent.
//!
//! That same ordering is what makes a crash safe: the watermark only moves
//! after the work is done, so a crash anywhere leaves it where it was and the
//! next run redoes the window. "Advance only after the batch" and "a crash does
//! not skip unprocessed items" are one property, not two.
//!
//! NOTE — `cloud/events.rs` keeps its watermark in a per-process `HashMap` and
//! advances it to the newest record's stamp. That is the shape this module
//! deliberately does not copy; the gap is filed as `cloud-watermark-gap`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::roadmap::engine::RoadmapEngine;
use crate::tracker::domain::WorkItem;
use crate::tracker::echo::{Verdict, classify};
use crate::tracker::port::{TrackerError, TrackerPort};

/// The instant before which everything is known to have been seen.
///
/// Keyed by provider within a project's file, so two providers sweep
/// independently and one falling behind cannot stall the other.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Watermarks {
    /// `provider -> RFC-3339 instant of the last COMPLETED sweep's start`.
    #[serde(default)]
    pub by_provider: BTreeMap<String, String>,
}

impl Watermarks {
    /// What to ask the provider for, or `None` when this provider has never
    /// swept — in which case the caller wants everything, not nothing.
    #[must_use]
    pub fn since(&self, provider: &str) -> Option<&str> {
        self.by_provider.get(provider).map(String::as_str)
    }
}

fn watermark_path(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir
        .join("tracker")
        .join("watermarks")
        .join(format!("{project_id}.json"))
}

/// Read a project's watermarks. A missing or corrupt file reads as empty, which
/// makes the next sweep fetch everything — the safe direction. Failing closed
/// here would mean a corrupt byte silently disabling the backstop.
#[must_use]
pub fn load(data_dir: &Path, project_id: &str) -> Watermarks {
    std::fs::read_to_string(watermark_path(data_dir, project_id))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist one provider's watermark.
///
/// The merge keeps whichever instant is LATER, so two processes sweeping the
/// same project concurrently cannot move the watermark backwards and re-open a
/// window that was already closed.
pub fn record(
    data_dir: &Path,
    project_id: &str,
    provider: &str,
    run_start: &str,
) -> std::io::Result<Watermarks> {
    let mut state = load(data_dir, project_id);
    state
        .by_provider
        .insert(provider.to_string(), run_start.to_string());

    crate::infra::persistence::locked_merge_write(
        &watermark_path(data_dir, project_id),
        &state,
        |ours: &Watermarks, disk: Watermarks| {
            let mut merged = disk;
            for (provider, ours_at) in &ours.by_provider {
                let keep = match merged.by_provider.get(provider) {
                    // Compared as INSTANTS. These strings come from different
                    // clocks and formats — a `Z` from one provider, a `+00:00`
                    // from ours — and byte order disagrees with time order
                    // across them.
                    Some(theirs) => {
                        if crate::roadmap::domain::rfc3339_newer(ours_at, theirs) {
                            ours_at.clone()
                        } else {
                            theirs.clone()
                        }
                    }
                    None => ours_at.clone(),
                };
                merged.by_provider.insert(provider.clone(), keep);
            }
            merged
        },
    )?;
    Ok(load(data_dir, project_id))
}

/// What one sweep found, split by what the echo fence made of it.
#[derive(Debug, Default, Clone)]
pub struct SweepReport {
    /// Everything the provider returned for the window.
    pub fetched: usize,
    /// Our own writes coming back. Suppressed, and the reason the sweep cannot
    /// start a loop with itself.
    pub echoes: usize,
    /// Echoes whose content hash disagreed with what we recorded — evidence the
    /// adapter's round trip is lossy, not evidence of a remote change.
    pub drifted: Vec<WorkItem>,
    /// Genuine changes that reached us by no other path. The whole point.
    pub remote: Vec<WorkItem>,
    /// The instant the watermark advanced to, or `None` when it did not move
    /// because the sweep failed partway.
    pub advanced_to: Option<String>,
}

/// Fetch everything changed since the last completed sweep and classify it.
///
/// `run_start` is the caller's clock, taken BEFORE any I/O — it is injected
/// rather than read here so a test can drive the ordering that makes the
/// no-gap rule observable.
///
/// The watermark advances only on success. An error leaves it untouched and
/// returns the error, so the next run redoes the window rather than skipping it.
pub async fn reconcile(
    engine: &RoadmapEngine,
    tracker: &dyn TrackerPort,
    data_dir: &Path,
    run_start: &str,
) -> Result<SweepReport, TrackerError> {
    let provider = tracker.provider().to_string();
    let project_id = engine.project_id().to_string();

    // Never swept? Ask for everything. `None` here would mean "ask for nothing",
    // which would make the first sweep a silent no-op.
    let marks = load(data_dir, &project_id);
    let since = marks.since(&provider).unwrap_or(BEGINNING).to_string();

    let items = tracker.fetch_since(&since).await?;

    let mut report = SweepReport {
        fetched: items.len(),
        ..SweepReport::default()
    };

    for item in items {
        // The link is what makes an item ours. Resolved by external id, because
        // that is the only identity that survives a human renaming the ticket.
        // Resolved by EXTERNAL ID, the only identity that survives a human
        // renaming the ticket. An item with no id was never ours.
        let link = item
            .external_id
            .as_deref()
            .and_then(|id| engine.tracker_link_by_external_id(&provider, id));

        match classify(&item, link) {
            Verdict::Echo => report.echoes += 1,
            Verdict::EchoWithDrift => {
                report.echoes += 1;
                report.drifted.push(item);
            }
            Verdict::Remote => report.remote.push(item),
        }
    }

    // Only now, with every item accounted for, does the window close.
    record(data_dir, &project_id, &provider, run_start)
        .map_err(|e| TrackerError::Transport(format!("could not persist the watermark: {e}")))?;
    report.advanced_to = Some(run_start.to_string());

    Ok(report)
}

/// The start of time, for a provider that has never been swept.
const BEGINNING: &str = "1970-01-01T00:00:00+00:00";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_that_has_never_swept_has_no_watermark() {
        let marks = Watermarks::default();
        assert_eq!(marks.since("linear"), None);
    }

    #[test]
    fn providers_keep_independent_windows() {
        let mut marks = Watermarks::default();
        marks
            .by_provider
            .insert("linear".into(), "2026-07-26T10:00:00+00:00".into());
        assert_eq!(marks.since("linear"), Some("2026-07-26T10:00:00+00:00"));
        assert_eq!(
            marks.since("github"),
            None,
            "one provider's progress must not imply another's"
        );
    }
}
