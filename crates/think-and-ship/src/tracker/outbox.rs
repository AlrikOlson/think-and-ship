//! Durable delivery for projections — the cloud outbox, reused rather than
//! re-invented.
//!
//! # One delivery guarantee, not two
//!
//! [`crate::cloud::outbox::Outbox`] already encodes the semantics this needs, and
//! they were learned the hard way: latest-only replacement per key so a replay
//! sends the current truth rather than a queue of stale states, bounded
//! drop-oldest so an offline week cannot grow without limit, tombstones so a
//! flushed entry is not resurrected by the next merge, and `locked_merge_write`
//! persistence so two processes sharing a data dir converge. Writing a second
//! queue would mean maintaining two answers to every one of those questions, and
//! they would diverge.
//!
//! What is NOT shared is the destination. The cloud outbox carries
//! `UnifiedRecordEnvelope`s to our own backend; a tracker write goes to GitHub.
//! So this is a separate *instance* of the same type, under its own path, with a
//! flush loop that mirrors `CloudClient::flush_outbox` exactly:
//!
//! - success → remove the entry and continue
//! - retryable failure → **stop**, leaving the rest queued (the provider is
//!   unwell; hammering it with the remaining backlog helps nobody)
//! - terminal failure → log loudly, drop the entry, continue (it would fail
//!   forever, and a queue that never drains is a queue nobody trusts)
//!
//! [`TrackerError::retryable`] was written from day one to mirror the cloud
//! client's classifier, and there is already a test asserting the two agree —
//! which is what makes riding this contract safe rather than merely convenient.

use std::path::PathBuf;
use std::sync::Arc;

use crate::cloud::outbox::Outbox;
use crate::tracker::domain::WorkItem;
use crate::tracker::port::{TrackerError, TrackerPort};

/// A queue of projections awaiting delivery to one provider.
#[derive(Clone)]
pub struct TrackerOutbox {
    inner: Arc<Outbox>,
}

impl TrackerOutbox {
    /// `path: None` keeps the queue in memory — still useful within a process,
    /// and what tests use.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Outbox::new(path)),
        }
    }

    /// `<data_dir>/tracker/outbox/<project_id>.json`, mirroring where the cloud
    /// outbox lives so operators find both in the same place.
    #[must_use]
    pub fn path_for(data_dir: &std::path::Path, project_id: &str) -> PathBuf {
        data_dir
            .join("tracker")
            .join("outbox")
            .join(format!("{project_id}.json"))
    }

    /// Identity of a queued projection. Latest-only replacement is keyed on
    /// this, so re-queuing the same chunk supersedes the older attempt rather
    /// than stacking a second write of a state we no longer believe.
    #[must_use]
    fn key(provider: &str, chunk_id: &str) -> String {
        format!("tracker/{provider}/{chunk_id}")
    }

    /// Queue a projection for replay.
    pub fn enqueue(&self, provider: &str, chunk_id: &str, item: &WorkItem) {
        let payload = serde_json::json!({
            "provider": provider,
            "chunk_id": chunk_id,
            "item": item,
        });
        self.inner.enqueue(Self::key(provider, chunk_id), payload);
    }

    /// How many projections are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Chunk ids currently queued for `provider`, oldest first.
    #[must_use]
    pub fn queued_chunks(&self, provider: &str) -> Vec<String> {
        let prefix = format!("tracker/{provider}/");
        self.inner
            .snapshot()
            .into_iter()
            .filter_map(|e| e.key.strip_prefix(&prefix).map(str::to_string))
            .collect()
    }

    /// Replay queued projections against `tracker`.
    ///
    /// Returns how many entries left the queue — drained, whether they succeeded
    /// or were dropped as permanently rejected. A retryable failure pauses the
    /// drain rather than skipping past it, so ordering is preserved and a sick
    /// provider is not hammered with the remaining backlog.
    pub async fn flush(&self, tracker: &dyn TrackerPort) -> usize {
        if self.inner.is_empty() || !self.inner.begin_flush() {
            return 0;
        }
        let provider = tracker.provider();
        let mut drained = 0;

        for entry in self.inner.snapshot() {
            if !entry.key.starts_with(&format!("tracker/{provider}/")) {
                continue;
            }
            let Some(item) = entry
                .envelope
                .get("item")
                .and_then(|v| serde_json::from_value::<WorkItem>(v.clone()).ok())
            else {
                // Undecodable: it can never succeed, so it is terminal by the
                // same reasoning as a 4xx.
                tracing::warn!(
                    target: "think_and_ship::tracker",
                    "queued projection {} is unreadable — dropping it", entry.key
                );
                self.inner.remove(&entry);
                drained += 1;
                continue;
            };

            match tracker.upsert_item(&item).await {
                Ok(_) => {
                    self.inner.remove(&entry);
                    drained += 1;
                }
                Err(e) if e.retryable() => {
                    tracing::debug!(
                        target: "think_and_ship::tracker",
                        "projection replay paused ({} left): {e}", self.inner.len()
                    );
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "think_and_ship::tracker",
                        "queued projection {} was REJECTED by the provider — dropping it \
                         (it would fail forever): {e}",
                        entry.key
                    );
                    self.inner.remove(&entry);
                    drained += 1;
                }
            }
        }

        self.inner.end_flush();
        if drained > 0 {
            tracing::info!(
                target: "think_and_ship::tracker",
                "replayed {drained} queued projection(s)"
            );
        }
        drained
    }
}

/// Apply the outbox contract to one failed write.
///
/// Split out so the projector and the replay loop cannot drift on what "5xx
/// queues, 4xx does not" means. Returns `true` when the failure was queued.
pub(crate) fn handle_failure(
    outbox: Option<&TrackerOutbox>,
    provider: &str,
    chunk_id: &str,
    item: &WorkItem,
    error: &TrackerError,
) -> bool {
    if error.retryable() {
        match outbox {
            Some(o) => {
                o.enqueue(provider, chunk_id, item);
                tracing::warn!(
                    target: "think_and_ship::tracker",
                    "projection of '{chunk_id}' failed, queued for replay: {error}"
                );
                true
            }
            None => false,
        }
    } else {
        // Never queued. A contract rejection would fail identically on every
        // replay, so queueing it produces a backlog that can only grow.
        tracing::warn!(
            target: "think_and_ship::tracker",
            "projection of '{chunk_id}' was REJECTED by the provider \
             (not queued — it would fail forever): {error}"
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::fake::FakeTracker;

    fn item(title: &str) -> WorkItem {
        WorkItem::new(title)
    }

    #[test]
    fn a_retryable_failure_queues_and_a_terminal_one_does_not() {
        let outbox = TrackerOutbox::new(None);

        assert!(handle_failure(
            Some(&outbox),
            "fake",
            "c1",
            &item("c1"),
            &TrackerError::Transport("connection reset".into()),
        ));
        assert!(handle_failure(
            Some(&outbox),
            "fake",
            "c2",
            &item("c2"),
            &TrackerError::Status {
                status: 503,
                body: String::new()
            },
        ));
        assert_eq!(outbox.len(), 2, "transport and 5xx both queue");

        assert!(!handle_failure(
            Some(&outbox),
            "fake",
            "c3",
            &item("c3"),
            &TrackerError::Status {
                status: 422,
                body: "required field missing".into()
            },
        ));
        assert!(!handle_failure(
            Some(&outbox),
            "fake",
            "c4",
            &item("c4"),
            &TrackerError::Unsupported("labels".into()),
        ));
        assert_eq!(outbox.len(), 2, "a 4xx must never be queued");
    }

    /// Latest-only: a chunk queued twice is one entry carrying the newer state,
    /// not two writes of a history nobody wants replayed.
    #[test]
    fn requeuing_a_chunk_supersedes_the_older_attempt() {
        let outbox = TrackerOutbox::new(None);
        outbox.enqueue("fake", "c1", &item("old"));
        outbox.enqueue("fake", "c1", &item("new"));
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox.queued_chunks("fake"), vec!["c1".to_string()]);
    }

    #[tokio::test]
    async fn a_successful_replay_drains_the_queue() {
        let outbox = TrackerOutbox::new(None);
        outbox.enqueue("fake", "c1", &item("c1"));
        let tracker = FakeTracker::new("fake");

        assert_eq!(outbox.flush(&tracker).await, 1);
        assert!(outbox.is_empty());
        assert_eq!(tracker.writes(), 1);
    }

    /// A sick provider must pause the drain, not have the whole backlog thrown
    /// at it — and nothing may be lost while it is down.
    #[tokio::test]
    async fn a_retryable_failure_pauses_the_drain_and_keeps_the_backlog() {
        let outbox = TrackerOutbox::new(None);
        outbox.enqueue("fake", "c1", &item("c1"));
        outbox.enqueue("fake", "c2", &item("c2"));
        let tracker = FakeTracker::new("fake");
        tracker.fail_next(TrackerError::Transport("down".into()));

        assert_eq!(outbox.flush(&tracker).await, 0);
        assert_eq!(
            outbox.len(),
            2,
            "nothing is lost while the provider is down"
        );

        // Recovered.
        assert_eq!(outbox.flush(&tracker).await, 2);
        assert!(outbox.is_empty());
    }

    /// A permanently-rejected entry must leave, or the queue never drains again.
    #[tokio::test]
    async fn a_terminal_failure_on_replay_drops_the_entry() {
        let outbox = TrackerOutbox::new(None);
        outbox.enqueue("fake", "c1", &item("c1"));
        let tracker = FakeTracker::new("fake");
        tracker.fail_next(TrackerError::Status {
            status: 422,
            body: "unprocessable".into(),
        });

        assert_eq!(outbox.flush(&tracker).await, 1);
        assert!(
            outbox.is_empty(),
            "a forever-failing entry must not block the queue"
        );
        assert_eq!(tracker.writes(), 0);
    }

    #[test]
    fn without_an_outbox_a_retryable_failure_is_not_swallowed() {
        assert!(!handle_failure(
            None,
            "fake",
            "c1",
            &item("c1"),
            &TrackerError::Transport("reset".into()),
        ));
    }
}
