//! Roadmap hygiene (`stale-risk-signals`) — the self-building roadmap (33e)
//! extended to self-cleaning.
//!
//! Inverts the measured 31j insight (readiness + activity predict completion):
//! a chunk that LOOKS next but shows no recent activity is a stalled-work
//! smell. Two findings:
//! - **stalled-in-progress**: `in_progress` untouched longer than `stall_days`;
//! - **ready-but-idle**: `pending`, all deps done, in the top-K ready chunks
//!   by priority (the inspectable proxy for "high predicted-next" — running
//!   the learned softmax here would add corpus plumbing for marginal gain),
//!   untouched longer than `idle_days`.
//!
//! Findings become ordinary signals (kind `Concern`, `from: "hygiene"`) with a
//! `chunk:<id>` cross-ref — they ride the existing triage inbox end to end.
//!
//! Anti-spam contract: a chunk is SUPPRESSED while any signal referencing it
//! is live (`New`/`Triaged`/`Researched`/`Surfaced`) or was created within the
//! throttle window (which also covers recently-`Dismissed` — the operator said
//! no; respect it). A `Promoted`/`Dismissed` signal OLDER than the window
//! allows re-emission: a chunk can honestly go stale twice.

use chrono::DateTime;

use crate::roadmap::domain::{Chunk, ChunkStatus};
use crate::signal::domain::{Signal, SignalStatus};

#[derive(Debug, Clone, Copy)]
pub struct HygieneOptions {
    pub stall_days: i64,
    pub idle_days: i64,
    /// How many ready-pending chunks (by priority) are "next enough" to nag about.
    pub top_k: usize,
}

impl Default for HygieneOptions {
    fn default() -> Self {
        Self {
            stall_days: 7,
            idle_days: 7,
            top_k: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    StalledInProgress,
    ReadyButIdle,
}

#[derive(Debug, Clone)]
pub struct HygieneFinding {
    pub chunk_id: String,
    pub kind: FindingKind,
    /// The one-line, human-facing reason (becomes the signal body).
    pub reason: String,
}

fn days_since(now: &str, then: &str) -> Option<i64> {
    let a = DateTime::parse_from_rfc3339(now).ok()?;
    let b = DateTime::parse_from_rfc3339(then).ok()?;
    Some((a - b).num_days())
}

/// Whether an existing signal suppresses a new hygiene emission for `chunk_ref`.
fn suppresses(signal: &Signal, chunk_ref: &str, now: &str, window_days: i64) -> bool {
    if !signal.cross_refs.iter().any(|r| r == chunk_ref) {
        return false;
    }
    let live = matches!(
        signal.status,
        SignalStatus::New
            | SignalStatus::Triaged
            | SignalStatus::Researched
            | SignalStatus::Surfaced
    );
    let recent = days_since(now, &signal.created).is_some_and(|d| d <= window_days);
    live || recent
}

/// Pure: detect stalled / ready-but-idle chunks, already throttled against the
/// existing signal set. `now` is RFC 3339 (injected for deterministic tests).
pub fn detect(
    chunks: &[Chunk],
    signals: &[Signal],
    now: &str,
    opts: HygieneOptions,
) -> Vec<HygieneFinding> {
    let done: std::collections::BTreeSet<&str> = chunks
        .iter()
        .filter(|c| c.status == ChunkStatus::Done)
        .map(|c| c.id.as_str())
        .collect();
    let deps_met = |c: &Chunk| c.deps.iter().all(|d| done.contains(d.as_str()));

    // The top-K ready pending chunks by priority — "next enough" to matter.
    let mut ready: Vec<&Chunk> = chunks
        .iter()
        .filter(|c| c.status == ChunkStatus::Pending && deps_met(c))
        .collect();
    ready.sort_by_key(|c| (c.priority, c.id.clone()));
    let top_ready: Vec<&Chunk> = ready.into_iter().take(opts.top_k).collect();

    let window = opts.stall_days.max(opts.idle_days);
    let mut findings = Vec::new();

    for chunk in chunks {
        let candidate = match chunk.status {
            ChunkStatus::InProgress => {
                days_since(now, &chunk.updated_at)
                    .filter(|d| *d > opts.stall_days)
                    .map(|d| HygieneFinding {
                        chunk_id: chunk.id.clone(),
                        kind: FindingKind::StalledInProgress,
                        reason: format!(
                            "'{}' has been in progress but untouched for {d} days (since {}). Finish, block, or release it.",
                            chunk.title,
                            &chunk.updated_at[..10.min(chunk.updated_at.len())],
                        ),
                    })
            }
            ChunkStatus::Pending if top_ready.iter().any(|c| c.id == chunk.id) => {
                days_since(now, &chunk.updated_at)
                    .filter(|d| *d > opts.idle_days)
                    .map(|d| HygieneFinding {
                        chunk_id: chunk.id.clone(),
                        kind: FindingKind::ReadyButIdle,
                        reason: format!(
                            "'{}' is ready (deps met, top {} by priority) but untouched for {d} days. Work it, demote it, or obsolete it.",
                            chunk.title, opts.top_k,
                        ),
                    })
            }
            _ => None,
        };
        let Some(finding) = candidate else { continue };
        let chunk_ref = format!("chunk:{}", finding.chunk_id);
        if signals
            .iter()
            .any(|s| suppresses(s, &chunk_ref, now, window))
        {
            continue;
        }
        findings.push(finding);
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::domain::SignalKind;

    const NOW: &str = "2026-06-11T00:00:00Z";

    fn chunk(id: &str, status: ChunkStatus, priority: u32, updated: &str, deps: &[&str]) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: format!("Chunk {id}"),
            name: crate::roadmap::name::derive(id),
            status,
            priority,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: None,
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: updated.into(),
        }
    }

    fn signal(chunk_ref: &str, status: SignalStatus, created: &str) -> Signal {
        Signal {
            id: format!("sig-{chunk_ref}-{created}"),
            kind: SignalKind::Concern,
            from: "hygiene".into(),
            body: String::new(),
            content: None,
            created: created.into(),
            status,
            enrichment: vec![],
            cross_refs: vec![chunk_ref.into()],
            surfaced_at: None,
            snooze_until: None,
            project_id: None,
        }
    }

    #[test]
    fn flags_stalled_in_progress_and_ready_idle_but_not_fresh_or_unready() {
        let chunks = vec![
            chunk(
                "stalled",
                ChunkStatus::InProgress,
                10,
                "2026-06-01T00:00:00Z",
                &[],
            ),
            chunk(
                "fresh-wip",
                ChunkStatus::InProgress,
                10,
                "2026-06-10T12:00:00Z",
                &[],
            ),
            chunk(
                "ready-idle",
                ChunkStatus::Pending,
                20,
                "2026-06-01T00:00:00Z",
                &[],
            ),
            chunk(
                "gated",
                ChunkStatus::Pending,
                1,
                "2026-06-01T00:00:00Z",
                &["missing-dep"],
            ),
            chunk(
                "done-old",
                ChunkStatus::Done,
                1,
                "2026-05-01T00:00:00Z",
                &[],
            ),
        ];
        let findings = detect(&chunks, &[], NOW, HygieneOptions::default());
        let ids: Vec<&str> = findings.iter().map(|f| f.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["stalled", "ready-idle"]);
        assert!(findings[0].reason.contains("10 days"));
        assert!(findings[1].reason.contains("ready"));
    }

    #[test]
    fn top_k_cuts_the_ready_nag_list_by_priority() {
        let mut chunks: Vec<Chunk> = (0..8)
            .map(|i| {
                chunk(
                    &format!("p{i}"),
                    ChunkStatus::Pending,
                    i,
                    "2026-06-01T00:00:00Z",
                    &[],
                )
            })
            .collect();
        chunks.push(chunk(
            "d",
            ChunkStatus::Done,
            0,
            "2026-05-01T00:00:00Z",
            &[],
        ));
        let findings = detect(
            &chunks,
            &[],
            NOW,
            HygieneOptions {
                top_k: 3,
                ..Default::default()
            },
        );
        assert_eq!(
            findings.len(),
            3,
            "only the top-3 ready chunks are next enough to nag"
        );
        assert!(
            findings
                .iter()
                .all(|f| ["p0", "p1", "p2"].contains(&f.chunk_id.as_str()))
        );
    }

    #[test]
    fn live_or_recent_signals_suppress_but_old_resolved_ones_do_not() {
        let chunks = vec![chunk(
            "stalled",
            ChunkStatus::InProgress,
            1,
            "2026-06-01T00:00:00Z",
            &[],
        )];
        let opts = HygieneOptions::default();

        // Live signal → suppressed (any age).
        let live = [signal(
            "chunk:stalled",
            SignalStatus::New,
            "2026-05-01T00:00:00Z",
        )];
        assert!(detect(&chunks, &live, NOW, opts).is_empty());

        // Recently dismissed → suppressed (the operator said no; respect it).
        let dismissed_recent = [signal(
            "chunk:stalled",
            SignalStatus::Dismissed,
            "2026-06-08T00:00:00Z",
        )];
        assert!(detect(&chunks, &dismissed_recent, NOW, opts).is_empty());

        // Dismissed long ago → re-emission allowed (it went stale again).
        let dismissed_old = [signal(
            "chunk:stalled",
            SignalStatus::Dismissed,
            "2026-04-01T00:00:00Z",
        )];
        assert_eq!(detect(&chunks, &dismissed_old, NOW, opts).len(), 1);

        // Promoted long ago → re-emission allowed too.
        let promoted_old = [signal(
            "chunk:stalled",
            SignalStatus::Promoted,
            "2026-04-01T00:00:00Z",
        )];
        assert_eq!(detect(&chunks, &promoted_old, NOW, opts).len(), 1);

        // A signal about a DIFFERENT chunk never suppresses.
        let other = [signal(
            "chunk:other",
            SignalStatus::New,
            "2026-06-10T00:00:00Z",
        )];
        assert_eq!(detect(&chunks, &other, NOW, opts).len(), 1);
    }

    #[test]
    fn a_freshly_worked_roadmap_is_silent() {
        let chunks = vec![
            chunk(
                "wip",
                ChunkStatus::InProgress,
                1,
                "2026-06-10T20:00:00Z",
                &[],
            ),
            chunk("next", ChunkStatus::Pending, 2, "2026-06-10T21:00:00Z", &[]),
        ];
        assert!(detect(&chunks, &[], NOW, HygieneOptions::default()).is_empty());
    }
}
