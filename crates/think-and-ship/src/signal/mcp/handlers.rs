//! The `signal_*` tool handlers — rmcp `#[tool]` methods over [`SignalService`],
//! mirroring `roadmap::mcp::handlers`. Args deserialize forgivingly (absent
//! fields become defaults, never a -32602 that would cancel sibling calls);
//! logical failures return a soft `{ok:false, …}` envelope.

use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use super::service::SignalService;
use crate::roadmap::domain::ChunkStatus;
use crate::signal::domain::{SignalKind, SignalStatus};

impl SignalService {
    pub(crate) fn make_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::tool_router()
    }
}

/// Parse a wire kind string into the domain enum (snake_case).
fn parse_kind(s: &str) -> Result<SignalKind, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|_| format!("invalid kind '{s}' (expected question|idea|concern|bug|feedback)"))
}

// ── Arg types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CaptureArgs {
    /// One of: question, idea, concern, bug, feedback.
    #[serde(default)]
    pub kind: String,
    /// Who raised the signal (display name / handle / email).
    #[serde(default)]
    pub from: String,
    /// The signal content.
    #[serde(default)]
    pub body: String,
    /// PREFER THIS over packing detail into `body` — the structured body that
    /// renders as UI: {version:1, summary: one plain sentence (no shorthand,
    /// no ids), facts?: [{label,value}], sections?: [{heading, prose?:
    /// markdown, list?: [{text, done?}]}]}. `body` stays the plain-prose
    /// fallback.
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetArgs {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LinkArgs {
    pub id: String,
    /// A cross-ref string: `think:N`, `task:X`, `action:N`, `check:X`,
    /// `chunk:X`, `signal:X`. `ref`/`crossref` accepted as forgiving aliases.
    #[serde(default, alias = "ref", alias = "crossref")]
    pub cross_ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PromoteArgs {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResearchArgs {
    pub id: String,
    /// A summary of what the agent found this round.
    #[serde(default)]
    pub summary: String,
    /// Agent confidence in the enrichment, 0..1 (clamped).
    #[serde(default)]
    pub confidence: f64,
    /// External references consulted (URLs, doc ids, ministr symbol ids).
    #[serde(default)]
    pub sources: Vec<String>,
    /// The think_* step number that motivated this enrichment (cross-ref'd).
    #[serde(default)]
    pub think_step: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PendingArgs {
    /// Minimum surfacing confidence (max enrichment confidence), 0..1. Default 0.6.
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Relevance hints (active files / keywords). When given, only signals whose
    /// body or a cross-ref contains a hint are returned. Empty = no filter.
    #[serde(default)]
    pub hints: Vec<String>,
    /// Max signals to return. Default 20.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SurfaceArgs {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnoozeArgs {
    pub id: String,
    /// Minutes to suppress the signal from signal_pending. Default 60.
    #[serde(default)]
    pub minutes: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IgnoreArgs {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoArgs {}

/// Derive a stable roadmap chunk id from a signal id. The full signal UUID
/// keeps chunk ids collision-free across signals.
fn chunk_id_for(signal_id: &str) -> String {
    format!("signal-{signal_id}")
}

/// A short, single-line title for a promoted chunk: the kind tag + a truncated
/// body.
fn promoted_title(kind: SignalKind, body: &str) -> String {
    let kind_str = match kind {
        SignalKind::Question => "question",
        SignalKind::Idea => "idea",
        SignalKind::Concern => "concern",
        SignalKind::Bug => "bug",
        SignalKind::Feedback => "feedback",
    };
    let first_line = body.lines().next().unwrap_or("").trim();
    let snippet: String = first_line.chars().take(80).collect();
    if snippet.is_empty() {
        format!("[{kind_str}] (signal)")
    } else {
        format!("[{kind_str}] {snippet}")
    }
}

// ── Tool handlers ──────────────────────────────────────────────────

#[tool_router]
impl SignalService {
    #[tool(
        name = "signal_capture",
        description = "Capture a stakeholder signal (question | idea | concern | bug | feedback) into the local store.\n\nInputs: kind (required — question|idea|concern|bug|feedback), from (who raised it), body (the content), content (RECOMMENDED — structured body: {version:1, summary, facts?[{label,value}], sections?[{heading, prose?, list?[{text,done?}]}]}; write the detail here so the webapp renders it as UI, and keep body to the person's plain words).\n\nReturns: the created signal (id minted, status 'new', created timestamp).\n\nPitfalls: malformed content is rejected with the exact field named.\n\nNote: the local store is a cache of the cloud system-of-record; cloud write-through arrives with the SyncTarget::Cloud client.",
        annotations(
            title = "Capture signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn signal_capture(
        &self,
        Parameters(args): Parameters<CaptureArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let kind = match parse_kind(&args.kind) {
            Ok(k) => k,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        if args.body.trim().is_empty() {
            return Ok(Self::err_structured("invalid_args", "signal body is empty"));
        }
        let content = match crate::content::parse_optional(args.content) {
            Ok(c) => c,
            Err(e) => return Ok(Self::err_structured("invalid_args", e)),
        };
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let signal = engine.capture_with_content(kind, args.from, args.body, content);
        Ok(Self::ok_structured(serde_json::to_value(signal).unwrap()))
    }

    #[tool(
        name = "signal_status",
        description = "Signal inbox snapshot: counts by lifecycle state (new/triaged/researched/surfaced/promoted/dismissed) + a newest-first, bounded list of signal summaries.\n\nInputs: none.\n\nReturns: { project_id, counts, signals, omitted }.",
        annotations(
            title = "Signal status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn signal_status(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        Ok(Self::ok_structured(engine.status()))
    }

    #[tool(
        name = "signal_get",
        description = "Fetch a single signal by id, including its enrichment trail and cross-refs.\n\nInputs: id (required).\n\nReturns: the signal, or a soft { ok:false, error_kind:'not_found' } when no such id.",
        annotations(
            title = "Get signal",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn signal_get(
        &self,
        Parameters(args): Parameters<GetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.get(&args.id) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("not_found", e)),
        }
    }

    #[tool(
        name = "signal_link",
        description = "Attach a typed cross-reference to a signal, fusing it into the think/ship/roadmap graph.\n\nInputs: id (required), cross_ref (required — think:N | task:X | action:N | check:X | chunk:X | signal:X).\n\nReturns: the updated signal. Duplicates are ignored.",
        annotations(
            title = "Link signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn signal_link(
        &self,
        Parameters(args): Parameters<LinkArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.link(&args.id, &args.cross_ref) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_args", e)),
        }
    }

    #[tool(
        name = "signal_promote",
        description = "Promote a validated (researched or surfaced) signal into a backlog roadmap chunk — the opportunity→solution move. Writes bidirectional cross-refs: chunk:<id> onto the signal, signal:<id> onto the new chunk, so provenance runs both ways.\n\nInputs: id (required).\n\nReturns: { signal_id, chunk_id, created, signal/chunk }. Idempotent: a signal already promoted returns its existing chunk with created=false (no duplicate).",
        annotations(
            title = "Promote signal to chunk",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn signal_promote(
        &self,
        Parameters(args): Parameters<PromoteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(roadmap) = self.roadmap.clone() else {
            return Ok(Self::err_structured(
                "not_wired",
                "signal_promote requires the roadmap engine (not wired in this context)",
            ));
        };

        // Step 1 — read what we need from the signal under a SCOPED lock, then
        // drop it (we never hold the signal + roadmap locks at once).
        let (already_chunk, promotable, kind, body) = {
            let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            let signal = match engine.get(&args.id) {
                Ok(s) => s,
                Err(e) => return Ok(Self::err_structured("not_found", e)),
            };
            let already = signal
                .cross_refs
                .iter()
                .find_map(|r| r.strip_prefix("chunk:").map(str::to_string));
            let promotable = matches!(
                signal.status,
                SignalStatus::Researched | SignalStatus::Surfaced
            );
            (already, promotable, signal.kind, signal.body.clone())
        };

        // Idempotent: already promoted → return the existing chunk, no new one.
        if let Some(chunk_id) = already_chunk {
            let chunk = {
                let r = roadmap.lock().map_err(|_| Self::poisoned())?;
                r.roadmap()
                    .chunks
                    .iter()
                    .find(|c| c.id == chunk_id)
                    .map(|c| serde_json::to_value(c).unwrap())
            };
            return Ok(Self::ok_structured(serde_json::json!({
                "signal_id": args.id,
                "chunk_id": chunk_id,
                "created": false,
                "chunk": chunk,
            })));
        }

        if !promotable {
            return Ok(Self::err_structured(
                "invalid_state",
                "signal must be researched or surfaced before it can be promoted",
            ));
        }

        // Step 2 — create the backlog chunk + the signal:<id> back-ref under a
        // SCOPED roadmap lock.
        let chunk_id = chunk_id_for(&args.id);
        let title = promoted_title(kind, &body);
        let description = format!("Promoted from signal {}. Original: {body}", args.id);
        {
            let mut r = roadmap.lock().map_err(|_| Self::poisoned())?;
            // If the chunk already exists (a prior partial promote), keep going
            // and just (re)assert the link — promotion stays idempotent.
            let _ = r.add_chunk(
                chunk_id.clone(),
                title,
                ChunkStatus::Backlog,
                0,
                description,
                Vec::new(),
                Vec::new(),
                false,
            );
            if let Err(e) = r.link_chunk(&chunk_id, &format!("signal:{}", args.id)) {
                return Ok(Self::err_structured("invalid_state", e));
            }
        }

        // Step 3 — mark the signal Promoted + write the chunk:<id> ref under a
        // SCOPED signal lock.
        let signal_value = {
            let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            if let Err(e) = engine.promote(&args.id) {
                return Ok(Self::err_structured("invalid_state", e));
            }
            let _ = engine.link(&args.id, &format!("chunk:{chunk_id}"));
            serde_json::to_value(engine.get(&args.id).unwrap()).unwrap()
        };

        Ok(Self::ok_structured(serde_json::json!({
            "signal_id": args.id,
            "chunk_id": chunk_id,
            "created": true,
            "signal": signal_value,
        })))
    }

    #[tool(
        name = "signal_research",
        description = "Record one round of churning on a signal: append a durable enrichment { think_step?, sources[], summary, confidence } and advance the signal's lifecycle toward 'researched' (new→triaged→researched). When think_step is given it is cross-ref'd onto the signal so the reasoning is auditable.\n\nInputs: id (required), summary (required), confidence (0..1, clamped), sources (string[]), think_step (u32, optional).\n\nReturns: the updated signal. Rejects a promoted/dismissed (terminal) signal; never moves status backward.",
        annotations(
            title = "Research signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn signal_research(
        &self,
        Parameters(args): Parameters<ResearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.summary.trim().is_empty() {
            return Ok(Self::err_structured(
                "invalid_args",
                "research summary is empty",
            ));
        }
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.research(
            &args.id,
            args.summary,
            args.confidence,
            args.sources,
            args.think_step,
        ) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "signal_pending",
        description = "Return signals ready to raise to the human under earned-interruption discipline: status 'researched', max-enrichment-confidence >= min_confidence, NOT already surfaced, NOT snoozed, and (when hints are given) relevant to the active context. Never returns an un-researched or low-confidence signal.\n\nInputs: min_confidence (0..1, default 0.6), hints (string[] of active files/keywords — empty = no relevance filter), limit (default 20).\n\nReturns: { count, signals } (highest-confidence first).",
        annotations(
            title = "Pending signals",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn signal_pending(
        &self,
        Parameters(args): Parameters<PendingArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let min_confidence = args.min_confidence.unwrap_or(0.6);
        let limit = args.limit.unwrap_or(20) as usize;
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let pending = engine.pending(min_confidence, &args.hints, limit);
        Ok(Self::ok_structured(serde_json::json!({
            "count": pending.len(),
            "signals": pending,
        })))
    }

    #[tool(
        name = "signal_surface",
        description = "Mark a signal as surfaced (raised to the human): researched -> surfaced + stamps surfaced_at, so signal_pending won't re-raise it.\n\nInputs: id (required).\n\nReturns: the updated signal. Rejects a signal that isn't researched.",
        annotations(
            title = "Surface signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn signal_surface(
        &self,
        Parameters(args): Parameters<SurfaceArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.surface(&args.id) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }

    #[tool(
        name = "signal_snooze",
        description = "Snooze a signal: suppress it from signal_pending for `minutes` (sets snooze_until = now + minutes). Status is unchanged.\n\nInputs: id (required), minutes (default 60).\n\nReturns: the updated signal.",
        annotations(
            title = "Snooze signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn signal_snooze(
        &self,
        Parameters(args): Parameters<SnoozeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let minutes = args.minutes.unwrap_or(60);
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.snooze(&args.id, minutes) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("not_found", e)),
        }
    }

    #[tool(
        name = "signal_ignore",
        description = "Ignore a signal: dismiss it (terminal). Use when a surfaced signal isn't worth acting on.\n\nInputs: id (required).\n\nReturns: the dismissed signal.",
        annotations(
            title = "Ignore signal",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn signal_ignore(
        &self,
        Parameters(args): Parameters<IgnoreArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.ignore(&args.id) {
            Ok(signal) => Ok(Self::ok_structured(serde_json::to_value(signal).unwrap())),
            Err(e) => Ok(Self::err_structured("invalid_state", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_args_default_when_absent() {
        // Missing fields deserialize to "" (never a -32602); the handler
        // soft-errors on an empty/invalid kind.
        let bare: CaptureArgs = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(bare.kind, "");
        assert_eq!(bare.from, "");
        assert_eq!(bare.body, "");
    }

    #[test]
    fn parse_kind_accepts_vocabulary_and_rejects_others() {
        assert_eq!(parse_kind("feedback").unwrap(), SignalKind::Feedback);
        assert_eq!(parse_kind("bug").unwrap(), SignalKind::Bug);
        assert!(parse_kind("rant").is_err());
    }

    #[test]
    fn chunk_id_uses_full_signal_id() {
        assert_eq!(chunk_id_for("f1c9a4e2-1234"), "signal-f1c9a4e2-1234");
    }

    // ── signal_promote / signal_link (drive the async handlers, assert on the
    //    real engine state — the test that matters) ──────────────────────────

    use crate::roadmap::engine::RoadmapEngine;
    use crate::signal::engine::SignalEngine;
    use std::sync::{Arc, Mutex};

    /// A SignalService over a fresh signal engine + a shared roadmap engine,
    /// with one signal already advanced to `Researched`. Returns (svc, roadmap, id).
    fn promotable() -> (SignalService, Arc<Mutex<RoadmapEngine>>, String) {
        let roadmap = Arc::new(Mutex::new(RoadmapEngine::new("proj".into())));
        let mut engine = SignalEngine::new("proj".into());
        let id = engine
            .capture(SignalKind::Idea, "dana".into(), "add a dark theme".into())
            .id
            .clone();
        engine.set_status(&id, SignalStatus::Triaged).unwrap();
        engine.set_status(&id, SignalStatus::Researched).unwrap();
        let svc = SignalService::new(engine).with_roadmap(Arc::clone(&roadmap));
        (svc, roadmap, id)
    }

    #[tokio::test]
    async fn promote_creates_chunk_with_bidirectional_refs() {
        let (svc, roadmap, id) = promotable();
        svc.signal_promote(Parameters(PromoteArgs { id: id.clone() }))
            .await
            .unwrap();

        let chunk_id = format!("signal-{id}");
        // The chunk exists, is backlog, and carries signal:<id>.
        {
            let r = roadmap.lock().unwrap();
            let chunk = r
                .roadmap()
                .chunks
                .iter()
                .find(|c| c.id == chunk_id)
                .expect("chunk created");
            assert_eq!(chunk.status, ChunkStatus::Backlog);
            assert!(chunk.cross_refs.contains(&format!("signal:{id}")));
        }
        // The signal is Promoted and carries chunk:<chunk_id>.
        {
            let e = svc.engine.lock().unwrap();
            let s = e.get(&id).unwrap();
            assert_eq!(s.status, SignalStatus::Promoted);
            assert!(s.cross_refs.contains(&format!("chunk:{chunk_id}")));
        }
    }

    #[tokio::test]
    async fn promote_is_idempotent_no_duplicate_chunk() {
        let (svc, roadmap, id) = promotable();
        svc.signal_promote(Parameters(PromoteArgs { id: id.clone() }))
            .await
            .unwrap();
        // Second promote must not create a second chunk.
        svc.signal_promote(Parameters(PromoteArgs { id: id.clone() }))
            .await
            .unwrap();
        let count = roadmap
            .lock()
            .unwrap()
            .roadmap()
            .chunks
            .iter()
            .filter(|c| c.id.starts_with("signal-"))
            .count();
        assert_eq!(count, 1, "re-promote must be a no-op");
    }

    #[tokio::test]
    async fn promote_rejects_unresearched_signal() {
        let roadmap = Arc::new(Mutex::new(RoadmapEngine::new("proj".into())));
        let mut engine = SignalEngine::new("proj".into());
        let id = engine
            .capture(SignalKind::Bug, "x".into(), "boom".into())
            .id
            .clone(); // status New
        let svc = SignalService::new(engine).with_roadmap(Arc::clone(&roadmap));

        svc.signal_promote(Parameters(PromoteArgs { id: id.clone() }))
            .await
            .unwrap();
        // No chunk, signal untouched.
        assert!(roadmap.lock().unwrap().roadmap().chunks.is_empty());
        assert_eq!(
            svc.engine.lock().unwrap().get(&id).unwrap().status,
            SignalStatus::New
        );
    }

    #[tokio::test]
    async fn link_attaches_validated_crossref() {
        let mut engine = SignalEngine::new("proj".into());
        let id = engine
            .capture(SignalKind::Question, "x".into(), "why?".into())
            .id
            .clone();
        let svc = SignalService::new(engine); // no roadmap needed for link

        svc.signal_link(Parameters(LinkArgs {
            id: id.clone(),
            cross_ref: "think:42".into(),
        }))
        .await
        .unwrap();
        assert!(
            svc.engine
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .cross_refs
                .contains(&"think:42".to_string())
        );
    }
}
