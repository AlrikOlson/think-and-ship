//! Durable outbound queue for failed cloud pushes (sync-offline-queue).
//! Before this, a fire-and-forget push that failed was logged
//! and DROPPED — an offline session's mutations never reached the workspace
//! until each record happened to be touched again.
//!
//! Capture happens INSIDE [`CloudClient::push`](crate::cloud::client::CloudClient::push)
//! (the one chokepoint every writer funnels through), so all four families,
//! the backfill, and every future writer inherit it with zero wiring.
//! Flushing happens wherever connectivity is (re)proven: the realtime loop's
//! `refresh("*")` (every successful connect + every poll-fallback cycle) and
//! the boot hydrate, BEFORE the pull — so queued local mutations land first
//! and win recency at the store.
//!
//! Semantics:
//! - **Per-record latest-only**: keyed by `<family>/<kind>/<id>`; a newer
//!   failed push replaces the older (the store is LWW on the same key, so
//!   replaying intermediate states is pure waste). Replay is idempotent by
//!   the envelope's idempotency key.
//! - **Bounded**: at most [`OUTBOX_CAP`] entries; overflow drops the OLDEST
//!   with a WARN naming the dropped record (no silent caps).
//! - **Durable**: persisted via the locked-merge discipline (one writer at a
//!   time, concurrent processes union by key, memory wins on conflict), so a
//!   crashed session's queue survives to the next boot.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::infra::persistence::locked_merge_write;

/// Bound on queued records; overflow drops the oldest with a WARN.
pub const OUTBOX_CAP: usize = 512;

/// One queued push: the record's identity key plus the full envelope JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboxEntry {
    /// `<family>/<kind>/<id>` — the store's identity, NOT the idempotency key
    /// (latest-only replacement needs the logical identity).
    pub key: String,
    /// The envelope exactly as it would have been pushed.
    pub envelope: serde_json::Value,
}

/// The persisted shape — insertion-ordered (oldest first) for drop-oldest
/// and oldest-first replay. `tombstones` are content digests of FLUSHED
/// envelopes: a plain union-by-key merge would resurrect removed entries
/// from the disk copy (found live by the 31d-c cross-process proof — every
/// boot re-pushed stale entries forever). A tombstone defeats the union for
/// exactly that envelope; a NEWER envelope for the same record has a
/// different digest and queues normally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutboxState {
    pub entries: Vec<OutboxEntry>,
    #[serde(default)]
    pub tombstones: Vec<String>,
}

/// Bound on remembered tombstones (oldest pruned beyond this).
const TOMBSTONE_CAP: usize = 1024;

/// The content digest a tombstone records — envelope-exact, so flushing one
/// version of a record never blocks queueing a later version.
fn digest(envelope: &serde_json::Value) -> String {
    crate::cloud::envelope::sha256_hex(&envelope.to_string())
}

/// Union `disk` into `memory` (memory wins by key; tombstoned envelopes stay
/// dead) — the outbox's locked-merge rule.
fn merge_outboxes(memory: &OutboxState, disk: OutboxState) -> OutboxState {
    let mut merged = memory.clone();
    for stone in disk.tombstones {
        if !merged.tombstones.contains(&stone) {
            merged.tombstones.push(stone);
        }
    }
    for entry in disk.entries {
        if !merged.entries.iter().any(|e| e.key == entry.key)
            && !merged.tombstones.contains(&digest(&entry.envelope))
        {
            merged.entries.push(entry);
        }
    }
    while merged.tombstones.len() > TOMBSTONE_CAP {
        merged.tombstones.remove(0);
    }
    merged
}

/// The shared queue. Lives inside the [`CloudClient`](super::client::CloudClient) behind an `Arc`, so
/// client clones across engines share one queue.
#[derive(Debug)]
pub struct Outbox {
    /// `None` ⇒ in-memory only (persistence disabled / no data dir) — still
    /// flushes within the process lifetime.
    path: Option<PathBuf>,
    state: Mutex<OutboxState>,
    /// Guards against concurrent flushes (boot hydrate + realtime `*` race).
    flushing: AtomicBool,
}

impl Outbox {
    /// Build the outbox, loading any persisted queue from `path` (tolerant:
    /// a missing or malformed file starts empty — never blocks a boot).
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut state = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<OutboxState>(&raw).ok())
            .unwrap_or_default();
        // Belt-and-braces: a concurrently-written file may carry entries a
        // tombstone already killed.
        state
            .entries
            .retain(|e| !state.tombstones.contains(&digest(&e.envelope)));
        Self {
            path,
            state: Mutex::new(state),
            flushing: AtomicBool::new(false),
        }
    }

    /// Queue a failed push (latest-only per key; bounded). Never fails — a
    /// persist error is logged and the entry stays in memory.
    pub fn enqueue(&self, key: String, envelope: serde_json::Value) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        // A re-queued envelope is live again — its tombstone must not kill it.
        let d = digest(&envelope);
        state.tombstones.retain(|t| t != &d);
        state.entries.retain(|e| e.key != key);
        state.entries.push(OutboxEntry { key, envelope });
        if state.entries.len() > OUTBOX_CAP {
            let dropped = state.entries.remove(0);
            tracing::warn!(
                target: "think_and_ship::cloud",
                "outbox full ({OUTBOX_CAP}): dropping oldest queued push {}",
                dropped.key
            );
        }
        let snapshot = state.clone();
        drop(state);
        self.persist(&snapshot);
    }

    /// Snapshot the queue oldest-first (for a flush pass).
    #[must_use]
    pub fn snapshot(&self) -> Vec<OutboxEntry> {
        self.state
            .lock()
            .map(|s| s.entries.clone())
            .unwrap_or_default()
    }

    /// Remove one entry after a successful replay (only if the envelope is
    /// unchanged — a fresher enqueue for the same key must survive the flush).
    /// Leaves an envelope-exact tombstone so the locked merge with the disk
    /// copy cannot resurrect it.
    pub fn remove(&self, entry: &OutboxEntry) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state
            .entries
            .retain(|e| !(e.key == entry.key && e.envelope == entry.envelope));
        state.tombstones.push(digest(&entry.envelope));
        if state.tombstones.len() > TOMBSTONE_CAP {
            state.tombstones.remove(0);
        }
        let snapshot = state.clone();
        drop(state);
        self.persist(&snapshot);
    }

    /// How many pushes are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().map(|s| s.entries.len()).unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Try to take the flush guard; the caller MUST `end_flush` when done.
    pub(crate) fn begin_flush(&self) -> bool {
        !self.flushing.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn end_flush(&self) {
        self.flushing.store(false, Ordering::SeqCst);
    }

    fn persist(&self, snapshot: &OutboxState) {
        let Some(path) = &self.path else { return };
        if let Err(e) = locked_merge_write(path, snapshot, merge_outboxes) {
            tracing::warn!(
                target: "think_and_ship::cloud",
                "outbox persist failed (queue stays in memory): {e}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry_keys(outbox: &Outbox) -> Vec<String> {
        outbox.snapshot().into_iter().map(|e| e.key).collect()
    }

    #[test]
    fn latest_only_per_key_keeps_the_newest_envelope() {
        let outbox = Outbox::new(None);
        outbox.enqueue("ship/task/a".into(), json!({"v": 1}));
        outbox.enqueue("think/step/9".into(), json!({"v": 1}));
        outbox.enqueue("ship/task/a".into(), json!({"v": 2}));

        assert_eq!(outbox.len(), 2);
        let snap = outbox.snapshot();
        let a = snap.iter().find(|e| e.key == "ship/task/a").unwrap();
        assert_eq!(a.envelope, json!({"v": 2}));
    }

    #[test]
    fn overflow_drops_the_oldest() {
        let outbox = Outbox::new(None);
        for n in 0..=OUTBOX_CAP {
            outbox.enqueue(format!("think/step/{n}"), json!(n));
        }
        assert_eq!(outbox.len(), OUTBOX_CAP);
        assert!(!entry_keys(&outbox).contains(&"think/step/0".to_string()));
    }

    #[test]
    fn remove_only_drops_the_exact_envelope() {
        let outbox = Outbox::new(None);
        outbox.enqueue("ship/task/a".into(), json!({"v": 1}));
        let stale = outbox.snapshot()[0].clone();
        // A fresher enqueue lands while the flush is in flight…
        outbox.enqueue("ship/task/a".into(), json!({"v": 2}));
        // …so removing the replayed (stale) entry must keep the fresh one.
        outbox.remove(&stale);
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox.snapshot()[0].envelope, json!({"v": 2}));
    }

    #[test]
    fn a_flushed_entry_stays_dead_on_disk_and_across_reloads() {
        // The bug the cross-process proof found: union-by-key merge
        // resurrected removed entries from the disk copy.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.json");
        let outbox = Outbox::new(Some(path.clone()));
        outbox.enqueue("signal/signal/s1".into(), json!({"id": "s1"}));
        let entry = outbox.snapshot()[0].clone();
        outbox.remove(&entry);
        assert!(outbox.is_empty());

        // Another persist (e.g. a later enqueue of a DIFFERENT record) must
        // not bring s1 back from the disk copy…
        outbox.enqueue("think/step/2".into(), json!({"n": 2}));
        assert_eq!(entry_keys(&outbox), vec!["think/step/2"]);

        // …and a fresh process loading the file must not see it either.
        let reloaded = Outbox::new(Some(path));
        assert_eq!(entry_keys(&reloaded), vec!["think/step/2"]);

        // A NEWER envelope for the same record queues normally (digest differs).
        reloaded.enqueue("signal/signal/s1".into(), json!({"id": "s1", "v": 2}));
        assert_eq!(reloaded.len(), 2);
    }

    #[test]
    fn persists_and_reloads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.json");
        {
            let outbox = Outbox::new(Some(path.clone()));
            outbox.enqueue("roadmap/chunk/c1".into(), json!({"id": "c1"}));
        }
        let reloaded = Outbox::new(Some(path));
        assert_eq!(entry_keys(&reloaded), vec!["roadmap/chunk/c1"]);
    }

    #[test]
    fn flush_guard_is_exclusive() {
        let outbox = Outbox::new(None);
        assert!(outbox.begin_flush());
        assert!(!outbox.begin_flush());
        outbox.end_flush();
        assert!(outbox.begin_flush());
    }
}
