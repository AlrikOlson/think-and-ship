use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::service::RoadmapService;
use crate::infra::CrossRef;
use crate::roadmap::domain::ChunkStatus;

impl RoadmapService {
    pub(crate) fn make_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::tool_router()
    }
}

/// Parse a wire status string into the domain enum (snake_case), surfacing a
/// readable error for the structured-error envelope.
fn parse_status(s: &str) -> Result<ChunkStatus, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        format!(
            "invalid status '{s}' (expected backlog|pending|in_progress|blocked|done|obsoleted)"
        )
    })
}

// Priority parsing lives in `infra::coerce` (the single, infallible home):
// `coerce::priority` accepts int | numeric string | named level and falls
// back to 0 rather than erroring, so it can never emit a -32602.

// ── Arg types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddChunkArgs {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    // The full rule lives in the tool description and in the engine's refusal,
    // which are the two copies a caller actually reads — one before forming the
    // call, one when they get it wrong. A third copy here only costs bytes in
    // every tools/list handshake.
    /// Short node label, max 24 chars. Derived from the id if omitted.
    #[serde(default)]
    pub name: String,
    /// Initial status (default `pending`).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "crate::infra::coerce::priority")]
    pub priority: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub acceptance: Vec<String>,
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub deps: Vec<String>,
    #[serde(default)]
    pub shared: bool,
    /// The workstream. Omit for ungrouped.
    #[serde(default)]
    pub group: Option<String>,
    /// PREFER THIS over packing detail into `description` — the structured
    /// body that renders as UI: {version:1, summary: one plain sentence (no
    /// shorthand, no ids), facts?: [{label,value}], sections?: [{heading,
    /// prose?: markdown, list?: [{text, done?}]}]}. `description` stays the
    /// plain-prose fallback.
    #[serde(default)]
    #[schemars(schema_with = "crate::content::input_schema")]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TrackerSetupArgs {
    /// Which tracker: `linear` or `github`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// A Linear team KEY (letters and digits only, e.g. `ENG`) or a GitHub
    /// `owner/repo`.
    #[serde(default)]
    pub into: String,
    /// Display name to use if the destination has to be created.
    #[serde(default)]
    pub name: Option<String>,
    /// Only include one priority band: critical|high|medium|low|later.
    #[serde(default)]
    pub band: Option<String>,
    /// Name for the initiative (the roof mirrored projects file under), for
    /// providers that have one. Absent keeps the configured name or the
    /// directory-basename default; blank is a no-op.
    #[serde(default)]
    pub initiative: Option<String>,
    /// Create the destination upstream when it is absent. **Defaults to false.**
    /// This writes to a third-party system and cannot be undone from here — only
    /// set it when the human has asked for the destination to be created.
    #[serde(default)]
    pub create_missing: bool,
    /// Seconds between unattended pushes; 0 leaves auto-push alone.
    #[serde(default = "default_push_secs")]
    pub push_secs: u64,
    /// Report what would happen and write nothing, locally or upstream.
    #[serde(default)]
    pub dry_run: bool,
}

fn default_provider() -> String {
    "github".to_string()
}

const fn default_push_secs() -> u64 {
    300
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetStatusArgs {
    #[serde(default)]
    pub id: String,
    /// One of: backlog, pending, in_progress, blocked, done, obsoleted.
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateChunkArgs {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Rewrite the short node label (at most 24 characters), leaving the
    /// sentence `title` alone. Send an empty string to re-seed it from the id.
    #[serde(default)]
    pub name: Option<String>,
    /// Accepts a number or a named level, like `roadmap_add_chunk` — it took
    /// only numbers, so the same word worked on one tool and not the other.
    #[serde(default, deserialize_with = "crate::infra::coerce::optional_priority")]
    pub priority: Option<u32>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub acceptance: Option<Vec<String>>,
    #[serde(default)]
    pub deps: Option<Vec<String>>,
    /// Replace the chunk's structured body (same shape as
    /// `roadmap_add_chunk.content`). Absent leaves stored content unchanged.
    #[serde(default)]
    #[schemars(schema_with = "crate::content::input_schema")]
    pub content: Option<serde_json::Value>,
    /// Resolve an open title proposal: "accept" adopts the tracker's title
    /// into the plan, "reject" clears the proposal so the plan's title flows
    /// again. Errors when the chunk has no open proposal.
    #[serde(default)]
    pub resolve_title_proposal: Option<String>,
    /// Why this chunk cannot be worked, when the answer is not another chunk
    /// (that is `deps`).
    #[serde(default)]
    pub blocked_by: Option<BlockedByArgs>,
    /// Retract this chunk's blocker. Errors when there is none to retract.
    #[serde(default, deserialize_with = "crate::infra::coerce::lenient_bool")]
    pub clear_blocked_by: bool,
}

// The blocker payload on `UpdateChunkArgs`.
//
// DELIBERATELY NOT A DOC COMMENT. schemars serializes a `///` on a struct into
// the JSON schema's `description`, and this input schema is charged to
// `model_facing_tool_surface_stays_lean` — the bytes every agent pays on every
// session. Written as `///` first, this rationale alone cost ~520 B of context
// to explain an internal serde decision no caller acts on. Rationale for the
// maintainer goes in `//`; only what steers a CALLER earns a `///` here.
//
// Both strings are `#[serde(default)]` and neither is `required`, for the
// reason `add_chunk_priority_named_or_numeric` records: a missing field must
// reach the handler as an empty string and come back as a readable soft error,
// because a hard `-32602` from deserialization cancels every sibling tool call
// in a parallel batch. The emptiness checks live in
// `RoadmapEngine::validate_blocked_by`, which already refuses a blank reason.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BlockedByArgs {
    /// premise_refuted | premise_unmet | awaiting_human | external.
    #[serde(default)]
    pub kind: String,
    /// Why, in a sentence nobody should have to re-derive from the title.
    #[serde(default)]
    pub reason: String,
    /// Optional proof — a cross-ref (think:N | chunk:id | task:X).
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObsoleteArgs {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReprioritizeArgs {
    #[serde(default)]
    pub id: String,
    #[serde(default, deserialize_with = "crate::infra::coerce::priority")]
    pub suggested_priority: u32,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportArgs {
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "markdown".to_string()
}

// The bounded-fetch arguments.
//
// DELIBERATELY LIGHT ON `///`. schemars serializes every doc comment here into
// the input schema, which is charged to `model_facing_tool_surface_stays_lean`
// — bytes every agent pays on every session. The cap, the vocabulary and the
// three refusals are explained once, in `RoadmapEngine::get`, where a
// maintainer reads them; only what a CALLER must know to form a call is here.
//
// Both fields use `string_or_seq` for the reason `add_chunk_priority_named_or_numeric`
// records: a wrong shape must reach the handler and come back as a readable
// soft error, because a hard `-32602` from deserialization cancels every
// sibling tool call in a parallel batch. It also lets a single id arrive as a
// bare string.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetArgs {
    /// The chunk ids to fetch.
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub ids: Vec<String>,
    /// Project each record to these fields. Omit for the whole record.
    #[serde(default, deserialize_with = "crate::infra::coerce::string_or_seq")]
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartChunkArgs {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompleteChunkArgs {
    pub id: String,
    /// Optional proof-of-ship cross-ref to attach (e.g. `task:<id>`,
    /// `check:<name>`).
    #[serde(default)]
    pub ship_ref: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkArgs {
    #[serde(default)]
    pub id: String,
    /// A cross-ref string: `think:N`, `task:X`, `action:N`, `check:X`, `chunk:X`.
    /// `ref`/`crossref` are accepted as forgiving aliases.
    #[serde(default, alias = "ref", alias = "crossref")]
    pub cross_ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordRefreshArgs {
    pub summary: String,
    /// think step ids that motivated the mutation.
    #[serde(default)]
    pub think_steps: Vec<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetGroupArgs {
    #[serde(default)]
    pub id: String,
    /// The workstream name. Omit or pass empty to CLEAR the group — ungrouped
    /// is a valid answer, not a gap.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeGroupsArgs {
    /// Only report a cluster of at least this many ungrouped chunks. The floor
    /// is the point: without it a roadmap proposes a container per one-off slug.
    #[serde(default = "default_min_shared")]
    pub min_shared: usize,
}

const fn default_min_shared() -> usize {
    4
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

// Field docs here are deliberately terse. Every `///` below is serialized into
// the tool's inputSchema and shipped in tools/list on every handshake, and the
// lane recipe in particular was written three times over — in the description
// the model reads, in the schema, and in the refusal a caller actually hits
// when they get it wrong. Two of those three are the ones that reach a person;
// the schema is the copy that only costs bytes.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FocusGetArgs {
    /// Required. The asking caller's stable identity.
    #[serde(default)]
    pub lane: String,
    /// Peek at this workstream instead of the focused one. Changes nothing.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FocusSetArgs {
    /// Required. The focusing caller's stable identity.
    #[serde(default)]
    pub lane: String,
    /// Exact workstream name, or an unambiguous fragment.
    #[serde(default)]
    pub group: String,
    /// shape | build | listen.
    #[serde(default)]
    pub mode: String,
    /// Release this lane's focus; `group` and `mode` are then ignored.
    #[serde(default)]
    pub clear: bool,
}

// ── Tool handlers ──────────────────────────────────────────────────

#[tool_router]
impl RoadmapService {
    #[tool(
        name = "roadmap_add_chunk",
        description = "Add a chunk (phase/item) to the roadmap.\n\nInputs: id (required, stable slug), title (required — the full claim, a sentence is fine), name (the short label a canvas node wears, max 24 chars, e.g. 'Quota ceiling'; omit it and one is derived from the id, send a sentence and the call is REJECTED), status ('backlog'|'pending'|'in_progress'|'blocked'|'done'|'obsoleted', default 'pending'), priority (integer OR a named level 'critical'|'high'|'medium'|'low' → 100/200/300/400; lower sorts earlier), description, acceptance (string[]), deps (string[] of chunk ids), shared (bool — committed vs gitignored partition), group (the workstream — PASS IT AT BIRTH; an ungrouped chunk is invisible to every focused read), content (RECOMMENDED — structured body: {version:1, summary, facts?[{label,value}], sections?[{heading, prose?, list?[{text,done?}]}]}; write the detail here so the webapp renders it as UI, and keep description to a short plain-prose fallback).\n\nReturns: the created chunk.\n\nPitfalls: id must be unique; malformed content is rejected with the exact field named; an unusable group refuses the whole call rather than creating an ungrouped chunk. roadmap_* is the long-horizon plan above ship_* objectives.",
        annotations(
            title = "Add roadmap chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_add_chunk(
        &self,
        Parameters(args): Parameters<AddChunkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let status = match args.status {
            Some(s) => match parse_status(&s) {
                Ok(st) => st,
                Err(e) => return Ok(Self::err_structured("invalid_args", e)),
            },
            None => ChunkStatus::Pending,
        };
        let content = match crate::content::parse_optional(args.content) {
            Ok(c) => c,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.add_chunk_with_content(
            args.id,
            args.title,
            args.name,
            status,
            args.priority,
            args.description,
            args.acceptance,
            args.deps,
            args.shared,
            content,
            args.group,
        ) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_set_status",
        description = "Transition a chunk to a new status, validated against the lifecycle table.\n\nInputs: id (required), status (required — backlog|pending|in_progress|blocked|done|obsoleted).\n\nReturns: the updated chunk.\n\nAn explicit transition also disposes any open status proposal on the chunk: transitioning to the suggested status accepts it, transitioning anywhere else supersedes it. The sweep re-proposes if the divergence persists.\n\nPitfalls: illegal transitions (e.g. done→obsoleted) are rejected.",
        annotations(
            title = "Set chunk status",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_set_status(
        &self,
        Parameters(args): Parameters<SetStatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let status = match parse_status(&args.status) {
            Ok(st) => st,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.set_status(&args.id, status) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_update_chunk",
        description = "Patch a chunk's descriptive fields (status is handled by roadmap_set_status).\n\nInputs: id (required); any of title, name (the short node label, max 24 chars — send an empty string to re-seed it from the id), priority, description, acceptance (string[]), deps (string[]), content — omitted fields are left unchanged. content (RECOMMENDED when a chunk carries real detail) replaces the structured body, same shape as roadmap_add_chunk.content — the shape the webapp renders as UI; keep description as the short plain-prose fallback. resolve_title_proposal ('accept'|'reject') resolves an open contested-title proposal: accept adopts the tracker's title into the plan, reject clears it so the plan's title flows again.\n\nblocked_by records why a chunk cannot be worked when the answer is NOT another chunk — a refuted premise, a wait on a person, an external dependency — so it stops living as prose in the title. clear_blocked_by:true retracts it, and retracting is meant to be as easy as recording: a blocker nobody clears rots into the stale prose the field replaced. Sending both is refused.\n\nReturns: the updated chunk.\n\nAn explicit differing edit of a proposed-about field disposes that field's open proposal: a title edit disposes the title proposal, a priority edit disposes the reprioritize proposal.",
        annotations(
            title = "Update chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_update_chunk(
        &self,
        Parameters(args): Parameters<UpdateChunkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Malformed content refuses the whole call up front — before the
        // proposal resolution below can take effect on its own.
        let content = match crate::content::parse_optional(args.content) {
            Ok(c) => c,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        // Resolution first: an explicit accept/reject is the human speaking
        // about the proposal, and a title patch in the same call then speaks
        // on top of the resolved plan rather than being silently disposed by
        // the update path's proposal-clearing.
        if let Some(verb) = args.resolve_title_proposal.as_deref() {
            let accept = match verb.trim().to_ascii_lowercase().as_str() {
                "accept" => true,
                "reject" => false,
                other => {
                    return Ok(Self::err_structured(
                        "invalid_state",
                        format!(
                            "resolve_title_proposal must be 'accept' or 'reject', got '{other}'"
                        ),
                    ));
                }
            };
            if let Err(e) = engine.resolve_title_proposal(&args.id, accept) {
                return Ok(Self::err_structured("invalid_state", e));
            }
        }
        // The name is its own verb on the engine, so it is applied before the
        // patch rather than threaded through it. Rejected outright when over
        // budget: a partially-applied update is better than installing a label
        // no node can wear.
        if let Some(n) = args.name.as_deref()
            && let Err(e) = engine.set_name(&args.id, n)
        {
            return Ok(Self::err_structured("invalid_args", e));
        }
        // Recording a blocker and retracting one are opposite gestures, so
        // asking for both in a single call is a contradiction rather than an
        // ordering question — refused before anything is written, because
        // picking a winner would make the loser silently ineffective.
        if args.blocked_by.is_some() && args.clear_blocked_by {
            return Ok(Self::err_structured(
                "invalid_args",
                "blocked_by and clear_blocked_by contradict each other — send one",
            ));
        }
        if let Some(b) = args.blocked_by {
            let kind = match crate::roadmap::domain::BlockerKind::from_wire(&b.kind) {
                Ok(k) => k,
                Err(e) => return Ok(Self::err_structured("invalid_args", e)),
            };
            // The engine owns the rules. This seam parses the wire kind and
            // then hands off — it does not re-check the reason or the evidence,
            // so there is exactly one place either rule can change.
            let blocked_by = match engine.validate_blocked_by(kind, b.reason, b.evidence) {
                Ok(v) => v,
                Err(e) => return Ok(Self::err_structured("invalid_args", e)),
            };
            if let Err(e) = engine.set_blocked_by(&args.id, blocked_by) {
                return Ok(Self::err_structured("invalid_state", e));
            }
        }
        if args.clear_blocked_by
            && let Err(e) = engine.clear_blocked_by(&args.id)
        {
            return Ok(Self::err_structured("invalid_state", e));
        }
        match engine.update_chunk(
            &args.id,
            args.title,
            args.priority,
            args.description,
            args.acceptance,
            args.deps,
            content,
        ) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_obsolete_chunk",
        description = "Mark a chunk obsoleted with a reason. Kept for history, never selected by roadmap_next.\n\nInputs: id (required), reason (required).\n\nReturns: the obsoleted chunk.",
        annotations(
            title = "Obsolete chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_obsolete_chunk(
        &self,
        Parameters(args): Parameters<ObsoleteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.obsolete_chunk(&args.id, args.reason) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_reprioritize",
        description = "Record a re-prioritization PROPOSAL. This does NOT reorder the roadmap — it attaches a suggested priority + reason for a human to accept. Re-prioritization stays a human decision.\n\nInputs: id (required), suggested_priority (integer OR a named level 'critical'|'high'|'medium'|'low', required), reason (required).\n\nReturns: the chunk with the pending proposal attached (its real priority is unchanged).\n\nIdempotent for an unchanged suggestion. Disposed by an explicit priority edit via roadmap_update_chunk (accepting = setting the suggested priority).",
        annotations(
            title = "Propose re-prioritization",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_reprioritize(
        &self,
        Parameters(args): Parameters<ReprioritizeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.propose_reprioritize(&args.id, args.suggested_priority, args.reason) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_next",
        description = "Return the next ready chunk: the lowest-priority `pending` chunk that carries no `blocked_by` and whose dependencies are all `done`. Returns null when nothing is ready.\n\nInputs: none.\n\nReturns: the chunk, or null.",
        annotations(
            title = "Next ready chunk",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_next(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let value = match engine.next() {
            Some(chunk) => serde_json::to_value(chunk).unwrap(),
            None => serde_json::Value::Null,
        };
        Ok(Self::ok_structured(value))
    }

    #[tool(
        name = "roadmap_status",
        description = "Roadmap snapshot: status counts, the next-ready chunk id, and the priority-sorted chunk list.\n\nInputs: none.\n\nReturns: counts + next + chunks. Call this to reconstruct the plan after context loss.",
        annotations(
            title = "Roadmap status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // Scope the roadmap lock so it's released BEFORE the signal lock below —
        // signal_promote takes signal→roadmap, so holding both here in the
        // opposite order could deadlock. We never hold both at once.
        let mut status = {
            let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            engine.status()
        };
        // Fold in a pending-signal count when the signal engine is
        // wired (composed at the wire layer — no RoadmapEngine→SignalEngine dep).
        if let Some(signal) = &self.signal
            && let Ok(sig) = signal.lock()
            && let Some(obj) = status.as_object_mut()
        {
            obj.insert(
                "pending_signals".into(),
                serde_json::json!(sig.pending_count(0.6)),
            );
        }
        Ok(Self::ok_structured(status))
    }

    #[tool(
        name = "roadmap_export",
        description = "Export the roadmap as a human-readable markdown projection or json.\n\nInputs: format ('markdown'|'json', default 'markdown').\n\nReturns: { format, roadmap }. The markdown view reproduces a ROADMAP.md-shaped document.",
        annotations(
            title = "Export roadmap",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_export(
        &self,
        Parameters(args): Parameters<ExportArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let output = engine.export(&args.format);
        Ok(Self::ok_structured(
            serde_json::json!({ "format": args.format, "roadmap": output }),
        ))
    }

    #[tool(
        name = "roadmap_get",
        description = "Read the FULL stored records — description, content, acceptance, deps, cross_refs — for a named handful of chunks. The verb between roadmap_status (every chunk, one truncated line each, no body) and roadmap_export (the whole database, ~1.5 MB on a mature roadmap and too large to return).\n\nInputs: ids (string[], required, a SET of at most 20 distinct ids — the largest set whose worst case still fits a tool result; a repeated id is answered once); fields (string[], optional) projects each record, JSON:API sparse-fieldset style, e.g. ['content'] or ['acceptance','deps'].\n\nReturns: { records, returned, unknown, fields }. Records come back in the order you named them.\n\nPitfalls: nothing here fails quietly. Over the cap is an ERROR, not a short answer; an id matching no chunk is listed in `unknown` rather than omitted, so you can tell 'no such chunk' from 'that chunk has no summary'; and an unrecognised field name is refused with the full vocabulary rather than ignored. `id` is returned whatever you project.",
        annotations(
            title = "Get chunks by id",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.get(&args.ids, &args.fields) {
            Ok(value) => Ok(Self::ok_structured(value)),
            Err(e) => Ok(Self::err_structured("invalid_args", e)),
        }
    }

    #[tool(
        name = "roadmap_start_chunk",
        description = "Begin work on a chunk: marks it in_progress and returns the `chunk:<id>` backref. Wire that backref into ship_set_objective (as the objective's scope/ref) and into a think step's execution_ref, then roadmap_link the resulting task:/think: refs back to close the loop.\n\nInputs: id (required).\n\nReturns: { chunk, backref, hint }.\n\nPitfalls: only Pending/Blocked chunks can start.",
        annotations(
            title = "Start chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_start_chunk(
        &self,
        Parameters(args): Parameters<StartChunkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let backref = CrossRef::RoadmapChunk(args.id.clone()).to_wire();
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.start_chunk(&args.id) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::json!({
                "chunk": serde_json::to_value(chunk).unwrap(),
                "backref": backref,
                "hint": "pass `backref` to ship_set_objective and a think execution_ref, then roadmap_link the resulting task:/think: refs back",
            }))),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_complete_chunk",
        description = "Mark a chunk done, optionally attaching the proof-of-ship cross-ref (e.g. task:<id> from the ship objective that realized it).\n\nInputs: id (required), ship_ref (optional cross-ref string).\n\nReturns: the completed chunk.\n\nPitfalls: a malformed ship_ref is rejected before the status changes; only InProgress chunks transition to done.",
        annotations(
            title = "Complete chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_complete_chunk(
        &self,
        Parameters(args): Parameters<CompleteChunkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.complete_chunk(&args.id, args.ship_ref.as_deref()) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_link",
        description = "Attach a cross-reference to a chunk, fusing the three families into one graph. Validated against the CrossRef wire format; duplicates are ignored.\n\nInputs: id (required), cross_ref (required — think:N | task:X | action:N | check:X | chunk:X).\n\nReturns: the updated chunk.",
        annotations(
            title = "Link chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_link(
        &self,
        Parameters(args): Parameters<LinkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.link_chunk(&args.id, &args.cross_ref) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_args", e)),
        }
    }

    #[tool(
        name = "roadmap_record_refresh",
        description = "Append a datestamped refresh note recording a roadmap mutation and the think step ids that motivated it (the /roadmap-refresh provenance, made first-class).\n\nInputs: summary (required), think_steps (u32[]).\n\nReturns: { recorded, summary, think_steps, total_refreshes }.",
        annotations(
            title = "Record refresh note",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_record_refresh(
        &self,
        Parameters(args): Parameters<RecordRefreshArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let total = engine.record_refresh(args.summary.clone(), args.think_steps.clone());
        Ok(Self::ok_structured(serde_json::json!({
            "recorded": true,
            "summary": args.summary,
            "think_steps": args.think_steps,
            "total_refreshes": total,
        })))
    }

    #[tool(
        name = "roadmap_set_group",
        description = "Put a chunk in a workstream, or take it out of one. The group is what a tracker maps to a container — a Linear PROJECT.\n\nInputs: id (required), group (the workstream name; omit or pass empty to CLEAR it).\n\nThe group is also the chunk's REGION on the tech-tree canvas, and the two readings pull different ways. A tracker is happy with no group: such a chunk mirrors as a plain issue with no project, and a container holding one item is worse than no container. A canvas is not: a chunk with no region is drawn in 'Uncharted ground', the one region for what nobody has placed. Prefer an existing region over a new one — the map level can only carry 20 marks.\n\nA name that repeats a chunk id prefix is REFUSED here and now, not reported by `doctor` later: 'signal' or 'saas' is a slug wearing a region's job, and a region has to be somewhere a person could point at. Name the place, not the prefix.\n\nReturns: the updated chunk, or an invalid_state error naming the slug it clashed with.",
        annotations(
            title = "Set a chunk's workstream",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_set_group(
        &self,
        Parameters(args): Parameters<SetGroupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.set_group(&args.id, args.group) {
            Ok(chunk) => Ok(Self::ok_structured(serde_json::to_value(chunk).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "roadmap_focus_get",
        description = "What one LANE is working on, and what is ready inside it. READ-ONLY — never sets, changes or clears a focus.\n\nInputs: lane (required — focus is per-caller; pass a stable identity you already have: your worktree's absolute path, your session id, or your task id); group (optional — peek at THAT workstream's frontier without focusing it).\n\nReturns: { lane, focus (null if this lane never focused), frontier, groups, persistent }. `frontier` = counts, ready list, every blocked chunk with its reason or unmet deps, and the next candidate — scoped to one workstream and unable to name a chunk outside it. `persistent` says whether the focus survives a restart.\n\nPitfalls: an unfocused or unknown lane is ANSWERED (focus:null plus the workstream list), not refused — you cannot read another lane's focus by guessing, only learn yours is unset.",
        annotations(
            title = "Read a lane's focus",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_focus_get(
        &self,
        Parameters(args): Parameters<FocusGetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let focus = engine.focus_get(&args.lane);
        // Which workstream the frontier describes: an explicit `group` wins,
        // otherwise the lane's own focus, otherwise nothing to report.
        let subject = args
            .group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .or_else(|| focus.map(|f| f.group.clone()));
        let frontier = match subject {
            // An explicit group that resolves is reported under its STORED
            // name; one that does not is refused in place rather than
            // silently reported as an empty workstream, which reads
            // identically to a real workstream nobody has filled.
            Some(g) => match engine.resolve_group(&g) {
                crate::roadmap::engine::GroupResolution::Exact(name) => engine.group_status(&name),
                other => serde_json::json!({ "error": other.explain(&g) }),
            },
            None => serde_json::Value::Null,
        };
        Ok(Self::ok_structured(serde_json::json!({
            "lane": args.lane,
            "focus": focus,
            "frontier": frontier,
            "groups": engine.groups(),
            "persistent": engine.persistence_enabled(),
        })))
    }

    #[tool(
        name = "roadmap_focus_set",
        description = "Point one LANE at a workstream and a mode, or release it. The ONLY tool that changes a focus, and it changes nothing else — no chunk status moves, no ordering is touched, no work is started.\n\nInputs: lane (required, never defaulted), group (exact workstream name or unambiguous fragment), mode ('shape' | 'build' | 'listen'), clear (releases this lane's focus; group/mode ignored).\n\nReturns: { lane, focus, frontier, persistent }.\n\nPitfalls: an UNKNOWN or AMBIGUOUS group writes NOTHING and returns the exact candidates — a failed switch cannot strand a lane somewhere it never asked to be, and a focus it already held survives untouched. A blank lane is refused, not collapsed into a shared default: a shared default would let a second agent silently re-point the first. No synonyms — 'implement' is not read as 'build'.",
        annotations(
            title = "Set a lane's focus",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_focus_set(
        &self,
        Parameters(args): Parameters<FocusSetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        if args.clear {
            // Validated even on the clear path: a blank lane cannot be allowed
            // to mean "clear whichever focus you like".
            if let Err(e) = crate::roadmap::domain::validate_lane(&args.lane) {
                return Ok(Self::err_structured("invalid_args", e));
            }
            let released = engine.focus_clear(&args.lane);
            return Ok(Self::ok_structured(serde_json::json!({
                "lane": args.lane,
                "focus": serde_json::Value::Null,
                "released": released,
                "persistent": engine.persistence_enabled(),
            })));
        }
        let mode = match crate::roadmap::domain::FocusMode::from_wire(&args.mode) {
            Ok(m) => m,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        match engine.focus_set(&args.lane, &args.group, mode) {
            Ok(focus) => {
                let frontier = engine.group_status(&focus.group);
                Ok(Self::ok_structured(serde_json::json!({
                    "lane": focus.lane,
                    "focus": focus,
                    "frontier": frontier,
                    "persistent": engine.persistence_enabled(),
                })))
            }
            Err(e) => Ok(Self::err_structured("invalid_args", e)),
        }
    }

    #[tool(
        name = "roadmap_propose_groups",
        description = "Which UNGROUPED chunks belong together, for YOU to name. Clusters them by shared id prefix where at least `min_shared` chunks share one (default 4).\n\nThis places nothing, and cannot. A region name that is contained in a chunk id prefix names a slug rather than a place, and a prefix is contained in itself — so every group this could guess would be rejected. It reports the grouping, which is the part a machine can honestly see, and leaves the naming to you.\n\nReturns: clusters (each with the shared prefix, the ungrouped chunk ids, and why that prefix cannot be the name), the groups already in play, and min_shared. Pick a real place name for each cluster and apply it with roadmap_set_group — which will refuse the prefix if you pass it back.",
        annotations(
            title = "Propose workstreams from chunk ids",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn roadmap_propose_groups(
        &self,
        Parameters(args): Parameters<ProposeGroupsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let clusters: Vec<_> = engine
            .propose_groups_from_ids(args.min_shared)
            .into_iter()
            .map(|c| {
                serde_json::json!({
                    "prefix": c.prefix,
                    "chunk_ids": c.chunk_ids,
                    "why_prefix_is_unfit": c.why_prefix_is_unfit,
                })
            })
            .collect();
        let groups = engine.groups();
        Ok(Self::ok_structured(serde_json::json!({
            "clusters": clusters,
            "min_shared": args.min_shared,
            "groups": groups,
            "placed": 0,
        })))
    }

    #[tool(
        name = "tracker_status",
        description = "Where this project's roadmap is mirrored, if anywhere. Read-only.\n\nInputs: none.\n\nReturns: enabled, provider, target, included (chunks opted in), not_included (ACTIVE chunks that are not — the drift), and inherits_new_chunks (whether new chunks are born in scope). Call this before tracker_setup to see whether there is anything to do, and after it to confirm what changed.\n\nPitfalls: `included` only ever grows, so it cannot tell you the scope went stale — read `not_included`. A non-zero not_included with inherits_new_chunks false means work is invisible to the tracker.",
        annotations(
            title = "Tracker mirroring status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn tracker_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (_, project_id, config) = crate::cli::tracker_config();
        let provider = config.provider.clone().unwrap_or_default();
        let (included, not_included) = if provider.is_empty() {
            (0, 0)
        } else {
            let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            (
                engine.chunks_opted_in(&provider).len(),
                engine.chunks_not_opted_in(&provider).len(),
            )
        };
        Ok(Self::ok_structured(serde_json::json!({
            "project_id": project_id,
            "enabled": crate::tracker::should_project(&config),
            "provider": config.provider,
            "target": config.target,
            "included": included,
            "not_included": not_included,
            "inherits_new_chunks": crate::tracker::inherited_opt_in(&config).is_some(),
        })))
    }

    #[tool(
        name = "tracker_setup",
        description = "Connect this project's roadmap to an issue tracker, end to end: check the destination exists, turn mirroring on, include the active chunks, and wire up unattended pushing.\n\nInputs: provider ('linear'|'github', default 'github'), into (REQUIRED — a Linear team key like ENG, or a GitHub owner/repo), name (display name if it has to be created), band (only include one priority band), initiative (name for the roof the mirrored projects file under; absent keeps the configured or directory-basename default), create_missing (default FALSE — see below), push_secs (default 300, 0 skips auto-push), dry_run.\n\nCREATE_MISSING WRITES TO SOMEBODY ELSE'S SYSTEM. It creates the team/board upstream, which cannot be undone from here. Leave it false unless the human you are working with has actually asked for the destination to be created. With it false, a missing destination stops the run and nothing at all is written.\n\nNOTE Linear team keys are letters and digits only (ENG, WOW) — Linear silently rewrites anything else into a different key.\n\nReturns: what it did — target_existed/target_created/target_unverified, mirroring_enabled, included[], already_included, auto_push, and any follow-up the human must do.\n\nAfter auto_push is set the MCP server must be RECONNECTED before the cadence starts; say so rather than assuming it is running.",
        annotations(
            title = "Set up tracker mirroring",
            read_only_hint = false,
            // Both true and both meant: it can create a team upstream, and it
            // reaches a third-party system over the network.
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true,
        )
    )]
    pub async fn tracker_setup(
        &self,
        Parameters(args): Parameters<TrackerSetupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let (data_dir, project_id, _) = crate::cli::tracker_config();
        let req = crate::cli::SetupRequest {
            provider: args.provider.clone(),
            into: args.into.clone(),
            name: args.name.clone(),
            band: args.band.clone(),
            initiative: args.initiative.clone(),
            // An immediate push would be a second network phase and a second
            // way to fail; the cadence or `tracker push` covers it.
            push: false,
            push_secs: args.push_secs,
            // NOT wired to anything a caller can set. `yes` is the CLI's
            // "skip the prompt" flag and this path has no prompt to skip —
            // `create_missing` is the whole of the decision here.
            yes: false,
            dry_run: args.dry_run,
        };

        let probe_cfg = crate::tracker::TrackerConfig {
            enabled: true,
            provider: Some(req.provider.trim().to_ascii_lowercase()),
            target: Some(req.into.trim().to_string()),
            ..crate::tracker::TrackerConfig::default()
        };
        let port = match crate::cli::tracker_port_async(&probe_cfg).await {
            Ok(p) => p,
            Err(e) => return Ok(Self::err_structured("invalid_target", e.to_string())),
        };

        // ── The network half. NO ENGINE LOCK IS HELD HERE. ────────────────
        // This await is why the split exists: holding `self.engine` across it
        // would be the lock-across-await defect this codebase forbids.
        let display = req.name.clone().unwrap_or_else(|| req.into.trim().into());
        let may_create = args.create_missing && !req.dry_run;
        let phase = match crate::cli::setup_probe(port.as_ref(), &display, may_create).await {
            Ok(p) => p,
            Err(e) => return Ok(Self::err_structured("tracker_error", e.to_string())),
        };

        use crate::cli::TargetPhase;
        if phase == TargetPhase::MissingAndNotCreated && !req.dry_run {
            // Refusing to write is the whole point: config naming a destination
            // that does not exist is the defect `tracker setup` was built to fix.
            return Ok(Self::err_structured(
                "target_missing",
                format!(
                    "'{}' does not exist on {}. Nothing was written. Ask the human \
                     whether to create it, then call again with create_missing: true \
                     — that writes to their tracker and cannot be undone from here.",
                    req.into.trim(),
                    req.provider.trim()
                ),
            ));
        }

        // ── The local half. Lock taken now, released before returning. ────
        // `setup_local` awaits nothing and prints nothing, so this is safe on
        // both counts — stdout here is the JSON-RPC transport.
        let cwd = std::env::current_dir().unwrap_or_default();
        let outcome = {
            let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            match crate::cli::setup_local(&mut engine, &req, &data_dir, &project_id, &cwd) {
                Ok(o) => o,
                Err(e) => return Ok(Self::err_structured("setup_failed", e.to_string())),
            }
        };

        let mut next_steps: Vec<String> = Vec::new();
        if outcome.auto_push.is_some() && !outcome.auto_push_unchanged {
            next_steps.push(
                "Reconnect the MCP server so the push cadence starts — it is only \
                 spawned at server startup."
                    .into(),
            );
        }
        if let Some(manual) = &outcome.auto_push_manual {
            next_steps.push(manual.clone());
        }
        if outcome.auto_push_no_entry {
            next_steps.push(
                "Auto-push was skipped: no think-and-ship MCP entry was found for \
                 this project — not in .mcp.json, .cursor/, .windsurf/, nor in \
                 ~/.claude.json. Run `think-and-ship init` here (or \
                 `claude mcp add`), then call again."
                    .into(),
            );
        }
        if !outcome.included.is_empty() {
            next_steps.push("Nothing has been sent yet; `tracker push` sends it now.".into());
        }

        Ok(Self::ok_structured(serde_json::json!({
            "project_id": project_id,
            "provider": req.provider,
            "target": req.into,
            "dry_run": req.dry_run,
            "target_existed": phase == TargetPhase::Present,
            "target_created": phase == TargetPhase::Created,
            "target_unverified": phase == TargetPhase::Unverifiable,
            "mirroring_enabled": outcome.mirroring_enabled,
            "included": outcome.included,
            "already_included": outcome.already_included,
            "auto_push_secs": outcome.auto_push,
            "auto_push_written_to": outcome.auto_push_at,
            "auto_push_unchanged": outcome.auto_push_unchanged,
            "auto_push_manual_step": outcome.auto_push_manual,
            "next_steps": next_steps,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Priority *parsing* is unit-tested in infra::coerce. Here we only prove
    // the arg structs wire it up and deserialize infallibly.

    #[test]
    fn add_chunk_priority_named_or_numeric() {
        let named: AddChunkArgs =
            serde_json::from_str(r#"{"id":"x","title":"t","priority":"high"}"#).unwrap();
        assert_eq!(named.priority, 200);

        let numeric: AddChunkArgs =
            serde_json::from_str(r#"{"id":"x","title":"t","priority":42}"#).unwrap();
        assert_eq!(numeric.priority, 42);

        // Omitted priority falls back to the serde default (0).
        let absent: AddChunkArgs = serde_json::from_str(r#"{"id":"x","title":"t"}"#).unwrap();
        assert_eq!(absent.priority, 0);

        // An unrecognized level is NOT an error — it coerces to 0. A -32602
        // here would cancel sibling tool calls, so infallibility is the point.
        let urgent: AddChunkArgs =
            serde_json::from_str(r#"{"id":"x","title":"t","priority":"urgent"}"#).unwrap();
        assert_eq!(urgent.priority, 0);

        // id/title absent → "" (not -32602); handler soft-errors on empty id.
        let bare: AddChunkArgs = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(bare.id, "");
        assert_eq!(bare.title, "");
    }

    #[test]
    fn content_input_schema_is_typed_and_still_described() {
        // The regression this guards: with `content` left untyped in the
        // input schema, schema-decoding clients (Claude Code) deliver the
        // object as a JSON-encoded STRING and every structured body is
        // rejected. The type declaration is what makes them send an object;
        // the doc-comment description must survive `schema_with` because it
        // is the only place the shape is steered from.
        for schema in [
            schemars::schema_for!(AddChunkArgs),
            schemars::schema_for!(UpdateChunkArgs),
        ] {
            let root = schema.to_value();
            let content = &root["properties"]["content"];
            assert_eq!(content["type"], serde_json::json!(["object", "null"]));
            let description = content["description"].as_str().unwrap_or_default();
            assert!(!description.is_empty(), "description lost: {root}");
        }

        // The shape itself is steered from exactly one place: the add tool.
        let add = schemars::schema_for!(AddChunkArgs).to_value();
        let steer = add["properties"]["content"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(steer.contains("version:1"), "shape line lost: {steer}");
    }

    #[test]
    fn reprioritize_priority_named_or_numeric() {
        let named: ReprioritizeArgs =
            serde_json::from_str(r#"{"id":"x","suggested_priority":"high","reason":"r"}"#).unwrap();
        assert_eq!(named.suggested_priority, 200);
    }

    // ── blocked-by-set-and-cleared ────────────────────────────────────────
    //
    // Driven through the TOOL seam — real wire JSON deserialized into
    // `UpdateChunkArgs`, then the handler — rather than through the engine
    // verbs, because everything the engine half does was already proven in
    // `chunk-blocked-by-vocabulary`. What is new here is the parameter
    // plumbing, and a parameter is exactly the thing an engine test cannot
    // reach: a field can be typed correctly and still be dropped, ignored,
    // or applied when it should not be.

    /// A service holding one chunk, ready to be patched.
    fn svc_with_chunk() -> RoadmapService {
        use crate::roadmap::domain::ChunkStatus;
        use crate::roadmap::engine::RoadmapEngine;

        let mut engine = RoadmapEngine::new("p".into());
        engine
            .add_chunk(
                "c".into(),
                "A chunk".into(),
                ChunkStatus::Pending,
                10,
                "d".into(),
                vec![],
                vec![],
                false,
            )
            .expect("chunk added");
        RoadmapService::new(engine)
    }

    /// Patch the chunk with raw wire JSON, exactly as a client would send it.
    async fn patch(svc: &RoadmapService, wire: &str) -> serde_json::Value {
        let args: UpdateChunkArgs = serde_json::from_str(wire).expect("wire deserializes");
        svc.roadmap_update_chunk(Parameters(args))
            .await
            .expect("handler returns")
            .structured_content
            .expect("structured content")
    }

    #[tokio::test]
    async fn a_blocker_is_set_through_the_tool_seam() {
        let svc = svc_with_chunk();
        let out = patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"awaiting_human","reason":"  needs a decision  ",
                "evidence":"think:42"}}"#,
        )
        .await;

        let b = &out["blocked_by"];
        assert_eq!(b["kind"], "awaiting_human");
        // Trimmed by the engine's validator, not by this seam — the seam is
        // asserted to PASS THROUGH rather than to clean up after the caller.
        assert_eq!(b["reason"], "needs a decision");
        assert_eq!(b["evidence"], "think:42");
        assert!(
            !b["blocked_at"].as_str().unwrap_or_default().is_empty(),
            "a blocker must be stamped: {out}"
        );
    }

    #[tokio::test]
    async fn omitting_blocked_by_leaves_a_stored_blocker_alone() {
        // THE RULE THIS TOOL ALREADY FOLLOWS ON SIX OTHER FIELDS. Without it,
        // any agent editing a description would silently retract a blocker it
        // never mentioned — the failure that makes an omitted-means-unchanged
        // tool unsafe to use at all.
        let svc = svc_with_chunk();
        patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"external","reason":"waiting on a vendor"}}"#,
        )
        .await;

        let out = patch(&svc, r#"{"id":"c","description":"an unrelated edit"}"#).await;
        assert_eq!(out["description"], "an unrelated edit");
        assert_eq!(
            out["blocked_by"]["reason"], "waiting on a vendor",
            "an unrelated patch retracted the blocker: {out}"
        );
    }

    #[tokio::test]
    async fn clearing_leaves_no_trace_on_disk() {
        // THE LOAD-BEARING PROPERTY. A clear that stored an emptied husk would
        // read as "blocked, reason unknown" to every later reader, which is
        // strictly worse than the prose this field replaced. The proof is
        // byte-level and derived: the whole record before and after, with only
        // the timestamps (which MUST move) lifted out.
        fn without_timestamps(mut v: serde_json::Value) -> serde_json::Value {
            let obj = v.as_object_mut().expect("chunk is an object");
            obj.remove("updated_at");
            v
        }

        let svc = svc_with_chunk();
        let pristine = patch(&svc, r#"{"id":"c"}"#).await;
        assert!(
            pristine.get("blocked_by").is_none(),
            "a chunk that was never blocked must carry no key: {pristine}"
        );

        patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"premise_unmet","reason":"no tenant yet"}}"#,
        )
        .await;
        let cleared = patch(&svc, r#"{"id":"c","clear_blocked_by":true}"#).await;

        assert!(
            cleared.get("blocked_by").is_none(),
            "clearing left a husk behind: {cleared}"
        );
        assert_eq!(
            without_timestamps(cleared),
            without_timestamps(pristine),
            "an unblocked chunk must be indistinguishable from one never blocked"
        );
    }

    #[tokio::test]
    async fn set_then_clear_then_set_again_through_the_tool_seam() {
        // The sequence a real chunk lives: blocked, unblocked when the premise
        // is re-tested, blocked again for a different reason. The second set is
        // the one that would fail if clearing had left the field in some
        // half-state the setter refused to overwrite.
        let svc = svc_with_chunk();
        for wire in [
            r#"{"id":"c","blocked_by":{"kind":"premise_refuted","reason":"first"}}"#,
            r#"{"id":"c","clear_blocked_by":true}"#,
            r#"{"id":"c","blocked_by":{"kind":"awaiting_human","reason":"second"}}"#,
        ] {
            patch(&svc, wire).await;
        }
        let out = patch(&svc, r#"{"id":"c"}"#).await;
        assert_eq!(out["blocked_by"]["kind"], "awaiting_human");
        assert_eq!(out["blocked_by"]["reason"], "second");
    }

    #[tokio::test]
    async fn a_blocker_is_restated_without_clearing_first() {
        // Replacement, not refusal: the reason a chunk is stuck changes more
        // often than the fact that it is, and requiring a clear in between
        // would put a gesture between a human and the truth.
        let svc = svc_with_chunk();
        patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"premise_unmet","reason":"not yet"}}"#,
        )
        .await;
        let out = patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"premise_refuted","reason":"never will be"}}"#,
        )
        .await;
        assert_eq!(out["blocked_by"]["kind"], "premise_refuted");
        assert_eq!(out["blocked_by"]["reason"], "never will be");
    }

    #[tokio::test]
    async fn an_unrecognised_kind_names_every_legal_value() {
        // Derived from the vocabulary rather than spelled out, so a fifth kind
        // added later is covered here without anyone remembering to come back.
        use crate::roadmap::domain::BlockerKind;

        let svc = svc_with_chunk();
        let out = patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"blocked-by-vibes","reason":"r"}}"#,
        )
        .await;

        assert_eq!(out["ok"], false, "a bad kind must be refused: {out}");
        let msg = out["message"].as_str().unwrap_or_default();
        for kind in BlockerKind::ALL {
            assert!(
                msg.contains(kind.as_wire()),
                "the error must name '{}' among the legal values, got: {msg}",
                kind.as_wire()
            );
        }

        let after = patch(&svc, r#"{"id":"c"}"#).await;
        assert!(
            after.get("blocked_by").is_none(),
            "a refused kind must write nothing: {after}"
        );
    }

    #[tokio::test]
    async fn clearing_a_chunk_that_is_not_blocked_is_loud() {
        // Matching resolve_title_proposal: a caller who believes there is a
        // blocker to retract when there is none is working from a stale
        // picture, and a silent success would confirm it.
        let svc = svc_with_chunk();
        let out = patch(&svc, r#"{"id":"c","clear_blocked_by":true}"#).await;
        assert_eq!(out["ok"], false, "a no-op clear reported success: {out}");
        assert!(
            out["message"]
                .as_str()
                .unwrap_or_default()
                .contains("no blocker"),
            "the error must say what was missing: {out}"
        );
    }

    #[tokio::test]
    async fn setting_and_clearing_in_one_call_is_refused_before_anything_is_written() {
        let svc = svc_with_chunk();
        let out = patch(
            &svc,
            r#"{"id":"c","blocked_by":{"kind":"external","reason":"r"},"clear_blocked_by":true}"#,
        )
        .await;
        assert_eq!(out["ok"], false, "a contradiction was accepted: {out}");

        let after = patch(&svc, r#"{"id":"c"}"#).await;
        assert!(
            after.get("blocked_by").is_none(),
            "the refused call still wrote a blocker: {after}"
        );
    }

    #[test]
    fn blocker_args_never_fail_deserialization() {
        // A hard -32602 out of deserialization cancels every sibling tool call
        // in a parallel batch, so malformed input must arrive at the handler
        // and come back as a readable soft error instead. Same rule as
        // `add_chunk_priority_named_or_numeric`.
        let empty: UpdateChunkArgs = serde_json::from_str(r#"{"id":"c","blocked_by":{}}"#).unwrap();
        let b = empty.blocked_by.expect("present");
        assert_eq!(b.kind, "");
        assert_eq!(b.reason, "");
        assert!(b.evidence.is_none());

        // The clear flag is lenient for the same reason a priority level is.
        let stringly: UpdateChunkArgs =
            serde_json::from_str(r#"{"id":"c","clear_blocked_by":"true"}"#).unwrap();
        assert!(stringly.clear_blocked_by);

        let absent: UpdateChunkArgs = serde_json::from_str(r#"{"id":"c"}"#).unwrap();
        assert!(absent.blocked_by.is_none());
        assert!(!absent.clear_blocked_by);
    }

    #[test]
    fn link_accepts_ref_alias() {
        let canonical: LinkArgs =
            serde_json::from_str(r#"{"id":"x","cross_ref":"think:5"}"#).unwrap();
        assert_eq!(canonical.cross_ref, "think:5");

        let aliased: LinkArgs = serde_json::from_str(r#"{"id":"x","ref":"think:5"}"#).unwrap();
        assert_eq!(aliased.cross_ref, "think:5");

        let aliased2: LinkArgs = serde_json::from_str(r#"{"id":"x","crossref":"task:y"}"#).unwrap();
        assert_eq!(aliased2.cross_ref, "task:y");
    }

    // ── Focus at the wire seam ─────────────────────────────────────────

    /// Two grouped chunks in two workstreams, plus one ungrouped, so a leak in
    /// either direction is visible.
    fn svc_with_workstreams() -> RoadmapService {
        use crate::roadmap::domain::ChunkStatus;
        use crate::roadmap::engine::RoadmapEngine;

        let mut engine = RoadmapEngine::new("p".into());
        for (id, priority, group) in [
            ("auth-1", 10u32, Some("Authentication")),
            ("auth-2", 30, Some("Authentication")),
            ("bill-1", 1, Some("Billing")),
            ("loose", 2, None),
        ] {
            engine
                .add_chunk(
                    id.into(),
                    format!("Chunk {id}"),
                    ChunkStatus::Pending,
                    priority,
                    String::new(),
                    vec![],
                    vec![],
                    false,
                )
                .expect("chunk added");
            if let Some(g) = group {
                engine
                    .set_group(id, Some(g.to_string()))
                    .expect("these names are real places, not id prefixes");
            }
        }
        RoadmapService::new(engine)
    }

    async fn focus_get(svc: &RoadmapService, wire: &str) -> serde_json::Value {
        let args: FocusGetArgs = serde_json::from_str(wire).expect("wire deserializes");
        svc.roadmap_focus_get(Parameters(args))
            .await
            .expect("handler returns")
            .structured_content
            .expect("structured content")
    }

    async fn focus_set(svc: &RoadmapService, wire: &str) -> serde_json::Value {
        let args: FocusSetArgs = serde_json::from_str(wire).expect("wire deserializes");
        svc.roadmap_focus_set(Parameters(args))
            .await
            .expect("handler returns")
            .structured_content
            .expect("structured content")
    }

    /// A chunk created over the wire WITH a group is focusable in one call.
    ///
    /// The reported symptom was six chunks added for a workstream, all landing
    /// ungrouped, all invisible to the focused frontier — and six follow-up
    /// `roadmap_set_group` calls needed to repair it. This is that whole flow
    /// through the tool seam, in one call.
    #[tokio::test]
    async fn a_chunk_added_with_a_group_is_focusable_without_a_second_call() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));

        let args: AddChunkArgs = serde_json::from_str(
            r#"{"id":"auth-1","title":"Rotate sessions","priority":10,"group":"Authentication"}"#,
        )
        .expect("wire deserializes");
        let created = svc
            .roadmap_add_chunk(Parameters(args))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(created["group"], "Authentication");

        // Focus and read the frontier: the chunk is there, with no set_group.
        let focused = focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;
        assert_eq!(focused["frontier"]["next"], "auth-1");
        assert_eq!(focused["frontier"]["ready_count"], 1);
    }

    /// Omitting `group` is unchanged behaviour — every existing caller keeps
    /// working and its chunks stay ungrouped.
    #[tokio::test]
    async fn omitting_the_group_still_creates_an_ungrouped_chunk() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));
        let args: AddChunkArgs =
            serde_json::from_str(r#"{"id":"loose","title":"t","priority":1}"#).unwrap();
        let created = svc
            .roadmap_add_chunk(Parameters(args))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert!(
            created.get("group").is_none_or(serde_json::Value::is_null),
            "an omitted group must leave the chunk ungrouped, not invent one"
        );
    }

    /// An unusable group refuses the whole call over the wire — it does not
    /// quietly create the chunk without one.
    #[tokio::test]
    async fn an_unusable_group_refuses_the_add_over_the_wire() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));
        let args: AddChunkArgs = serde_json::from_str(
            r#"{"id":"billing-1","title":"t","priority":1,"group":"Billing"}"#,
        )
        .unwrap();
        let res = svc
            .roadmap_add_chunk(Parameters(args))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(res["ok"], false);
        assert_eq!(res["error_kind"], "invalid_state");

        // Nothing was created — not even an ungrouped fallback.
        let status = svc
            .roadmap_status(Parameters(NoArgs {}))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(status["counts"]["total"], 0);
    }

    /// The happy path, end to end over the wire: set, then read back.
    #[tokio::test]
    async fn setting_a_focus_returns_the_lane_the_mode_and_its_frontier() {
        let svc = svc_with_workstreams();
        let set = focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;
        // Success carries no `ok` envelope — only a soft failure sets one, so
        // the absence of `ok:false` IS the success signal here.
        assert!(set.get("ok").is_none(), "success must not report an error");
        assert_eq!(set["lane"], "/w/a");
        assert_eq!(set["persistent"], false);
        assert_eq!(set["focus"]["group"], "Authentication");
        assert_eq!(set["focus"]["mode"], "build");
        // The frontier is the FOCUSED workstream's, not the roadmap's: bill-1
        // has the best priority overall and must not appear.
        assert_eq!(set["frontier"]["next"], "auth-1");
        assert_eq!(set["frontier"]["ready_count"], 2);

        let got = focus_get(&svc, r#"{"lane":"/w/a"}"#).await;
        assert_eq!(got["focus"]["group"], "Authentication");
        assert_eq!(got["frontier"]["next"], "auth-1");
    }

    /// A read is a read. `roadmap_focus_get` must not create a focus as a side
    /// effect of being asked about one.
    #[tokio::test]
    async fn reading_a_focus_never_creates_one() {
        let svc = svc_with_workstreams();
        let got = focus_get(&svc, r#"{"lane":"/w/never-focused"}"#).await;
        assert_eq!(got["focus"], serde_json::Value::Null);
        assert!(
            got["groups"]
                .as_array()
                .unwrap()
                .contains(&"Billing".into())
        );
        // Still nothing stored, so a second read agrees with the first.
        assert_eq!(
            focus_get(&svc, r#"{"lane":"/w/never-focused"}"#).await["focus"],
            serde_json::Value::Null
        );
    }

    /// Peeking at another workstream reports it WITHOUT switching to it.
    #[tokio::test]
    async fn peeking_at_a_group_reports_it_without_switching() {
        let svc = svc_with_workstreams();
        focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"shape"}"#,
        )
        .await;
        let peek = focus_get(&svc, r#"{"lane":"/w/a","group":"Billing"}"#).await;
        assert_eq!(peek["frontier"]["group"], "Billing");
        // The focus itself did not move.
        assert_eq!(peek["focus"]["group"], "Authentication");
        assert_eq!(peek["focus"]["mode"], "shape");
    }

    /// An unknown workstream is a soft error carrying the candidates, and it
    /// leaves an existing focus exactly where it was.
    #[tokio::test]
    async fn an_unknown_or_ambiguous_group_is_refused_over_the_wire_without_mutating() {
        let svc = svc_with_workstreams();
        focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;

        let bad = focus_set(&svc, r#"{"lane":"/w/a","group":"Payments","mode":"build"}"#).await;
        assert_eq!(bad["ok"], false);
        assert_eq!(bad["error_kind"], "invalid_args");
        let msg = bad["message"].as_str().unwrap();
        assert!(msg.contains("Authentication") && msg.contains("Billing"));

        // Untouched.
        let got = focus_get(&svc, r#"{"lane":"/w/a"}"#).await;
        assert_eq!(got["focus"]["group"], "Authentication");
        assert_eq!(got["focus"]["mode"], "build");
    }

    /// A blank lane is refused on BOTH verbs, including the clear path — the
    /// clear path is the one where a default would be most tempting and most
    /// destructive, since it would release somebody else's focus.
    #[tokio::test]
    async fn a_blank_lane_is_refused_on_set_and_on_clear() {
        let svc = svc_with_workstreams();
        focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;

        let set = focus_set(&svc, r#"{"lane":"","group":"Billing","mode":"build"}"#).await;
        assert_eq!(set["ok"], false);
        let clear = focus_set(&svc, r#"{"lane":"  ","clear":true}"#).await;
        assert_eq!(clear["ok"], false);

        // The real lane still holds its focus.
        assert_eq!(
            focus_get(&svc, r#"{"lane":"/w/a"}"#).await["focus"]["group"],
            "Authentication"
        );
    }

    /// An unknown mode is refused with the vocabulary, and no synonym is taken.
    #[tokio::test]
    async fn an_unknown_mode_is_refused_with_the_vocabulary() {
        let svc = svc_with_workstreams();
        let bad = focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"implement"}"#,
        )
        .await;
        assert_eq!(bad["ok"], false);
        let msg = bad["message"].as_str().unwrap();
        assert!(msg.contains("shape") && msg.contains("build") && msg.contains("listen"));
        assert_eq!(
            focus_get(&svc, r#"{"lane":"/w/a"}"#).await["focus"],
            serde_json::Value::Null,
            "a refused mode must not have created a focus"
        );
    }

    /// Clearing releases only the asking lane.
    #[tokio::test]
    async fn clearing_releases_only_the_asking_lane() {
        let svc = svc_with_workstreams();
        focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;
        focus_set(&svc, r#"{"lane":"/w/b","group":"Billing","mode":"listen"}"#).await;

        let cleared = focus_set(&svc, r#"{"lane":"/w/a","clear":true}"#).await;
        assert_eq!(cleared["released"], true);
        assert_eq!(
            focus_get(&svc, r#"{"lane":"/w/a"}"#).await["focus"],
            serde_json::Value::Null
        );
        assert_eq!(
            focus_get(&svc, r#"{"lane":"/w/b"}"#).await["focus"]["group"],
            "Billing"
        );
    }

    /// The compatibility promise: the two tools that took no input still take
    /// none, and still answer identically.
    #[tokio::test]
    async fn roadmap_next_and_status_keep_their_no_input_behaviour() {
        let svc = svc_with_workstreams();
        let before_next = svc
            .roadmap_next(Parameters(NoArgs {}))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        focus_set(
            &svc,
            r#"{"lane":"/w/a","group":"Authentication","mode":"build"}"#,
        )
        .await;
        let after_next = svc
            .roadmap_next(Parameters(NoArgs {}))
            .await
            .unwrap()
            .structured_content
            .unwrap();

        // Focusing does not narrow the GLOBAL next — the narrowing lives in the
        // focus frontier, so an existing caller sees no behaviour change.
        assert_eq!(before_next["id"], "bill-1");
        assert_eq!(after_next["id"], "bill-1");

        let status = svc
            .roadmap_status(Parameters(NoArgs {}))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(status["counts"]["total"], 4);
    }

    /// The compact summary carries the workstream, so a reader building a
    /// focused frontier from `roadmap_status` can tell the rows apart.
    #[tokio::test]
    async fn compact_chunk_summaries_carry_the_group() {
        let svc = svc_with_workstreams();
        let status = svc
            .roadmap_status(Parameters(NoArgs {}))
            .await
            .unwrap()
            .structured_content
            .unwrap();
        let rows = status["chunks"].as_array().unwrap();
        let row = |id: &str| rows.iter().find(|r| r["id"] == id).expect("row present");
        assert_eq!(row("auth-1")["group"], "Authentication");
        assert_eq!(row("bill-1")["group"], "Billing");
        // Ungrouped rows pay nothing rather than carrying a null.
        assert!(row("loose").get("group").is_none());
    }

    #[tokio::test]
    async fn roadmap_status_folds_in_pending_signal_count() {
        use crate::roadmap::engine::RoadmapEngine;
        use crate::signal::domain::SignalKind;
        use crate::signal::engine::SignalEngine;
        use std::sync::{Arc, Mutex};

        // A signal engine with one researched, above-threshold signal.
        let mut se = SignalEngine::new("p".into());
        let id = se
            .capture(SignalKind::Idea, "x".into(), "do it".into())
            .id
            .clone();
        se.research(&id, "looked".into(), 0.9, vec![], None)
            .unwrap();

        let svc = RoadmapService::new(RoadmapEngine::new("p".into()))
            .with_signal(Arc::new(Mutex::new(se)));
        let res = svc.roadmap_status(Parameters(NoArgs {})).await.unwrap();
        let sc = res.structured_content.expect("structured content");
        assert_eq!(sc["pending_signals"], 1);
    }

    // ── tracker_setup (tracker-mcp-setup) ─────────────────────────────────
    //
    // Three properties, and two of them are about what the handler must NEVER
    // do. Both would corrupt the JSON-RPC session rather than fail a call, so
    // neither can be caught by exercising the happy path.

    /// The production half of this file, so an assertion string naming a call
    /// cannot satisfy the assertion that looks for it.
    fn handler_source() -> &'static str {
        let whole = include_str!("handlers.rs");
        whole.split_once("\nmod tests").map_or(whole, |(b, _)| b)
    }

    /// TRAP 1. `confirm` reads STDIN, and in a stdio server stdin carries the
    /// JSON-RPC frames — a prompt reached from a handler eats protocol traffic
    /// and desynchronizes the session. It must be unreachable from here.
    ///
    /// HONEST SCOPE, as with the lock test below. Today `cli::confirm` is
    /// PRIVATE to the `cli` module, so this module cannot call it at all — the
    /// deliberately-broken build that proves this test bites had to make it
    /// `pub` first. Rust's privacy is the real guard; this test is what
    /// survives somebody widening that visibility for an unrelated reason.
    ///
    /// Worth recording what deliberately breaking it actually showed, because
    /// it is the failure mode itself: with `confirm` reachable, the EXECUTION
    /// test below did not fail — it HUNG, blocking forever on a stdin read.
    /// That is
    /// precisely what an MCP session would do. This source scan fails in 0.00s
    /// instead, which is the whole reason to keep a structural check alongside
    /// a behavioural one.
    #[test]
    fn no_tool_handler_can_prompt_on_stdin() {
        let src = handler_source();
        assert!(
            !src.contains("confirm("),
            "a tool handler reaches `confirm`, which reads stdin. In a stdio MCP \
             server stdin IS the transport, so this hangs or desyncs the session \
             rather than asking anybody anything. Consent must arrive as a \
             parameter (`create_missing`)"
        );
        // And the flag that skips the CLI's prompt must not be settable here —
        // there is no prompt to skip, so a true would only be confusing.
        assert!(
            src.contains("yes: false"),
            "the setup request built here must hard-code `yes: false`; the MCP \
             path has no prompt, and `create_missing` is the whole decision"
        );
    }

    /// TRAP 2. `run_stdio` hands STDOUT to the transport, so printing from a
    /// handler injects text into the protocol stream.
    #[test]
    fn no_tool_handler_writes_to_stdout() {
        let src = handler_source();
        for printer in ["println!", "print!"] {
            assert!(
                !src.contains(printer),
                "a tool handler calls `{printer}`. stdout is the JSON-RPC \
                 transport — this corrupts the stream. Return the value in \
                 structuredContent, or use `eprintln!` as the engine does"
            );
        }
    }

    /// TRAP 3, the one this codebase states as a standing rule: the engine mutex
    /// may not be held across an await.
    ///
    /// HONEST SCOPE, because this test is weaker than it looks and pretending
    /// otherwise would be the failure it is guarding against. Today rustc
    /// enforces this outright: `std::sync::MutexGuard` is not `Send`, the rmcp
    /// `#[tool]` macro requires a `Send` future, so locking before the probe is
    /// a COMPILE error ("future created by async block is not `Send`"). Verified
    /// by deliberately introducing the defect — the broken version would not
    /// build.
    ///
    /// So what is this test for? The moment somebody meets that compile error
    /// and "fixes" it by reaching for `tokio::sync::Mutex`, whose guard IS
    /// `Send`, the compiler goes quiet and the defect becomes real and silent.
    /// This asserts the ORDER survives that change. It is a guard on the future,
    /// not on the present.
    #[test]
    fn the_engine_lock_is_not_held_across_the_network_await() {
        let src = handler_source();
        let body = src
            .split_once("pub async fn tracker_setup(")
            .expect("tracker_setup must exist")
            .1;
        let body = body.split_once("\n    #[tool(").map_or(body, |(b, _)| b);

        let probe_at = body
            .find("setup_probe(")
            .expect("tracker_setup must probe the destination");
        let lock_at = body
            .find("self.engine.lock()")
            .expect("tracker_setup must lock the engine to write opt-ins");
        assert!(
            lock_at > probe_at,
            "the engine is locked BEFORE the network probe, so the lock is held \
             across an await — the defect this codebase forbids on every \
             objective. Probe first, then lock"
        );
        assert!(
            body[..probe_at].find("self.engine.lock()").is_none(),
            "an engine lock precedes the probe"
        );
    }

    /// The default that matters: an agent that omits `create_missing` cannot
    /// provision anything, and a missing destination writes NOTHING.
    #[tokio::test]
    async fn a_missing_destination_writes_nothing_when_create_missing_is_omitted() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));

        // Deserialized from the wire WITHOUT create_missing, which is how an
        // agent that has not been told to provision will call it.
        let args: TrackerSetupArgs =
            serde_json::from_value(serde_json::json!({ "into": "acme/widgets" }))
                .expect("args parse");
        assert!(
            !args.create_missing,
            "create_missing MUST default to false — an agent is further from a \
             human than a CLI user, and this writes to somebody else's system"
        );
        assert_eq!(
            args.provider, "github",
            "provider defaults like its siblings"
        );
        assert_eq!(args.push_secs, 300);

        // Driving the handler needs no network for the assertion that matters:
        // whatever happens, mirroring must not be silently switched on for a
        // destination nobody confirmed.
        let res = svc.tracker_setup(Parameters(args)).await.expect("no panic");
        let sc = res.structured_content.expect("structured content");
        assert_ne!(
            sc.get("target_created")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "nothing may be provisioned without an explicit create_missing"
        );
    }

    #[tokio::test]
    async fn tracker_status_reports_when_nothing_is_mirrored() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));
        let res = svc.tracker_status(Parameters(NoArgs {})).await.unwrap();
        let sc = res.structured_content.expect("structured content");
        assert!(
            sc.get("enabled").is_some() && sc.get("included").is_some(),
            "status must always answer both questions, even when off: {sc}"
        );
    }

    #[tokio::test]
    async fn roadmap_status_omits_count_when_signal_unwired() {
        use crate::roadmap::engine::RoadmapEngine;
        let svc = RoadmapService::new(RoadmapEngine::new("p".into()));
        let res = svc.roadmap_status(Parameters(NoArgs {})).await.unwrap();
        let sc = res.structured_content.expect("structured content");
        assert!(sc.get("pending_signals").is_none());
    }
}
