//! First-party eval corpus (`eval-corpus-harness`).
//!
//! A versioned, digest-stamped JSONL event stream extracted from the
//! operator's OWN local stores (roadmap chunks, think steps, signals) —
//! structural/sequence data only, no prose. The corpus is the substrate for
//! the offline eval harness in [`eval`] and, later, the learned
//! next-best-action predictor (31j-b).
//!
//! Provenance/consent boundary: the export reads
//! ONLY the local first-party workspace; the anonymized cloud cohort joins a
//! corpus exclusively through the existing consent-gated, k-thresholded
//! telemetry pipeline. The exported file stays local unless the operator
//! shares it deliberately.
//!
//! Determinism contract: building twice from the same stores yields the same
//! bytes and the same digest — there is no wallclock anywhere in the file.

pub mod eval;
pub mod learn;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::roadmap::domain::{Chunk, ChunkStatus};
use crate::signal::domain::Signal;
use crate::think::domain::step::ThinkStep;

/// Bumped on any event-shape change. v2 adds `cross_refs` on chunk events
/// (the `think:N` links already carried by [`Chunk`]) so think-adjacency is
/// computable; v1 files still parse (the field serde-defaults to empty).
pub const CORPUS_VERSION: u32 = 2;

/// Oldest corpus version this build still reads.
pub const MIN_CORPUS_VERSION: u32 = 1;

/// What kind of record an event was extracted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Chunk,
    ThinkStep,
    Signal,
}

/// One structural event. A single flat shape (optional fields per kind)
/// keeps JSONL lines self-describing and the parser trivial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusEvent {
    pub kind: EventKind,
    /// Chunk id / think step number / signal id.
    pub id: String,
    /// RFC 3339 creation time ("" when the source record lacks one).
    #[serde(default)]
    pub created: String,
    /// When the record left the active set: a chunk's `updated_at` once
    /// `done` or `obsoleted`. (Approximation: post-completion touches like
    /// cross-ref links bump `updated_at` by a small amount — documented.)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub closed: Option<String>,
    /// Final lifecycle status as a wire string (chunks + signals).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub priority: Option<u32>,
    /// Chunk deps (chunk ids) or think-step deps (step numbers as strings).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub deps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub confidence: Option<f64>,
    /// v2: a chunk's cross-family links in wire form (e.g. `think:42`).
    /// Accumulated over the chunk's life — consumers that need
    /// point-in-time truth must join against the think events' timestamps.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub cross_refs: Vec<String>,
}

/// A built corpus: versioned events in a stable, deterministic order.
#[derive(Debug, Clone, PartialEq)]
pub struct Corpus {
    pub version: u32,
    pub project: String,
    pub events: Vec<CorpusEvent>,
}

/// The JSONL header line (line 0 of the export).
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    corpus_version: u32,
    project: String,
    events: usize,
    /// `sha256:<hex>` over the event lines exactly as written.
    digest: String,
}

fn status_wire<T: Serialize>(status: &T) -> Option<String> {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
}

fn chunk_event(c: &Chunk) -> CorpusEvent {
    let closed = matches!(c.status, ChunkStatus::Done | ChunkStatus::Obsoleted)
        .then(|| c.updated_at.clone());
    CorpusEvent {
        kind: EventKind::Chunk,
        id: c.id.clone(),
        created: c.created_at.clone(),
        closed,
        status: status_wire(&c.status),
        priority: Some(c.priority),
        deps: c.deps.clone(),
        confidence: None,
        cross_refs: c.cross_refs.clone(),
    }
}

fn step_event(s: &ThinkStep) -> CorpusEvent {
    CorpusEvent {
        kind: EventKind::ThinkStep,
        id: s.step_number.to_string(),
        created: s.timestamp.clone().unwrap_or_default(),
        closed: None,
        status: None,
        priority: None,
        deps: s
            .dependencies
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|d| d.step().to_string())
            .collect(),
        confidence: s.confidence,
        cross_refs: Vec::new(),
    }
}

fn signal_event(s: &Signal) -> CorpusEvent {
    CorpusEvent {
        kind: EventKind::Signal,
        id: s.id.clone(),
        created: s.created.clone(),
        closed: None,
        status: status_wire(&s.status),
        priority: None,
        deps: Vec::new(),
        confidence: None,
        cross_refs: Vec::new(),
    }
}

/// Build a corpus from in-memory store contents. Pure; deterministic: events
/// are sorted by (kind, created, id), so the same stores always produce the
/// same corpus regardless of iteration order upstream.
pub fn build_corpus<'a>(
    project: &str,
    chunks: &[Chunk],
    steps: impl Iterator<Item = &'a ThinkStep>,
    signals: &[Signal],
) -> Corpus {
    let mut events: Vec<CorpusEvent> = chunks
        .iter()
        .map(chunk_event)
        .chain(steps.map(step_event))
        .chain(signals.iter().map(signal_event))
        .collect();
    events
        .sort_by(|a, b| (a.kind as u8, &a.created, &a.id).cmp(&(b.kind as u8, &b.created, &b.id)));
    Corpus {
        version: CORPUS_VERSION,
        project: project.to_owned(),
        events,
    }
}

fn event_lines(events: &[CorpusEvent]) -> String {
    events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_default() + "\n")
        .collect()
}

fn digest_of(lines: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(lines.as_bytes()))
}

/// Serialize as JSONL: a header line carrying the version + digest, then one
/// event per line. The digest covers the event lines byte-exactly.
pub fn to_jsonl(corpus: &Corpus) -> String {
    let lines = event_lines(&corpus.events);
    let header = Header {
        corpus_version: corpus.version,
        project: corpus.project.clone(),
        events: corpus.events.len(),
        digest: digest_of(&lines),
    };
    format!(
        "{}\n{}",
        serde_json::to_string(&header).unwrap_or_default(),
        lines
    )
}

/// Parse a JSONL corpus, verifying the header digest against the event lines
/// actually present (a corrupted or hand-edited file fails loudly).
pub fn parse_jsonl(input: &str) -> Result<Corpus, String> {
    let mut lines = input.lines();
    let header: Header = serde_json::from_str(lines.next().ok_or("empty corpus file")?)
        .map_err(|e| format!("malformed corpus header: {e}"))?;
    if header.corpus_version < MIN_CORPUS_VERSION || header.corpus_version > CORPUS_VERSION {
        return Err(format!(
            "unsupported corpus_version {} (this build reads v{MIN_CORPUS_VERSION}..=v{CORPUS_VERSION})",
            header.corpus_version
        ));
    }
    let mut events = Vec::new();
    let mut raw = String::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        events.push(
            serde_json::from_str::<CorpusEvent>(line)
                .map_err(|e| format!("malformed corpus event: {e}"))?,
        );
        raw.push_str(line);
        raw.push('\n');
    }
    let digest = digest_of(&raw);
    if digest != header.digest {
        return Err(format!(
            "corpus digest mismatch: header says {}, events hash to {digest}",
            header.digest
        ));
    }
    if events.len() != header.events {
        return Err(format!(
            "corpus event count mismatch: header says {}, found {}",
            header.events,
            events.len()
        ));
    }
    Ok(Corpus {
        version: header.corpus_version,
        project: header.project,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &str, status: ChunkStatus, priority: u32, created: &str, updated: &str) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: format!("chunk {id}"),
            name: crate::roadmap::name::derive(id),
            status,
            priority,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec![],
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: None,
            created_at: created.into(),
            updated_at: updated.into(),
        }
    }

    #[test]
    fn build_is_deterministic_and_digest_stable() {
        let chunks = vec![
            chunk(
                "b",
                ChunkStatus::Done,
                20,
                "2026-01-02T00:00:00Z",
                "2026-01-05T00:00:00Z",
            ),
            chunk(
                "a",
                ChunkStatus::Pending,
                10,
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            ),
        ];
        let c1 = build_corpus("proj", &chunks, std::iter::empty(), &[]);
        let mut reversed = chunks.clone();
        reversed.reverse();
        let c2 = build_corpus("proj", &reversed, std::iter::empty(), &[]);
        assert_eq!(c1, c2, "input order must not matter");
        assert_eq!(
            to_jsonl(&c1),
            to_jsonl(&c2),
            "bytes + digest must be stable"
        );
    }

    #[test]
    fn closed_only_for_done_and_obsoleted() {
        let done = chunk_event(&chunk("d", ChunkStatus::Done, 1, "c", "u"));
        let obs = chunk_event(&chunk("o", ChunkStatus::Obsoleted, 1, "c", "u"));
        let open = chunk_event(&chunk("p", ChunkStatus::Pending, 1, "c", "u"));
        assert_eq!(done.closed.as_deref(), Some("u"));
        assert_eq!(obs.closed.as_deref(), Some("u"));
        assert_eq!(open.closed, None);
        assert_eq!(done.status.as_deref(), Some("done"));
    }

    #[test]
    fn jsonl_roundtrips_and_detects_tampering() {
        let chunks = vec![chunk(
            "a",
            ChunkStatus::Done,
            5,
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
        )];
        let corpus = build_corpus("proj", &chunks, std::iter::empty(), &[]);
        let jsonl = to_jsonl(&corpus);
        let parsed = parse_jsonl(&jsonl).expect("roundtrip");
        assert_eq!(parsed, corpus);

        let tampered = jsonl.replace("\"priority\":5", "\"priority\":6");
        let err = parse_jsonl(&tampered).expect_err("tampering must fail");
        assert!(err.contains("digest mismatch"), "got: {err}");
    }
}
