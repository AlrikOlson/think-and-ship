//! The unified record envelope contract, built from the Rust side.
//!
//! `UnifiedRecordEnvelope` serializes to exactly the shape of
//! `contract/unified-record-envelope.schema.json` — the same canonical schema
//! the TypeScript backend validates against — so the Rust producer and the TS
//! consumer can never drift (proven by the schema-validation test below).
//!
//! Per-family `idempotency_key` is byte-identical to `backend/src/idempotency.ts`
//! (separator U+241F), so re-syncing a record collapses to one upsert on the
//! cloud regardless of which side produced it.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// This envelope schema's version (matches the contract's `schema_version`).
pub const SCHEMA_VERSION: &str = "1";

/// U+241F SYMBOL FOR UNIT SEPARATOR — the idempotency field separator.
const SEP: char = '\u{241f}';

/// Which tool family produced the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Think,
    Ship,
    Roadmap,
    Signal,
}

impl Family {
    /// The lowercase wire form, also used in the idempotency key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Ship => "ship",
            Self::Roadmap => "roadmap",
            Self::Signal => "signal",
        }
    }
}

/// The record type within its family (constrained to the family by the schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Step,
    Objective,
    Task,
    Action,
    Check,
    Gate,
    Chunk,
    Signal,
}

impl Kind {
    /// The lowercase wire form, also used in the idempotency key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Objective => "objective",
            Self::Task => "task",
            Self::Action => "action",
            Self::Check => "check",
            Self::Gate => "gate",
            Self::Chunk => "chunk",
            Self::Signal => "signal",
        }
    }
}

/// One directed edge in the cross-ref graph (the product's moat). `target` is
/// the `prefix:value` wire form (serialized as `ref`, matching the contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    #[serde(rename = "ref")]
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
}

impl Edge {
    /// An unlabeled edge to `target` (a `prefix:value` cross-ref).
    #[must_use]
    pub fn to(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            relation: None,
        }
    }
}

/// Build edges from a record's `cross_refs[]` (already `prefix:value` strings,
/// as roadmap chunks and signals store them).
#[must_use]
pub fn edges_from_cross_refs(cross_refs: &[String]) -> Vec<Edge> {
    cross_refs.iter().map(|r| Edge::to(r.clone())).collect()
}

pub(crate) fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The idempotency key for an owner-authored record (think/ship/roadmap):
/// `sha256(tenant ␟ family ␟ kind ␟ id)`. Re-syncing the same record yields the
/// same key — its own identity is stable.
#[must_use]
pub fn owner_idempotency_key(tenant: &str, family: Family, kind: Kind, id: &str) -> String {
    sha256_hex(&format!(
        "{tenant}{SEP}{family}{SEP}{kind}{SEP}{id}",
        family = family.as_str(),
        kind = kind.as_str(),
    ))
}

/// The idempotency key for an ingested signal — the 30a rule:
/// `sha256(tenant ␟ adapter ␟ external_id)` when there's a stable upstream id,
/// else `sha256(tenant ␟ kind ␟ from ␟ body ␟ created)`.
#[must_use]
pub fn signal_idempotency_key(
    tenant: &str,
    adapter_external: Option<(&str, &str)>,
    content: (&str, &str, &str, &str),
) -> String {
    match adapter_external {
        Some((adapter, external_id)) => {
            sha256_hex(&format!("{tenant}{SEP}{adapter}{SEP}{external_id}"))
        }
        None => {
            let (kind, from, body, created) = content;
            sha256_hex(&format!(
                "{tenant}{SEP}{kind}{SEP}{from}{SEP}{body}{SEP}{created}"
            ))
        }
    }
}

/// A record on the wire — the local family object (`record`) plus the cloud
/// envelope fields. Serializes to the unified-record contract exactly.
#[derive(Debug, Clone, Serialize)]
pub struct UnifiedRecordEnvelope {
    pub schema_version: &'static str,
    pub tenant_id: String,
    pub family: Family,
    pub kind: Kind,
    pub id: String,
    pub idempotency_key: String,
    pub created: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// Soft M:N lens membership within the workspace (31a-ii). `None` =
    /// unlensed and serializes to the exact pre-31a-ii wire bytes, so legacy
    /// pushes stay idempotency-identical. Never read by the key functions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lenses: Option<Vec<String>>,
    pub edges: Vec<Edge>,
    pub record: serde_json::Value,
}

impl UnifiedRecordEnvelope {
    /// Build an owner-authored envelope (think/ship/roadmap), computing the
    /// identity-based idempotency key. `record` is the verbatim local object.
    #[must_use]
    pub fn owner(
        tenant: impl Into<String>,
        family: Family,
        kind: Kind,
        id: impl Into<String>,
        created: impl Into<String>,
        record: serde_json::Value,
        edges: Vec<Edge>,
    ) -> Self {
        let tenant = tenant.into();
        let id = id.into();
        // Stamp the source project into the record payload (sync-project-scope).
        // The backend ADOPTS the token's tenant on store, so `tenant_id` stops
        // carrying project identity the moment an org-scoped token is used —
        // the record itself must say which project authored it, or every
        // project in the org bleeds into every other on reconcile. `record`
        // is open per the 31a contract; families whose domain object already
        // carries project_id (ship objectives) keep their own value.
        let mut record = record;
        if let Some(obj) = record.as_object_mut() {
            obj.entry("project_id".to_string())
                .or_insert_with(|| serde_json::Value::String(tenant.clone()));
        }
        let idempotency_key = owner_idempotency_key(&tenant, family, kind, &id);
        Self {
            schema_version: SCHEMA_VERSION,
            tenant_id: tenant,
            family,
            kind,
            id,
            idempotency_key,
            created: created.into(),
            updated: None,
            lenses: None,
            edges,
            record,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canonical_schema() -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/unified-record-envelope.schema.json"
        );
        serde_json::from_str(&std::fs::read_to_string(path).expect("read schema"))
            .expect("parse schema")
    }

    /// Validate a built envelope against the ONE canonical schema — the same
    /// file the TS ajv test uses, so Rust ↔ TS ↔ contract are all in agreement.
    fn assert_valid(env: &UnifiedRecordEnvelope) {
        let validator = jsonschema::validator_for(&canonical_schema()).expect("compile schema");
        let value = serde_json::to_value(env).expect("serialize envelope");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }

    /// 31a-ii back-compat: a lens-less envelope serializes to the exact
    /// pre-31a-ii wire shape (no `lenses` key at all), and a lensed one
    /// validates against the same canonical schema.
    #[test]
    fn lenses_are_optional_and_absent_means_legacy_wire_bytes() {
        let legacy = UnifiedRecordEnvelope::owner(
            "think-and-ship-676f38",
            Family::Roadmap,
            Kind::Chunk,
            "c1",
            "2026-06-08T00:00:00Z",
            json!({ "id": "c1", "status": "backlog" }),
            vec![],
        );
        assert_valid(&legacy);
        let value = serde_json::to_value(&legacy).unwrap();
        assert!(
            value.get("lenses").is_none(),
            "None must serialize to NO lenses key — legacy pushes stay byte-identical"
        );

        let mut lensed = legacy.clone();
        lensed.lenses = Some(vec!["think-and-ship".into(), "q3-perf".into()]);
        assert_valid(&lensed);
        // Idempotency is lens-blind: same identity → same key, lensed or not.
        assert_eq!(lensed.idempotency_key, legacy.idempotency_key);
    }

    #[test]
    fn roadmap_chunk_envelope_validates() {
        let env = UnifiedRecordEnvelope::owner(
            "think-and-ship-676f38",
            Family::Roadmap,
            Kind::Chunk,
            "saas-cloud-envelope",
            "2026-06-08T00:00:00Z",
            json!({ "id": "saas-cloud-envelope", "status": "in_progress" }),
            edges_from_cross_refs(&["think:9".into(), "signal:abc".into()]),
        );
        assert_valid(&env);
    }

    #[test]
    fn think_step_envelope_validates() {
        let env = UnifiedRecordEnvelope::owner(
            "think-and-ship-676f38",
            Family::Think,
            Kind::Step,
            "899",
            "2026-06-08T00:00:00Z",
            json!({ "step_number": 899, "purpose": "x" }),
            vec![Edge {
                target: "think:8".into(),
                relation: Some("depends_on".into()),
            }],
        );
        assert_valid(&env);
    }

    #[test]
    fn ship_action_envelope_validates() {
        let env = UnifiedRecordEnvelope::owner(
            "think-and-ship-676f38",
            Family::Ship,
            Kind::Action,
            "action:42",
            "2026-06-08T00:00:00Z",
            json!({ "id": 42, "type": "code" }),
            vec![Edge::to("think:9")],
        );
        assert_valid(&env);
    }

    /// The approval-gate kind (webapp-approval-gates): a ship/gate envelope —
    /// built exactly as `cloud::build::from_gate` builds it — must satisfy the
    /// SAME canonical schema the backend validates against, or a gate could
    /// never reach the workspace to be answered.
    #[test]
    fn ship_gate_envelope_validates() {
        let gate = crate::ship::gate::open(
            "b7e2c4a0-1f3d-4e5b-8a9c-2d6f0e4b8c1a".into(),
            "Deploy the new billing Worker to production now?",
            "All tests pass; rollback is one redeploy.",
            vec![
                crate::ship::gate::GateOption {
                    key: "deploy".into(),
                    label: "Deploy now".into(),
                },
                crate::ship::gate::GateOption {
                    key: "hold".into(),
                    label: "Hold for review".into(),
                },
            ],
            "hold",
            3600,
            chrono::Utc::now(),
        )
        .expect("a valid gate");
        let env =
            crate::cloud::build::from_gate("think-and-ship-676f38", &gate, Some("deploy-31g"));
        assert!(
            env.edges.iter().any(|e| e.target == "task:deploy-31g"),
            "the gate must carry its part_of edge to the paused task"
        );
        assert_valid(&env);
    }

    #[test]
    fn signal_envelope_validates() {
        let env = UnifiedRecordEnvelope::owner(
            "think-and-ship-676f38",
            Family::Signal,
            Kind::Signal,
            "sig-1",
            "2026-06-08T00:00:00Z",
            json!({ "id": "sig-1", "kind": "bug", "from": "dana", "body": "x", "status": "new" }),
            edges_from_cross_refs(&["chunk:saas-cloud-envelope".into()]),
        );
        assert_valid(&env);
    }

    #[test]
    fn owner_idempotency_is_deterministic_lowercase_hex() {
        let a = owner_idempotency_key("t", Family::Roadmap, Kind::Chunk, "c1");
        let b = owner_idempotency_key("t", Family::Roadmap, Kind::Chunk, "c1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // A different id yields a different key.
        assert_ne!(
            a,
            owner_idempotency_key("t", Family::Roadmap, Kind::Chunk, "c2")
        );
    }

    #[test]
    fn owner_idempotency_matches_the_documented_separator_rule() {
        // sha256("t␟roadmap␟chunk␟c1") — must equal the TS computation.
        let expected: String = {
            let input = format!("t{SEP}roadmap{SEP}chunk{SEP}c1");
            super::sha256_hex(&input)
        };
        assert_eq!(
            owner_idempotency_key("t", Family::Roadmap, Kind::Chunk, "c1"),
            expected
        );
    }

    #[test]
    fn signal_idempotency_uses_adapter_external_when_present() {
        let with_ext =
            signal_idempotency_key("t", Some(("github_issue", "I_1")), ("bug", "d", "b", "c"));
        let content = signal_idempotency_key("t", None, ("bug", "d", "b", "c"));
        assert_ne!(with_ext, content);
        assert_eq!(with_ext.len(), 64);
    }

    /// A real regression, made permanent. `record_tracker_link` pushes an
    /// `ext:<provider>/<external_id>` cross-ref onto the chunk, and
    /// `edges_from_cross_refs` maps EVERY cross-ref into `edges[]` unfiltered.
    /// While the contract's `CrossRef` pattern omitted `ext`, that envelope was
    /// a 422 — and a 422 is terminal, so `CloudClient::push` DROPPED it instead
    /// of queueing it. Binding a chunk to a tracker silently killed that
    /// chunk's cloud sync forever, at WARN level only. It went unseen because
    /// `tests/tracker_seam.rs` never wires a cloud client; this test is what
    /// makes it impossible to reintroduce.
    #[test]
    fn an_ext_tracker_cross_ref_validates_against_the_contract() {
        let cross_refs = vec![
            "think:44".to_string(),
            "ext:github/owner/repo#12".to_string(),
        ];
        let env = UnifiedRecordEnvelope::owner(
            "t",
            Family::Roadmap,
            Kind::Chunk,
            "c1",
            "2026-06-08T00:00:00Z",
            json!({ "id": "c1", "status": "backlog" }),
            edges_from_cross_refs(&cross_refs),
        );
        // The ext: ref survives as a real edge — it is provenance, not noise.
        assert!(
            env.edges
                .iter()
                .any(|e| e.target == "ext:github/owner/repo#12"),
            "the tracker edge must reach the wire, not be filtered out"
        );
        assert_valid(&env);
    }
}
