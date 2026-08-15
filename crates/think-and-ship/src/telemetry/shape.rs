//! The structural-shape extractor (31h-a): envelopes → [`StructuralShape`].
//!
//! Privacy invariant, enforced at the EMIT side: every string in the output
//! is either (a) a closed-vocabulary token (family/kind/status/relation/
//! bucket names — fixed sets defined in this file), or (b) a salted-hash
//! pseudonym ([`HASH_LEN`] lowercase hex). The free-text payload
//! (`record`'s prose fields) is never read except through allowlisted,
//! vocabulary-mapped accessors. [`extract`] additionally re-scans its own
//! serialized output with the [`scrub`] detectors
//! and refuses to return a shape that trips them.

use std::collections::BTreeMap;

use chrono::DateTime;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::cloud::envelope::UnifiedRecordEnvelope;
use crate::telemetry::scrub;

/// Width of a hash pseudonym in hex chars (64 bits — collision-safe at any
/// plausible corpus size, short enough to keep shapes compact).
pub const HASH_LEN: usize = 16;

/// Shape schema identifier (versioned so the 31h-c ingest can evolve).
pub const SHAPE_SCHEMA: &str = "telemetry-shape/1";

/// Statuses that may appear verbatim — the union of the four families'
/// closed lifecycle vocabularies. Anything else maps to `"other"` so a
/// free-text status field can never smuggle content out.
const STATUS_VOCAB: &[&str] = &[
    // roadmap chunks
    "backlog",
    "pending",
    "in_progress",
    "blocked",
    "done",
    "obsoleted",
    // ship objective/tasks
    "defined",
    "planned",
    "active",
    "completed",
    "skipped",
    "shipped",
    "abandoned",
    // signals
    "new",
    "researched",
    "surfaced",
    "promoted",
    "ignored",
    "snoozed",
];

/// Edge relations that may appear verbatim (the contract's labeled edges).
const RELATION_VOCAB: &[&str] = &[
    "realizes",
    "part_of",
    "verifies",
    "supports",
    "refutes",
    "depends_on",
];

const OTHER: &str = "other";
const UNLABELED: &str = "unlabeled";

/// Lifetime buckets for created→updated spans (closed vocabulary).
const BUCKETS: &[(&str, i64)] = &[
    ("lt1m", 60),
    ("lt10m", 600),
    ("lt1h", 3_600),
    ("lt1d", 86_400),
];
const BUCKET_MAX: &str = "ge1d";

/// Salted-hash pseudonym: `SHA-256(salt ‖ 0x1f ‖ label)` truncated to
/// [`HASH_LEN`] lowercase hex chars. Pseudonymization, not authentication —
/// the per-install salt makes labels unlinkable across installs.
#[must_use]
pub fn pseudonym(salt: &[u8], label: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update([0x1f]);
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(HASH_LEN);
    for byte in digest.iter().take(HASH_LEN / 2) {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn vocab_or_other(vocab: &[&'static str], value: &str) -> &'static str {
    vocab
        .iter()
        .find(|v| **v == value)
        .copied()
        .unwrap_or(OTHER)
}

fn duration_bucket(created: &str, updated: &str) -> Option<&'static str> {
    let start = DateTime::parse_from_rfc3339(created).ok()?;
    let end = DateTime::parse_from_rfc3339(updated).ok()?;
    let secs = (end - start).num_seconds();
    if secs < 0 {
        return None;
    }
    for (name, limit) in BUCKETS {
        if secs < *limit {
            return Some(name);
        }
    }
    Some(BUCKET_MAX)
}

/// Graph topology — counts and a degree histogram, never node identities
/// beyond pseudonyms (and the histogram doesn't even carry those).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct GraphShape {
    /// Total records (graph nodes).
    pub nodes: usize,
    /// Total edges across all records.
    pub edges: usize,
    /// Edge counts by relation label (closed vocabulary or "unlabeled"/"other").
    pub relations: BTreeMap<&'static str, usize>,
    /// Out-degree histogram: degree → how many records have it.
    pub out_degree: BTreeMap<usize, usize>,
}

/// The anonymized structural shape of a workspace's records. Every string is
/// closed-vocabulary or a salted-hash pseudonym — see the module invariant.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct StructuralShape {
    /// Shape schema version ([`SHAPE_SCHEMA`]).
    pub schema: String,
    /// Record counts by `family.kind` (both from closed enums).
    pub records: BTreeMap<String, usize>,
    /// Record counts by `family.kind.status` (status vocabulary-mapped).
    pub statuses: BTreeMap<String, usize>,
    pub graph: GraphShape,
    /// Tool usage counts by tool pseudonym.
    pub tools: BTreeMap<String, usize>,
    /// Tool-sequence bigrams: pseudonym → next-pseudonym → count (order
    /// within each record's `tools_used` sequence).
    pub tool_bigrams: BTreeMap<String, BTreeMap<String, usize>>,
    /// created→updated lifetime buckets (records with both timestamps).
    pub lifetimes: BTreeMap<&'static str, usize>,
    /// How many records carry lens membership (count only — no slugs).
    pub lensed: usize,
}

/// Extraction failure.
#[derive(Debug, thiserror::Error)]
pub enum ShapeError {
    /// The serialized shape tripped the output detectors — a privacy
    /// regression; the shape is withheld rather than returned.
    #[error("extracted shape failed the scrub detector: {0:?}")]
    DirtyOutput(Vec<scrub::ScrubFinding>),
    /// The shape could not be serialized for verification.
    #[error("shape serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Reduce envelopes to their structural shape. Pure: no IO, no clock, no
/// global state — the per-install `salt` is the only ambient input. The
/// returned shape has been serialized and re-scanned by the
/// [`scrub`] detectors; a dirty result is an error,
/// never a return value.
pub fn extract(
    envelopes: &[UnifiedRecordEnvelope],
    salt: &[u8],
) -> Result<StructuralShape, ShapeError> {
    let mut shape = StructuralShape {
        schema: SHAPE_SCHEMA.to_string(),
        ..StructuralShape::default()
    };
    shape.graph.nodes = envelopes.len();

    for env in envelopes {
        let family_kind = format!("{}.{}", env.family.as_str(), env.kind.as_str());
        *shape.records.entry(family_kind.clone()).or_default() += 1;

        // Status: read ONLY through the vocabulary map.
        if let Some(status) = env.record.get("status").and_then(|s| s.as_str()) {
            let mapped = vocab_or_other(STATUS_VOCAB, status);
            *shape
                .statuses
                .entry(format!("{family_kind}.{mapped}"))
                .or_default() += 1;
        }

        // Graph topology.
        shape.graph.edges += env.edges.len();
        *shape.graph.out_degree.entry(env.edges.len()).or_default() += 1;
        for edge in &env.edges {
            let relation = edge
                .relation
                .as_deref()
                .map_or(UNLABELED, |r| vocab_or_other(RELATION_VOCAB, r));
            *shape.graph.relations.entry(relation).or_default() += 1;
        }

        // Tool sequences: names are user-influenced strings → pseudonyms.
        let tools: Vec<String> = env
            .record
            .get("tools_used")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|name| pseudonym(salt, name))
                    .collect()
            })
            .unwrap_or_default();
        for tool in &tools {
            *shape.tools.entry(tool.clone()).or_default() += 1;
        }
        for pair in tools.windows(2) {
            *shape
                .tool_bigrams
                .entry(pair[0].clone())
                .or_default()
                .entry(pair[1].clone())
                .or_default() += 1;
        }

        // Lifetime bucket: envelope `updated` when set, else the record's own
        // lifecycle timestamps (chunks carry `updated_at`, ship tasks
        // `completed_at` — builders don't copy them up to the envelope).
        // Timestamps are parsed, never emitted — only the bucket name is.
        let end = env.updated.as_deref().or_else(|| {
            ["updated_at", "completed_at"]
                .iter()
                .find_map(|f| env.record.get(f).and_then(|v| v.as_str()))
        });
        if let Some(bucket) = end.and_then(|updated| duration_bucket(&env.created, updated)) {
            *shape.lifetimes.entry(bucket).or_default() += 1;
        }

        if env.lenses.as_ref().is_some_and(|l| !l.is_empty()) {
            shape.lensed += 1;
        }
    }

    // Defense-in-depth: the shape must pass its own output detectors.
    let serialized = serde_json::to_string(&shape)?;
    let findings = scrub::scan(&serialized);
    if findings.is_empty() {
        Ok(shape)
    } else {
        Err(ShapeError::DirtyOutput(findings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::envelope::{Edge, Family, Kind};
    use serde_json::json;

    const SALT: &[u8] = b"test-install-salt";

    fn envelope(
        kind: Kind,
        id: &str,
        record: serde_json::Value,
        edges: Vec<Edge>,
    ) -> UnifiedRecordEnvelope {
        UnifiedRecordEnvelope::owner(
            "tenant-x",
            Family::Think,
            kind,
            id,
            "2026-06-10T00:00:00Z",
            record,
            edges,
        )
    }

    #[test]
    fn pseudonyms_are_stable_salted_and_shaped() {
        let a = pseudonym(SALT, "cargo test");
        assert_eq!(a.len(), HASH_LEN);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(a, pseudonym(SALT, "cargo test"));
        assert_ne!(a, pseudonym(b"other-salt", "cargo test"));
        assert_ne!(a, pseudonym(SALT, "cargo build"));
    }

    #[test]
    fn statuses_outside_the_vocabulary_collapse_to_other() {
        let envs = vec![
            envelope(Kind::Chunk, "c1", json!({"status": "done"}), vec![]),
            envelope(
                Kind::Chunk,
                "c2",
                json!({"status": "AKIAIOSFODNN7EXAMPLE"}),
                vec![],
            ),
        ];
        let shape = extract(&envs, SALT).expect("clean shape");
        assert_eq!(shape.statuses.get("think.chunk.done"), Some(&1));
        assert_eq!(shape.statuses.get("think.chunk.other"), Some(&1));
    }

    #[test]
    fn structure_is_preserved_topology_tools_lifetimes() {
        let mut env = envelope(
            Kind::Step,
            "7",
            json!({
                "status": "done",
                "tools_used": ["Edit", "Bash", "Edit"],
            }),
            vec![
                Edge {
                    target: "think:6".into(),
                    relation: Some("supports".into()),
                },
                Edge {
                    target: "task:explore".into(),
                    relation: None,
                },
            ],
        );
        env.updated = Some("2026-06-10T00:05:00Z".into());
        let shape = extract(&[env], SALT).expect("clean shape");

        assert_eq!(shape.graph.nodes, 1);
        assert_eq!(shape.graph.edges, 2);
        assert_eq!(shape.graph.relations.get("supports"), Some(&1));
        assert_eq!(shape.graph.relations.get("unlabeled"), Some(&1));
        assert_eq!(shape.graph.out_degree.get(&2), Some(&1));

        // 3 tool uses, 2 distinct, 2 bigrams (Edit→Bash, Bash→Edit).
        let edit = pseudonym(SALT, "Edit");
        let bash = pseudonym(SALT, "Bash");
        assert_eq!(shape.tools.get(&edit), Some(&2));
        assert_eq!(shape.tools.get(&bash), Some(&1));
        assert_eq!(
            shape.tool_bigrams.get(&edit).and_then(|m| m.get(&bash)),
            Some(&1)
        );

        // 5 minutes → lt10m bucket.
        assert_eq!(shape.lifetimes.get("lt10m"), Some(&1));
    }
}
