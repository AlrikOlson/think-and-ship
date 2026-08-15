//! Build unified record envelopes from local domain records.
//! Each builder maps a family's domain object to the envelope: the object
//! itself becomes `record`, and its links become graph `edges`.

use crate::cloud::envelope::{Edge, Family, Kind, UnifiedRecordEnvelope, edges_from_cross_refs};
use crate::roadmap::domain::{Chunk, TrackerLink, TrackerOptIn};
use crate::ship::domain::action::Action;
use crate::ship::domain::check::Check;
use crate::ship::domain::objective::Objective;
use crate::ship::domain::task::Task;
use crate::signal::domain::Signal;
use crate::think::domain::step::ThinkStep;

/// The cycle key — the stable wire identity of one ship cycle, derived from
/// the objective's `created_at` (the same identity the disk merge uses). All
/// four ship kinds prefix their wire ids with it, so ids never collide across
/// cycles (sync-ship-full — the bare numeric action ids collided LIVE: every
/// cycle's `action 1` overwrote the previous one's). Characters outside the
/// contract id alphabet (`[A-Za-z0-9:._-]`) are mapped to `-`.
#[must_use]
pub fn cycle_key(objective_created_at: &str) -> String {
    let sanitized: String = objective_created_at
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("obj-{sanitized}")
}

/// Extract the `chunk:<slug>` backref the /roadmap loop embeds in objective
/// text, so the objective record carries a graph edge to its roadmap chunk.
fn chunk_backref(text: &str) -> Option<String> {
    let start = text.find("chunk:")?;
    let slug: String = text[start + "chunk:".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    (!slug.is_empty()).then(|| format!("chunk:{slug}"))
}

/// Build the cloud envelope for a ship objective. Wire id = the cycle key;
/// re-push after status changes (defined → shipped) updates in place.
#[must_use]
pub fn from_objective(tenant: &str, cycle: &str, objective: &Objective) -> UnifiedRecordEnvelope {
    let record = serde_json::to_value(objective).unwrap_or(serde_json::Value::Null);
    let mut edges = Vec::new();
    if let Some(backref) =
        chunk_backref(&objective.description).or_else(|| chunk_backref(&objective.scope))
    {
        edges.push(Edge {
            target: backref,
            relation: Some("realizes".into()),
        });
    }
    let created = objective.created_at.clone().unwrap_or_default();
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Ship,
        Kind::Objective,
        cycle.to_string(),
        created,
        record,
        edges,
    )
}

/// Build the cloud envelope for a ship task. Wire id = `<cycle>.<task_id>`;
/// every lifecycle transition re-pushes the full task state in place. The
/// store requires `record.id == envelope.id`, so the payload carries the wire
/// id and keeps the per-cycle task id as `local_id`. `created` falls back to
/// the objective's clock for tasks that haven't started yet.
#[must_use]
pub fn from_task(
    tenant: &str,
    cycle: &str,
    objective_created_at: &str,
    task: &Task,
) -> UnifiedRecordEnvelope {
    let wire_id = format!("{cycle}.{}", task.id);
    let mut record = serde_json::to_value(task).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = record.as_object_mut() {
        obj.insert(
            "local_id".into(),
            serde_json::Value::String(task.id.clone()),
        );
        obj.insert("id".into(), serde_json::Value::String(wire_id.clone()));
    }
    let edges = vec![Edge {
        target: format!("objective:{cycle}"),
        relation: Some("part_of".into()),
    }];
    let created = task
        .started_at
        .clone()
        .unwrap_or_else(|| objective_created_at.to_string());
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Ship,
        Kind::Task,
        wire_id,
        created,
        record,
        edges,
    )
}

/// Build the cloud envelope for a quality-gate check. Checks have no id of
/// their own — identity is the append-only index within the owning task
/// (`<cycle>.<task_id>.check-<seq>`), stable across re-pushes; `created` is
/// the check's own timestamp.
#[must_use]
pub fn from_check(
    tenant: &str,
    cycle: &str,
    task_id: &str,
    seq: usize,
    check: &Check,
) -> UnifiedRecordEnvelope {
    let record = serde_json::to_value(check).unwrap_or(serde_json::Value::Null);
    let edges = vec![Edge {
        target: format!("task:{cycle}.{task_id}"),
        relation: Some("verifies".into()),
    }];
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Ship,
        Kind::Check,
        format!("{cycle}.{task_id}.check-{seq}"),
        check.timestamp.clone(),
        record,
        edges,
    )
}

/// Build the cloud envelope for an approval gate (webapp-approval-gates).
/// Unlike the other ship kinds, a gate's id is its own UUID rather than a
/// cycle-scoped key: the gate is born ON the cloud (it exists to be answered
/// from a browser) and its id must survive `ship_reset` and cycle turnover
/// while an agent is still waiting on it.
#[must_use]
pub fn from_gate(
    tenant: &str,
    gate: &crate::ship::gate::Gate,
    task_id: Option<&str>,
) -> UnifiedRecordEnvelope {
    let record = serde_json::to_value(gate).unwrap_or(serde_json::Value::Null);
    let edges = task_id
        .map(|tid| {
            vec![Edge {
                target: format!("task:{tid}"),
                relation: Some("part_of".into()),
            }]
        })
        .unwrap_or_default();
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Ship,
        Kind::Gate,
        gate.id.clone(),
        gate.opened_at.clone(),
        record,
        edges,
    )
}

/// The key the chunk's tracker state travels under inside `record`.
pub const TRACKER_SIDECAR: &str = "tracker";

/// Build the cloud envelope for a roadmap chunk — the chunk as `record`, its
/// `cross_refs[]` as graph edges, and its tracker state as a sidecar.
///
/// # Why the tracker state is a sidecar and not a `Chunk` field
///
/// [`TrackerLink`] and [`TrackerOptIn`] are keyed by `(chunk_id, provider)` and
/// live on [`Roadmap`], not on [`Chunk`] — deliberately, so `tracker` never
/// learns that roadmaps exist. But they still have to reach the other machine:
/// without the link, a second machine has no binding, mints its own twin, and
/// the chunk ends up with two tickets; without the opt-in, it does not know the
/// chunk is in scope at all.
///
/// A new cloud record kind would be the tidy answer and is the wrong one: the
/// reconcile path silently SKIPS records whose kind it does not recognize, so
/// any client older than the change would drop tracker state without saying so.
/// Riding the chunk envelope is additive instead — `record` is
/// `additionalProperties`-open in the contract, and the read side deserializes
/// `record` into [`Chunk`], where serde ignores unknown fields. So an old client
/// reading a new envelope simply does not see the sidecar, which is the correct
/// degradation.
///
/// The cost, paid in [`RoadmapEngine::touch_chunk_for_link`]: both sides resolve
/// chunks by strict `updated_at` recency, so a tracker write must bump the
/// chunk's stamp or the peer declines the envelope as not-newer.
///
/// [`TrackerLink`]: crate::roadmap::domain::TrackerLink
/// [`TrackerOptIn`]: crate::roadmap::domain::TrackerOptIn
/// [`Roadmap`]: crate::roadmap::domain::Roadmap
/// [`RoadmapEngine::touch_chunk_for_link`]: crate::roadmap::engine::RoadmapEngine
#[must_use]
pub fn from_chunk(
    tenant: &str,
    chunk: &Chunk,
    links: &[TrackerLink],
    opt_ins: &[TrackerOptIn],
) -> UnifiedRecordEnvelope {
    let mut record = serde_json::to_value(chunk).unwrap_or(serde_json::Value::Null);
    mark_unprovable_origin(&mut record, chunk.project_id.as_ref());
    if (!links.is_empty() || !opt_ins.is_empty())
        && let Some(obj) = record.as_object_mut()
    {
        obj.insert(
            TRACKER_SIDECAR.to_string(),
            serde_json::json!({ "links": links, "opt_ins": opt_ins }),
        );
    }
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Roadmap,
        Kind::Chunk,
        chunk.id.clone(),
        chunk.created_at.clone(),
        record,
        edges_from_cross_refs(&chunk.cross_refs),
    )
}

/// Send an UNPROVABLE origin across the wire as unprovable, instead of letting
/// the pusher's identity fill the gap.
///
/// `Chunk::project_id` and `Signal::project_id` are `Option`, and both carry
/// `skip_serializing_if = "Option::is_none"`, so a record with no origin reaches
/// [`UnifiedRecordEnvelope::owner`] with the key ABSENT — where
/// `.entry().or_insert_with(tenant)` helpfully supplies the pushing project.
/// That is how a record of unknown origin becomes provably ours.
///
/// It is not hypothetical. On 2026-06-15 a cleanup in another project obsoleted
/// 22 chunks that had bled in from this one. Obsoleting is a mutation, sync is
/// write-through on mutation, and the push stamped every one of them with the
/// cleaning project's id — so the act of retiring the intruders is what made
/// them permanently the cleaner's own, self-consistent and invisible to `prune`,
/// which by design only removes what provably belongs to someone else.
///
/// `or_insert_with` cannot tell "unstamped because it predates the stamp and is
/// genuinely mine" from "unstamped because it bled in from elsewhere" — both are
/// `None`. So neither may be claimed here. Writing an explicit null keeps the
/// key present (so the stamp does not fire) and keeps it non-matching for
/// [`crate::cloud::pull`]'s `record_is_local`, which compares `as_str()`.
///
/// The store already refuses to DELETE on the strength of a `None`
/// ([`crate::cli::store_health`]); this is the same refusal in the other
/// direction, and `adopt` remains the one deliberate path from `None` to ours.
fn mark_unprovable_origin(record: &mut serde_json::Value, origin: Option<&String>) {
    if origin.is_none()
        && let Some(obj) = record.as_object_mut()
    {
        obj.insert("project_id".to_string(), serde_json::Value::Null);
    }
}

/// Build the cloud envelope for a signal — the signal as `record`, its
/// `cross_refs[]` as graph edges. (The local-only `surfaced_at`/`snooze_until`
/// fields are omitted when unset and ignored by the backend.)
#[must_use]
pub fn from_signal(tenant: &str, signal: &Signal) -> UnifiedRecordEnvelope {
    let mut record = serde_json::to_value(signal).unwrap_or(serde_json::Value::Null);
    mark_unprovable_origin(&mut record, signal.project_id.as_ref());
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Signal,
        Kind::Signal,
        signal.id.clone(),
        signal.created.clone(),
        record,
        edges_from_cross_refs(&signal.cross_refs),
    )
}

/// Build the cloud envelope for a ship action — the action as `record`, with a
/// `task:<id>` edge to its task and a labeled `think:<n>` edge to the reasoning
/// step it derives from (when set). The richest edge case in the graph.
#[must_use]
pub fn from_action(tenant: &str, cycle: &str, action: &Action) -> UnifiedRecordEnvelope {
    let wire_id = format!("{cycle}.action-{}", action.id);
    let mut record = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = record.as_object_mut() {
        obj.insert("local_id".into(), serde_json::Value::from(action.id));
        obj.insert("id".into(), serde_json::Value::String(wire_id.clone()));
    }
    let mut edges = vec![Edge::to(format!("task:{cycle}.{}", action.task_id))];
    if let Some(step) = action.think_step {
        edges.push(Edge {
            target: format!("think:{step}"),
            relation: Some("realizes".into()),
        });
    }
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Ship,
        Kind::Action,
        wire_id,
        action.timestamp.clone(),
        record,
        edges,
    )
}

/// Build the cloud envelope for a reasoning step — the step as `record`, with a
/// labeled `think:<n>` edge per dependency (the reasoning side's dependency
/// graph). A dependency's relation is kept only if it's one of the schema's
/// think relations (`supports`/`refutes`/`depends_on`); any other value is
/// dropped to unlabeled, mirroring the engine's allowlist.
#[must_use]
pub fn from_step(tenant: &str, step: &ThinkStep) -> UnifiedRecordEnvelope {
    let record = serde_json::to_value(step).unwrap_or(serde_json::Value::Null);
    let edges = step
        .dependencies
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|d| Edge {
            target: format!("think:{}", d.step()),
            relation: d
                .relation()
                .filter(|r| matches!(*r, "supports" | "refutes" | "depends_on"))
                .map(str::to_string),
        })
        .collect();
    UnifiedRecordEnvelope::owner(
        tenant,
        Family::Think,
        Kind::Step,
        // The immutable record id is the wire identity; step_number is only
        // the human-facing display number and may be renumbered locally
        // (think-step-stable-id). Number fallback covers content-keyless
        // steps that never got an id.
        step.record_id
            .clone()
            .unwrap_or_else(|| step.step_number.to_string()),
        step.timestamp.clone().unwrap_or_default(),
        record,
        edges,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap::domain::{Chunk, ChunkStatus};

    fn chunk() -> Chunk {
        Chunk {
            tier: None,
            id: "saas-cloud-sync-roadmap".into(),
            title: "t".into(),
            name: crate::roadmap::name::derive("saas-cloud-sync-roadmap"),
            status: ChunkStatus::InProgress,
            priority: 428,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec!["saas-cloud-client".into()],
            cross_refs: vec!["think:3".into(), "signal:abc".into()],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: None,
            created_at: "2026-06-08T00:00:00Z".into(),
            updated_at: "2026-06-08T00:00:00Z".into(),
        }
    }

    fn validator() -> jsonschema::Validator {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/unified-record-envelope.schema.json"
        );
        let schema: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    fn assert_schema_valid(envelope: &UnifiedRecordEnvelope) {
        let value = serde_json::to_value(envelope).unwrap();
        let errors: Vec<String> = validator()
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }

    #[test]
    fn from_chunk_builds_a_schema_valid_envelope_with_edges() {
        let envelope = from_chunk("think-and-ship-676f38", &chunk(), &[], &[]);
        assert_eq!(envelope.edges.len(), 2);
        assert_eq!(envelope.edges[0].target, "think:3");
        assert_eq!(envelope.id, "saas-cloud-sync-roadmap");
        assert_schema_valid(&envelope);
    }

    /// A chunk with no tracker state must serialize to the EXACT bytes it did
    /// before the sidecar existed. If an empty `tracker: {}` key appeared, every
    /// unprojected chunk in every store would look changed.
    #[test]
    fn a_chunk_with_no_tracker_state_carries_no_sidecar() {
        let envelope = from_chunk("t", &chunk(), &[], &[]);
        assert!(
            envelope.record.get(TRACKER_SIDECAR).is_none(),
            "silence must be byte-identical to the pre-sidecar wire form"
        );
        assert_schema_valid(&envelope);
    }

    /// The link is what stops a second machine minting a twin, so it has to
    /// reach the wire — and the envelope has to stay contract-valid carrying it,
    /// since `record` is the one open part of the schema.
    #[test]
    fn tracker_state_rides_the_chunk_envelope() {
        let link = TrackerLink {
            chunk_id: "saas-cloud-sync-roadmap".into(),
            provider: "github".into(),
            external_id: "owner/repo#12".into(),
            our_last_write_hash: "abc".into(),
            last_seen_version: Some("3".into()),
            our_last_relations_hash: None,
            our_last_authored_hash: None,
            created_at: "2026-07-25T09:00:00Z".into(),
            updated_at: "2026-07-25T09:00:00Z".into(),
        };
        let opt_in = TrackerOptIn {
            chunk_id: "saas-cloud-sync-roadmap".into(),
            provider: "github".into(),
            enabled: true,
            updated_at: "2026-07-25T09:00:00Z".into(),
        };
        let envelope = from_chunk(
            "t",
            &chunk(),
            std::slice::from_ref(&link),
            std::slice::from_ref(&opt_in),
        );

        let sidecar = envelope
            .record
            .get(TRACKER_SIDECAR)
            .expect("tracker state must travel");
        let links: Vec<TrackerLink> =
            serde_json::from_value(sidecar["links"].clone()).expect("links round-trip");
        let opt_ins: Vec<TrackerOptIn> =
            serde_json::from_value(sidecar["opt_ins"].clone()).expect("opt-ins round-trip");
        assert_eq!(links, vec![link]);
        assert_eq!(opt_ins, vec![opt_in]);

        // The sidecar must not disturb the parts the contract DOES police.
        assert_eq!(envelope.id, "saas-cloud-sync-roadmap");
        assert_schema_valid(&envelope);
    }

    /// An old client reading a new envelope must still get its chunk: `record`
    /// deserializes into `Chunk`, and serde ignores the unknown sidecar key.
    /// That degradation is the whole reason this is not a new record kind.
    #[test]
    fn an_older_client_still_reads_the_chunk_through_the_sidecar() {
        let link = TrackerLink {
            chunk_id: "saas-cloud-sync-roadmap".into(),
            provider: "github".into(),
            external_id: "owner/repo#12".into(),
            our_last_write_hash: "abc".into(),
            last_seen_version: None,
            our_last_relations_hash: None,
            our_last_authored_hash: None,
            created_at: "2026-07-25T09:00:00Z".into(),
            updated_at: "2026-07-25T09:00:00Z".into(),
        };
        let envelope = from_chunk("t", &chunk(), &[link], &[]);
        let decoded: Chunk =
            serde_json::from_value(envelope.record.clone()).expect("chunk still decodes");
        assert_eq!(decoded.id, "saas-cloud-sync-roadmap");
    }

    #[test]
    fn from_signal_builds_a_schema_valid_envelope_with_edges() {
        use crate::signal::domain::{SignalKind, SignalStatus};
        let signal = Signal {
            id: "b3e1c0de-0000-4000-8000-000000000000".into(),
            kind: SignalKind::Bug,
            from: "dana".into(),
            body: "it crashes on save".into(),
            content: None,
            created: "2026-06-08T00:00:00Z".into(),
            status: SignalStatus::New,
            enrichment: vec![],
            cross_refs: vec!["chunk:saas-cloud-sync-signal".into(), "think:5".into()],
            surfaced_at: None,
            snooze_until: None,
            project_id: None,
        };
        let envelope = from_signal("think-and-ship-676f38", &signal);
        assert_eq!(envelope.edges.len(), 2);
        assert_eq!(envelope.edges[0].target, "chunk:saas-cloud-sync-signal");
        assert_eq!(envelope.id, signal.id);
        assert_schema_valid(&envelope);
    }

    #[test]
    fn from_action_builds_a_schema_valid_envelope_with_task_and_think_edges() {
        use crate::ship::domain::action::{Action, ActionType};
        let action = Action {
            id: 42,
            task_id: "impl-shipcloud".into(),
            timestamp: "2026-06-08T00:00:00Z".into(),
            action_type: ActionType::Code,
            description: "wired the hook".into(),
            files_touched: vec!["src/ship/engine/mod.rs".into()],
            tools_used: vec!["Edit".into()],
            result: String::new(),
            think_step: Some(7),
        };
        let envelope = from_action("think-and-ship-676f38", "obj-2026-06-08T00:00:00Z", &action);
        assert_eq!(envelope.id, "obj-2026-06-08T00:00:00Z.action-42");
        assert_eq!(envelope.record["id"], envelope.id);
        assert_eq!(envelope.record["local_id"], 42);
        assert_eq!(envelope.edges.len(), 2);
        assert_eq!(
            envelope.edges[0].target,
            "task:obj-2026-06-08T00:00:00Z.impl-shipcloud"
        );
        assert_eq!(envelope.edges[1].target, "think:7");
        assert_eq!(envelope.edges[1].relation.as_deref(), Some("realizes"));
        assert_schema_valid(&envelope);
    }

    /// EVERY synced family stamps `record.project_id` on push, and a family
    /// that already carries its own keeps it (sync-project-scope).
    ///
    /// This is the push half of the org-tenant bleed, and it had no test at
    /// all: deleting the stamp from `Envelope::new` left all 76 cloud tests
    /// green, because schema validity does not require the field and every
    /// other assertion is about ids and edges. The read-side filter is well
    /// gated and is the half that LOOKS load-bearing, but the two are one
    /// mechanism — with the stamp gone the filter rejects every record as
    /// unstamped and cloud sync degrades to a silent no-op in both directions.
    ///
    /// It walks the builders rather than picking one, so a family added later
    /// that routes around `Envelope::new` fails here instead of bleeding.
    #[test]
    fn every_family_stamps_the_source_project_on_the_record() {
        use crate::ship::domain::action::{Action, ActionType};
        use crate::ship::domain::check::{Check, CheckType};
        use crate::ship::domain::objective::{Objective, ObjectiveStatus};
        use crate::ship::domain::task::{Task, TaskStatus, TaskType};
        use crate::signal::domain::{SignalKind, SignalStatus};
        use crate::think::domain::step::ThinkStep;
        use serde_json::json;

        const US: &str = "think-and-ship-676f38";
        let cycle = "obj-2026-06-08T00:00:00Z";

        let signal = Signal {
            id: "b3e1c0de-0000-4000-8000-000000000000".into(),
            kind: SignalKind::Bug,
            from: "dana".into(),
            body: "it crashes on save".into(),
            content: None,
            created: "2026-06-08T00:00:00Z".into(),
            status: SignalStatus::New,
            enrichment: vec![],
            cross_refs: vec![],
            surfaced_at: None,
            snooze_until: None,
            project_id: None,
        };
        let step: ThinkStep = serde_json::from_value(json!({
            "step_number": 909,
            "estimated_total": 910,
            "purpose": "x",
            "thought": "y",
            "outcome": "z",
            "timestamp": "2026-06-08T00:00:00Z"
        }))
        .expect("step fixture");
        let action = Action {
            id: 42,
            task_id: "impl-x".into(),
            timestamp: "2026-06-08T00:00:00Z".into(),
            action_type: ActionType::Code,
            description: "wired the hook".into(),
            files_touched: vec![],
            tools_used: vec![],
            result: String::new(),
            think_step: None,
        };
        let task = Task {
            id: "impl-x".into(),
            title: "Implement the thing".into(),
            task_type: TaskType::Implement,
            status: TaskStatus::Active,
            estimate: None,
            started_at: Some("2026-06-08T00:00:00Z".into()),
            completed_at: None,
            artifacts: vec![],
            checks: vec![],
            actions: vec![],
            blocked_reason: None,
            think_branch: None,
        };
        let check = Check {
            check_type: CheckType::Test,
            name: "cargo test".into(),
            passed: true,
            details: String::new(),
            required: true,
            timestamp: "2026-06-08T00:00:00Z".into(),
            command: None,
            exit_code: None,
            verified: true,
            report: None,
            results: None,
        };

        // The two families whose origin is an `Option` must SAY they are ours;
        // the push no longer supplies it for them (see `mark_unprovable_origin`),
        // so a fixture that stayed `None` would now be asserting the laundering.
        let owned_chunk = Chunk {
            project_id: Some(US.into()),
            ..chunk()
        };
        let owned_signal = Signal {
            project_id: Some(US.into()),
            ..signal.clone()
        };

        let built: Vec<(&str, UnifiedRecordEnvelope)> = vec![
            ("roadmap chunk", from_chunk(US, &owned_chunk, &[], &[])),
            ("signal", from_signal(US, &owned_signal)),
            ("think step", from_step(US, &step)),
            ("ship action", from_action(US, cycle, &action)),
            (
                "ship task",
                from_task(US, cycle, "2026-06-08T00:00:00Z", &task),
            ),
            (
                "ship check",
                from_check(US, cycle, "2026-06-08T00:00:00Z", 0, &check),
            ),
        ];
        for (family, envelope) in &built {
            assert_eq!(
                envelope.record.get("project_id").and_then(|v| v.as_str()),
                Some(US),
                "{family} pushed a record with no source project: with tenant_id adopted by the \
                 backend, nothing downstream can tell it from another project's"
            );
        }

        // The other half: a family whose domain object already answers "which
        // project" keeps its OWN answer. Ship objectives carry project_id, and
        // overwriting it with the pushing tenant would relabel a record that
        // already knew better.
        let objective = Objective {
            description: "do the thing".into(),
            acceptance_criteria: vec![],
            constraints: vec![],
            scope: String::new(),
            status: ObjectiveStatus::Defined,
            project_id: "some-other-project".into(),
            created_at: Some("2026-06-08T00:00:00Z".into()),
            completed_at: None,
        };
        let obj_env = from_objective(US, cycle, &objective);
        assert_eq!(
            obj_env.record.get("project_id").and_then(|v| v.as_str()),
            Some("some-other-project"),
            "the stamp overwrote a project identity the record already carried"
        );

        // THE THIRD CASE, and the one that actually bled: a record whose origin
        // is UNPROVABLE must cross the wire as explicitly unprovable. Filling it
        // in with the pusher is how 22 chunks became permanently another
        // project's — see `mark_unprovable_origin`. `None` is not evidence of
        // ownership in either direction: the store refuses to delete on it, so
        // it must refuse to claim on it too.
        for (family, envelope) in [
            ("roadmap chunk", from_chunk(US, &chunk(), &[], &[])),
            ("signal", from_signal(US, &signal)),
        ] {
            assert_eq!(
                envelope.record.get("project_id"),
                Some(&serde_json::Value::Null),
                "{family}: an unprovable origin was laundered into the pusher's own"
            );
        }
    }

    #[test]
    fn cycle_key_is_contract_safe_and_deterministic() {
        // '+' is outside the id alphabet and must map to '-'.
        assert_eq!(
            cycle_key("2026-06-10T02:51:47.620058+00:00"),
            "obj-2026-06-10T02:51:47.620058-00:00"
        );
        assert_eq!(
            cycle_key("2026-06-10T02:51:47Z"),
            "obj-2026-06-10T02:51:47Z"
        );
    }

    #[test]
    fn ship_cycle_builders_emit_schema_valid_cycle_scoped_envelopes() {
        use crate::ship::domain::check::{Check, CheckType};
        use crate::ship::domain::objective::{Objective, ObjectiveStatus};
        use crate::ship::domain::task::{Task, TaskStatus, TaskType};

        let objective = Objective {
            description: "do the thing. chunk:sync-ship-full".into(),
            acceptance_criteria: vec!["it works".into()],
            constraints: vec![],
            scope: String::new(),
            status: ObjectiveStatus::Defined,
            project_id: "think-and-ship-676f38".into(),
            created_at: Some("2026-06-10T02:51:47.620058+00:00".into()),
            completed_at: None,
        };
        let cycle = cycle_key(objective.created_at.as_deref().unwrap());

        let obj_env = from_objective("think-and-ship-676f38", &cycle, &objective);
        assert_eq!(obj_env.id, cycle);
        // The chunk: backref embedded in the description becomes a graph edge.
        assert_eq!(obj_env.edges.len(), 1);
        assert_eq!(obj_env.edges[0].target, "chunk:sync-ship-full");
        assert_eq!(obj_env.edges[0].relation.as_deref(), Some("realizes"));
        assert_schema_valid(&obj_env);

        let task = Task {
            id: "implement-x".into(),
            title: "Implement the thing".into(),
            task_type: TaskType::Implement,
            status: TaskStatus::Active,
            estimate: None,
            started_at: Some("2026-06-10T02:55:00Z".into()),
            completed_at: None,
            artifacts: vec![],
            checks: vec![],
            actions: vec![],
            blocked_reason: None,
            think_branch: None,
        };
        let task_env = from_task(
            "think-and-ship-676f38",
            &cycle,
            objective.created_at.as_deref().unwrap(),
            &task,
        );
        assert_eq!(task_env.id, format!("{cycle}.implement-x"));
        // The store enforces record.id == envelope.id; the local id survives.
        assert_eq!(task_env.record["id"], task_env.id);
        assert_eq!(task_env.record["local_id"], "implement-x");
        assert_eq!(task_env.edges[0].target, format!("objective:{cycle}"));
        assert_schema_valid(&task_env);

        // A not-yet-started task still gets a valid `created` (the objective's clock).
        let unstarted = Task {
            started_at: None,
            ..task.clone()
        };
        let unstarted_env = from_task(
            "think-and-ship-676f38",
            &cycle,
            objective.created_at.as_deref().unwrap(),
            &unstarted,
        );
        assert_eq!(unstarted_env.created, "2026-06-10T02:51:47.620058+00:00");
        assert_schema_valid(&unstarted_env);

        // Check names contain spaces — the id never embeds the name.
        let check = Check {
            check_type: CheckType::Test,
            name: "cargo test + clippy".into(),
            passed: true,
            details: "791 pass".into(),
            required: true,
            verified: true,
            command: Some("cargo test".into()),
            exit_code: Some(0),
            report: None,
            results: None,
            timestamp: "2026-06-10T02:56:00Z".into(),
        };
        let check_env = from_check("think-and-ship-676f38", &cycle, "implement-x", 0, &check);
        assert_eq!(check_env.id, format!("{cycle}.implement-x.check-0"));
        assert_eq!(
            check_env.edges[0].target,
            format!("task:{cycle}.implement-x")
        );
        assert_schema_valid(&check_env);

        // Re-building the same logical entity yields the same identity (the
        // idempotent-upsert property the collision bug lacked).
        assert_eq!(
            from_task(
                "think-and-ship-676f38",
                &cycle,
                objective.created_at.as_deref().unwrap(),
                &task
            )
            .idempotency_key,
            task_env.idempotency_key
        );
    }

    #[test]
    fn from_step_builds_a_schema_valid_envelope_with_normalized_dep_edges() {
        use crate::think::domain::step::ThinkStep;
        use serde_json::json;
        let step: ThinkStep = serde_json::from_value(json!({
            "step_number": 909,
            "estimated_total": 910,
            "purpose": "x",
            "thought": "y",
            "outcome": "z",
            "dependencies": [8, {"step": 1, "relation": "supports"}, {"step": 2, "relation": "weird"}],
            "timestamp": "2026-06-08T00:00:00Z"
        }))
        .unwrap();
        let envelope = from_step("think-and-ship-676f38", &step);
        assert_eq!(envelope.id, "909");
        assert_eq!(envelope.edges.len(), 3);
        // bare dep → unlabeled
        assert_eq!(envelope.edges[0].target, "think:8");
        assert_eq!(envelope.edges[0].relation, None);
        // tagged, schema-valid relation → kept
        assert_eq!(envelope.edges[1].target, "think:1");
        assert_eq!(envelope.edges[1].relation.as_deref(), Some("supports"));
        // tagged, out-of-enum relation → dropped to unlabeled
        assert_eq!(envelope.edges[2].target, "think:2");
        assert_eq!(envelope.edges[2].relation, None);
        assert_schema_valid(&envelope);
    }
}
