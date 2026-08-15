//! `outputSchema` definitions for the `roadmap_*` tools, mirroring the
//! `ship` family's approach: each JSON-returning tool advertises a schema so
//! 2026 clients can parse `structuredContent` instead of the text fallback.

use std::sync::Arc;

use rmcp::model::JsonObject;
use schemars::{JsonSchema, schema_for};
use serde::Serialize;
use serde_json::Value;

// WHY THE TWO FOCUS TOOLS ADVERTISE NO `outputSchema`, MEASURED TWICE.
//
// They return `structuredContent` like every sibling; what is missing is only
// the advertisement, and it is missing for a priced reason rather than an
// oversight — the same class of refusal recorded for `blocked_by` below.
//
// Both alternatives were built and measured against the 154,000 B ceiling the
// focus verbs land 370 B under:
//
//   * fully described (`focus` a five-field record, `frontier` counts + ready
//     + blocked + next):                    +2,406 B  ->  156,036  (over)
//   * minimal envelope, both rich fields as
//     bare `Value` (schemars emits `true`):  +1,096 B  ->  154,726  (over)
//
// MCP cannot `$ref` across tools, so even the minimal envelope is paid twice.
// 1,096 B does not fit in 370 B, and the ceiling had just been moved once by an
// explicit human decision for this very chunk — moving it a second time in the
// same chunk to buy back what the first move was granted for is exactly the
// behaviour the rule on `tools_list_payload_stays_within_budget` forbids.
//
// So the shapes are documented in the tool descriptions, which is where a model
// reads them anyway (`outputSchema` is dropped by clients bridging to the
// Messages API before the model sees it — see `model_facing_bytes`). This entry
// becomes payable the moment the duplicated `signal_*` schemas are reclaimed.

#[derive(Serialize, JsonSchema)]
pub struct ReprioritizeProposalOutput {
    pub suggested_priority: u32,
    pub reason: String,
    pub proposed_at: String,
}

/// A full chunk record, as returned by the mutating tools and `roadmap_next`.
#[derive(Serialize, JsonSchema)]
pub struct ChunkOutput {
    pub id: String,
    pub title: String,
    /// One of: backlog, pending, in_progress, blocked, done, obsoleted.
    pub status: String,
    pub priority: u32,
    pub description: String,
    pub acceptance: Vec<String>,
    pub deps: Vec<String>,
    pub cross_refs: Vec<String>,
    pub shared: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reprioritize: Option<ReprioritizeProposalOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub obsoleted_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// WHY `ChunkOutput` DOES NOT DESCRIBE `blocked_by`, THOUGH THE RESPONSE CARRIES IT.
//
// The handlers return `serde_json::to_value(chunk)` — the real `Chunk` — so a
// blocked chunk's response DOES include `blocked_by`. What is missing is only
// its ADVERTISEMENT here, and that omission was measured rather than assumed:
// adding a four-field `BlockedByOutput` to this struct moved `tools_list`
// from 147,267 B to 156,337 B — 9,070 B for four fields, because `ChunkOutput`
// is inlined into roughly ten roadmap tool schemas and MCP cannot `$ref` a
// schema across tools. That is 6,337 B through a 150,000 B ceiling whose
// standing rule is "re-decide the surface, don't raise the line".
//
// So this is a known, priced incompleteness, not an oversight — and it is the
// same one `group`, `name`, `content` and `notes` already have. The honest fix
// is a shared chunk schema rather than ten copies of one, which is a surface
// decision of its own; `chunk-output-schema-is-a-partial-projection` records it.

#[derive(Serialize, JsonSchema)]
pub struct RoadmapCounts {
    pub backlog: usize,
    pub pending: usize,
    pub in_progress: usize,
    pub blocked: usize,
    pub done: usize,
    pub obsoleted: usize,
    pub total: usize,
    /// Blocker-carrying chunks on the ACTIVE board, by kind. Cross-cutting, not
    /// a seventh bucket: a blocked chunk keeps its status and is counted above.
    pub blocked_by: BlockerCounts,
}

// WHY THE TALLY IS BROKEN DOWN AND NOT LUMPED. The kinds mean different things
// to a reader deciding what to do next: `premise_refuted` is a result and wants
// re-scoping, `awaiting_human` is a queue and wants a person, and one total
// cannot tell them apart. Every kind is reported even at zero, because absence
// and zero are different answers — the engine seeds the map from
// `BlockerKind::ALL` rather than from what happens to be on the board.
//
// Doc-comment budget: this rationale is a `//`, not a `///`, because every
// `///` here is serialized into the `roadmap_status` output schema and shipped
// in the tools/list payload against a 150,000 B ceiling.
/// The by-kind blocker tally. Every kind is reported, at 0 when nothing carries it.
#[derive(Serialize, JsonSchema)]
pub struct BlockerCounts {
    pub total: usize,
    pub premise_refuted: usize,
    pub premise_unmet: usize,
    pub awaiting_human: usize,
    pub external: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct ChunkSummary {
    pub id: String,
    pub title: String,
    /// The short node label. Present in the summary alongside `title` because a
    /// caller rendering a list of chunks is exactly the caller that needs a
    /// label rather than 519 sentences.
    pub name: String,
    pub status: String,
    pub priority: u32,
    /// The band `priority` falls in: critical | high | medium | low | later.
    /// Names the number so a reader doesn't have to know the scale.
    pub band: String,
    pub deps: Vec<String>,
    pub has_reprioritize_proposal: bool,
    /// Whether a tracker sweep has suggested a different status for this chunk.
    /// A proposal is NEVER a transition — `status` above is still the truth.
    pub has_status_proposal: bool,
    /// Whether a contested tracker title has been conceded and awaits a human
    /// decision. While open, projections keep sending the tracker's title.
    pub has_title_proposal: bool,
    // Replaces the `has_blocker: bool` this field grew out of. The boolean was
    // exactly `blocked_by.is_some()`, so the two could never legally disagree
    // and one of them was a fifth wheel; the kind subsumes it and answers the
    // question it left open. Rationale lives in `//` for the budget reason
    // noted on `BlockerCounts` above.
    /// Why this chunk cannot be worked: `premise_refuted` | `premise_unmet` |
    /// `awaiting_human` | `external`, absent when nothing blocks it. Such a
    /// chunk is never what `roadmap_next` answers but keeps its priority and
    /// place — skipped is not hidden — and since `title` is truncated and a
    /// blocker is usually stated in a title's tail, this token is what survives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker_kind: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct RoadmapStatusOutput {
    pub project_id: String,
    pub counts: RoadmapCounts,
    /// Id of the next ready chunk (lowest priority, unblocked, deps
    /// satisfied), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    /// Total count of active (backlog/pending/in_progress/blocked) chunks.
    pub active_total: usize,
    /// Active chunks beyond the cap that are not included in `chunks`.
    pub omitted_active: usize,
    /// Active chunks only, priority-sorted, titles truncated, capped in length.
    /// Done/obsoleted chunks are summarized in `counts` + `recent_done`, not here,
    /// so the snapshot stays within the MCP output limit on a large roadmap.
    pub chunks: Vec<ChunkSummary>,
    /// A short tail of the most-recently-finished (done/obsoleted) chunks.
    pub recent_done: Vec<ChunkSummary>,
    /// Human-readable note when anything was elided (empty otherwise).
    pub note: String,
}

#[derive(Serialize, JsonSchema)]
pub struct RoadmapExportOutput {
    pub format: String,
    pub roadmap: String,
}

// WHY `records` IS A BARE `Value` AND NOT A `Vec<ChunkOutput>`.
//
// Two reasons, and the first is correctness rather than thrift. `fields`
// projects each record, so the shape genuinely VARIES per call — a schema
// enumerating twenty-two chunk keys would describe a response this tool only
// returns when no projection was asked for, and a schema that is wrong most of
// the time is worse than one that declines to guess.
//
// The second is the price recorded above: `ChunkOutput` inlines into ~10
// roadmap tool schemas and MCP cannot `$ref` across tools, so reaching for it
// here would have added an eleventh copy. schemars emits `true` for a `Value`,
// which costs almost nothing, and the fields that DO NOT vary are enumerated
// properly below — those are the ones a client can actually validate against.
/// `roadmap_get` — the bounded, projectable fetch.
#[derive(Serialize, JsonSchema)]
pub struct RoadmapGetOutput {
    /// The stored records, in the order `ids` named them. Shape follows
    /// `fields`; `id` is present whatever was projected.
    pub records: Value,
    pub returned: usize,
    /// Ids that matched no chunk. Reported rather than omitted, so a caller can
    /// tell "no such chunk" from "that chunk has no summary".
    pub unknown: Vec<String>,
    /// The projection that was applied; empty means the whole record.
    pub fields: Vec<String>,
}

/// `roadmap_start_chunk`: the chunk plus the backref the agent wires into
/// ship/think.
#[derive(Serialize, JsonSchema)]
pub struct StartChunkOutput {
    pub chunk: ChunkOutput,
    /// `chunk:<id>` — pass to ship_set_objective + a think execution_ref.
    pub backref: String,
    pub hint: String,
}

#[derive(Serialize, JsonSchema)]
pub struct RefreshOutput {
    pub recorded: bool,
    pub summary: String,
    pub think_steps: Vec<u32>,
    pub total_refreshes: usize,
}

#[derive(Serialize, JsonSchema)]
pub struct StructuredError {
    pub error_kind: String,
    pub message: String,
}

/// `roadmap_propose_groups` — which ungrouped chunks belong together, for a
/// person to name.
#[derive(Serialize, JsonSchema)]
pub struct ProposeGroupsOutput {
    /// One entry per cluster: the shared id prefix, the ungrouped chunk ids in
    /// it, and why that prefix cannot be the name of the region holding them.
    pub clusters: Value,
    /// The threshold that was applied: a cluster needed at least this many
    /// ungrouped chunks to be worth naming.
    pub min_shared: usize,
    /// The groups already in play, so a caller can reuse a region rather than
    /// mint one — the map level draws at most 20 marks.
    pub groups: Value,
    /// Said in the response rather than left to `doctor`: this call places
    /// nothing, because no name derived from an id prefix can pass the region
    /// check.
    pub placed: usize,
}

/// `tracker_status` — read-only mirroring state.
#[derive(Serialize, JsonSchema)]
pub struct TrackerStatusOutput {
    pub project_id: String,
    /// Whether mirroring would actually push. False is the default: tracker
    /// projection is opt-in, so `provider`/`target` can be set while this is
    /// still false.
    pub enabled: bool,
    pub provider: Option<String>,
    pub target: Option<String>,
    /// How many chunks are opted in to this provider.
    pub included: usize,
    /// THE DRIFT: how many ACTIVE chunks are not opted in, and are therefore
    /// invisible to the tracker. Reported alongside `included` because
    /// `included` only ever grows and so can never reveal a scope that stopped
    /// growing — which is exactly how this went unnoticed at 46 of 66.
    pub not_included: usize,
    /// Whether newly-created chunks are born in scope. True only for a project
    /// that EXPLICITLY ran `tracker on` / `tracker setup`.
    pub inherits_new_chunks: bool,
}

/// `tracker_setup` — what the setup run actually did.
///
/// The three `target_*` booleans are mutually exclusive and one is always
/// true: the destination was found, created, or could not be verified. They
/// are separate fields rather than an enum because that is the JSON the
/// handler builds, and a schema that improved on it would be a lie.
#[derive(Serialize, JsonSchema)]
pub struct TrackerSetupOutput {
    pub project_id: String,
    pub provider: String,
    pub target: String,
    /// True when nothing was written anywhere.
    pub dry_run: bool,
    pub target_existed: bool,
    /// True only when `create_missing` was set AND the destination was absent.
    pub target_created: bool,
    /// The destination could not be checked; nothing was created on a guess.
    pub target_unverified: bool,
    pub mirroring_enabled: bool,
    /// Chunk ids newly opted in by this run.
    pub included: Vec<String>,
    /// Chunk ids that were already opted in.
    pub already_included: usize,
    /// Configured auto-push cadence in seconds; absent or 0 means no cadence.
    pub auto_push_secs: Option<u64>,
    /// Where the cadence was written, when it was.
    pub auto_push_written_to: Option<String>,
    /// The cadence was already correct, so nothing was rewritten.
    pub auto_push_unchanged: bool,
    /// The cadence could not be written and a human must do it.
    pub auto_push_manual: Option<String>,
    /// Follow-up a human must perform — notably RECONNECTING the MCP server
    /// before an auto-push cadence starts.
    pub next_steps: Vec<String>,
}

pub fn output_schema_for(tool_name: &str) -> Option<Arc<JsonObject>> {
    let value: Value = match tool_name {
        "roadmap_add_chunk"
        | "roadmap_set_status"
        | "roadmap_update_chunk"
        | "roadmap_obsolete_chunk"
        | "roadmap_reprioritize"
        | "roadmap_complete_chunk"
        | "roadmap_link"
        | "roadmap_next" => schema_for!(ChunkOutput).to_value(),
        "roadmap_status" => schema_for!(RoadmapStatusOutput).to_value(),
        "roadmap_export" => schema_for!(RoadmapExportOutput).to_value(),
        "roadmap_get" => schema_for!(RoadmapGetOutput).to_value(),
        "roadmap_start_chunk" => schema_for!(StartChunkOutput).to_value(),
        "roadmap_record_refresh" => schema_for!(RefreshOutput).to_value(),
        // roadmap_set_group returns the patched chunk, exactly as the other
        // chunk-mutating verbs above do.
        "roadmap_set_group" => schema_for!(ChunkOutput).to_value(),
        "roadmap_propose_groups" => schema_for!(ProposeGroupsOutput).to_value(),
        // tracker_* rides this family (see UnifiedService::route_of), so its
        // schemas live here rather than in a fifth module.
        "tracker_status" => schema_for!(TrackerStatusOutput).to_value(),
        "tracker_setup" => schema_for!(TrackerSetupOutput).to_value(),
        _ => return None,
    };
    match value {
        Value::Object(map) => Some(Arc::new(map)),
        _ => None,
    }
}
