//! Read-side reconcile: pull the tenant's records from the cloud and merge them
//! into the local engines. The cloud is the system-of-record the
//! cache converges TOWARD, but per record the merge is NEWEST-wins (chunks:
//! `updated_at`; signals: lifecycle progress) — a stale cloud copy must never
//! clobber a fresher local mutation, the race a realtime refresh made live
//! (the reconcile recency guard). Local-only records are preserved.
//! The merge is SILENT (the engine's `upsert_*` does not re-emit), so a pull
//! never loops back into a push.
//!
//! Split into an async **fetch** (no engine access) and a sync **apply**
//! (`&mut Engine`, no awaits) so callers that share an engine behind a
//! `std::sync::Mutex` — the realtime subscriber — never
//! hold the guard across an await. The `reconcile_*` wrappers preserve the
//! original one-call shape for exclusive-engine callers and tests.
//!
//! This slice covers the think + roadmap + signal families (think joined in
//! sync-think-reconcile, which also added [`reconcile_all`] — the one-shot
//! boot hydrate). The ship family is `sync-ship-full`; offline write-queueing
//! is `sync-offline-queue`.

use std::sync::{Arc, Mutex};

use crate::cloud::client::{CloudClient, CloudError};
use crate::roadmap::RoadmapEngine;
use crate::roadmap::domain::{Chunk, TrackerLink, TrackerOptIn};
use crate::signal::SignalEngine;
use crate::signal::domain::Signal;
use crate::think::domain::ThinkStep;
use crate::think::engine::core::ReasoningServer;

/// Fetch the tenant's record envelopes for one family (`GET /v1/records?family=`).
/// Pure transport — no engine access, safe to await without holding any lock.
pub async fn fetch_family(
    client: &CloudClient,
    family: &str,
) -> Result<Vec<serde_json::Value>, CloudError> {
    client.list(Some(family), None).await
}

/// Incremental fetch: only records whose `updated >=` the watermark
/// (cloud-read-amplification). Pure transport.
pub async fn fetch_family_since(
    client: &CloudClient,
    family: &str,
    since: &str,
) -> Result<Vec<serde_json::Value>, CloudError> {
    client.list(Some(family), Some(since)).await
}

/// The newest change-cursor stamp in a batch of envelopes (`updated ?? created`)
/// — the next watermark after a merge. `None` for an empty batch.
#[must_use]
pub fn max_watermark(envelopes: &[serde_json::Value]) -> Option<String> {
    envelopes
        .iter()
        .filter_map(|e| {
            e.get("updated")
                .or_else(|| e.get("created"))
                .and_then(|v| v.as_str())
        })
        .max()
        .map(str::to_owned)
}

/// Does this envelope's record belong to `project_id`? (sync-project-scope)
///
/// The backend adopts the TOKEN's tenant, so with an org-scoped token every
/// project in the org shares one cloud namespace — `tenant_id` cannot tell
/// records apart. Push stamps `record.project_id` (envelope::owner); this is
/// the matching read-side guard. An UNSTAMPED record (pushed by a pre-stamp
/// binary) is treated as foreign: its origin is unknowable, and merging it is
/// exactly the cross-project bleed this exists to stop. Re-stamping the cloud
/// is one idempotent `sync push` per project.
fn record_is_local(envelope: &serde_json::Value, project_id: &str) -> bool {
    envelope
        .get("record")
        .and_then(|r| r.get("project_id"))
        .and_then(|v| v.as_str())
        == Some(project_id)
}

/// Read the tracker sidecar out of a chunk `record` and merge it in.
///
/// Silent when absent, which is the common case and also what every envelope
/// written before the sidecar existed looks like. Both halves are decoded
/// independently so a malformed opt-in cannot cost us the links.
fn adopt_tracker_sidecar(engine: &mut RoadmapEngine, record: &serde_json::Value) {
    let Some(sidecar) = record.get(crate::cloud::build::TRACKER_SIDECAR) else {
        return;
    };
    let links = sidecar
        .get("links")
        .and_then(|v| serde_json::from_value::<Vec<TrackerLink>>(v.clone()).ok())
        .unwrap_or_default();
    let opt_ins = sidecar
        .get("opt_ins")
        .and_then(|v| serde_json::from_value::<Vec<TrackerOptIn>>(v.clone()).ok())
        .unwrap_or_default();
    if links.is_empty() && opt_ins.is_empty() {
        return;
    }
    engine.adopt_tracker_state(links, opt_ins);
}

/// Merge fetched roadmap envelopes into `engine` (newest-wins by id; local-only
/// chunks preserved). Records from OTHER projects in the tenant — or unstamped
/// records of unknowable origin — are skipped (sync-project-scope). Returns the
/// number of chunks merged. A record that doesn't deserialize is logged and
/// skipped — one bad record never fails the whole reconcile. Synchronous:
/// never awaits.
pub fn apply_roadmap_records(engine: &mut RoadmapEngine, envelopes: &[serde_json::Value]) -> usize {
    let mut merged = 0;
    let mut foreign = 0;
    for envelope in envelopes {
        let Some(record) = envelope.get("record") else {
            continue;
        };
        if !record_is_local(envelope, engine.project_id()) {
            foreign += 1;
            continue;
        }
        match serde_json::from_value::<Chunk>(record.clone()) {
            Ok(mut chunk) => {
                // The wire stamped `record.project_id`; keep it on the chunk we
                // store. Dropping it here is what left the local store with no
                // record-intrinsic origin, so the 2026-06 bleed could only be
                // cleaned up by guessing from id prefixes.
                if chunk.project_id.is_none() {
                    chunk.project_id = record
                        .get("project_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                }
                engine.upsert_chunk(chunk);
                // The tracker sidecar rides `record` alongside the chunk's own
                // fields; serde ignored it above (Chunk has no such field), so
                // it is read out explicitly here. Malformed tracker state is
                // skipped without failing the chunk — the chunk is the record,
                // the sidecar is an addendum.
                adopt_tracker_sidecar(engine, record);
                merged += 1;
            }
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "skipping malformed roadmap record on reconcile: {e}"
            ),
        }
    }
    if foreign > 0 {
        tracing::debug!(
            target: "think_and_ship::cloud",
            "roadmap reconcile skipped {foreign} record(s) from other projects in the tenant"
        );
    }
    merged
}

/// Merge fetched signal envelopes into `engine` (newest-wins by id; local-only
/// signals preserved; other projects' records skipped, sync-project-scope).
/// Returns the number merged. Synchronous: never awaits.
pub fn apply_signal_records(engine: &mut SignalEngine, envelopes: &[serde_json::Value]) -> usize {
    let mut merged = 0;
    let mut foreign = 0;
    for envelope in envelopes {
        let Some(record) = envelope.get("record") else {
            continue;
        };
        if !record_is_local(envelope, engine.project_id()) {
            foreign += 1;
            continue;
        }
        match serde_json::from_value::<Signal>(record.clone()) {
            Ok(signal) => {
                engine.upsert_signal(signal);
                merged += 1;
            }
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "skipping malformed signal record on reconcile: {e}"
            ),
        }
    }
    if foreign > 0 {
        tracing::debug!(
            target: "think_and_ship::cloud",
            "signal reconcile skipped {foreign} record(s) from other projects in the tenant"
        );
    }
    merged
}

/// Merge fetched think envelopes into `engine` (insert-if-absent by
/// step_number — an existing local step is never replaced; see
/// `ReasoningServer::adopt_steps`). Other projects' records are skipped
/// (sync-project-scope) — CRITICAL here, because step numbers are
/// project-global: adopting a foreign project's step 1240 silently poisons
/// this project's numbering. Returns the number ADOPTED (not merely seen —
/// unlike the other families, an already-known step counts zero).
/// Synchronous: never awaits.
pub fn apply_think_records(engine: &mut ReasoningServer, envelopes: &[serde_json::Value]) -> usize {
    let mut steps = Vec::new();
    let mut foreign = 0;
    for envelope in envelopes {
        let Some(record) = envelope.get("record") else {
            continue;
        };
        if !record_is_local(envelope, engine.project_id()) {
            foreign += 1;
            continue;
        }
        match serde_json::from_value::<ThinkStep>(record.clone()) {
            Ok(step) => steps.push(step),
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "skipping malformed think record on reconcile: {e}"
            ),
        }
    }
    if foreign > 0 {
        tracing::debug!(
            target: "think_and_ship::cloud",
            "think reconcile skipped {foreign} record(s) from other projects in the tenant"
        );
    }
    engine.adopt_steps(steps)
}

/// Pull the tenant's think steps from the cloud and merge them into `engine`
/// (fetch + apply in one call, for callers with exclusive engine access).
pub async fn reconcile_think(
    client: &CloudClient,
    engine: &mut ReasoningServer,
) -> Result<usize, CloudError> {
    let envelopes = fetch_family(client, "think").await?;
    Ok(apply_think_records(engine, &envelopes))
}

/// One-shot reconcile of every pull-able family (think + roadmap + signal)
/// into the shared engines — the startup hydrate (sync-think-reconcile): a
/// fresh machine or empty data dir converges to the workspace on boot. Each
/// family fetches BEFORE locking and applies synchronously (the cloud::events
/// discipline — an engine mutex is never held across an await). A family
/// that fails to fetch is logged and skipped; the others still reconcile.
/// Returns (think, roadmap, signal) merge counts.
pub async fn reconcile_all(
    client: &CloudClient,
    think: &Arc<Mutex<ReasoningServer>>,
    roadmap: &Arc<Mutex<RoadmapEngine>>,
    signal: &Arc<Mutex<SignalEngine>>,
) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    match fetch_family(client, "think").await {
        Ok(envelopes) => {
            if let Ok(mut engine) = think.lock() {
                counts.0 = apply_think_records(&mut engine, &envelopes);
            }
        }
        Err(e) => tracing::warn!(
            target: "think_and_ship::cloud",
            "startup think reconcile failed: {e}"
        ),
    }
    match fetch_family(client, "roadmap").await {
        Ok(envelopes) => {
            if let Ok(mut engine) = roadmap.lock() {
                counts.1 = apply_roadmap_records(&mut engine, &envelopes);
            }
        }
        Err(e) => tracing::warn!(
            target: "think_and_ship::cloud",
            "startup roadmap reconcile failed: {e}"
        ),
    }
    match fetch_family(client, "signal").await {
        Ok(envelopes) => {
            if let Ok(mut engine) = signal.lock() {
                counts.2 = apply_signal_records(&mut engine, &envelopes);
            }
        }
        Err(e) => tracing::warn!(
            target: "think_and_ship::cloud",
            "startup signal reconcile failed: {e}"
        ),
    }
    counts
}

/// Pull the tenant's roadmap chunks from the cloud and merge them into `engine`
/// (fetch + apply in one call, for callers with exclusive engine access).
pub async fn reconcile_roadmap(
    client: &CloudClient,
    engine: &mut RoadmapEngine,
) -> Result<usize, CloudError> {
    let envelopes = fetch_family(client, "roadmap").await?;
    Ok(apply_roadmap_records(engine, &envelopes))
}

/// Pull the tenant's signals from the cloud and merge them into `engine`
/// (fetch + apply in one call, for callers with exclusive engine access).
pub async fn reconcile_signals(
    client: &CloudClient,
    engine: &mut SignalEngine,
) -> Result<usize, CloudError> {
    let envelopes = fetch_family(client, "signal").await?;
    Ok(apply_signal_records(engine, &envelopes))
}

#[cfg(test)]
mod watermark_tests {
    use super::max_watermark;
    use serde_json::json;

    #[test]
    fn max_watermark_prefers_updated_and_takes_the_newest() {
        let envs = vec![
            json!({"created": "2026-06-01T00:00:00Z"}),
            json!({"created": "2026-06-01T00:00:00Z", "updated": "2026-06-11T09:00:00Z"}),
            json!({"updated": "2026-06-10T00:00:00Z"}),
        ];
        assert_eq!(
            max_watermark(&envs).as_deref(),
            Some("2026-06-11T09:00:00Z")
        );
    }

    #[test]
    fn max_watermark_empty_is_none() {
        assert_eq!(max_watermark(&[]), None);
    }
}
