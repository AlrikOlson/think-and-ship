//! The 11 `#[tool]` handler methods, all in one `#[tool_router]` impl.
//!
//! The macro requires every annotated tool to live in a single impl
//! block. Description strings stay inline — the `#[tool]` macro accepts
//! only string literals (no `const`/`path` expressions), so a separate
//! `descriptions.rs` would require a build script. Inline is cleaner.
//!
//! Description shape (all 11 follow the same template):
//!
//!   1. When to use — one short sentence orienting tool selection.
//!   2. Inputs — references the inputSchema; lists required vs optional.
//!   3. Returns — names the outputSchema type and its key fields.
//!   4. Pitfalls — known failure modes and how to avoid them.

use rmcp::{
    ErrorData,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content},
    tool, tool_router,
};

use super::args::{
    BranchStatusArgs, ExportArgs, ImpactArgs, NoArgs, PinArgs, ReviseEstimateArgs, SearchArgs,
    StatusArgs, StepLookupArgs,
};
use crate::think::config::OutputFormat;
use crate::think::domain::DeliberateStep;
use crate::think::output_schemas::{
    PinStepOutput, ReviseEstimateOutput, SearchTraceOutput, SetBranchStatusOutput, WipeTraceOutput,
};

use super::service::ThinkService;

/// Pub-crate bridge to the `#[tool_router]`-generated `tool_router()`
/// helper. The macro emits it as a private associated function in this
/// module; sibling modules (notably `super::service::ThinkService::new`)
/// can't reach across the privacy boundary on their own. This re-exports
/// it under a stable name without touching the macro.
impl ThinkService {
    pub(crate) fn make_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::tool_router()
    }
}

#[tool_router]
impl ThinkService {
    #[tool(
        name = "think_record_step",
        description = "Record one reasoning step in a structured trace. Use any time you'd otherwise just think internally — write the step instead so the trace viewer and later turns of this conversation can audit the reasoning.\n\nInputs: see inputSchema. Required (server will diagnose if missing): step_number, estimated_total, purpose, context, thought, outcome, next_action, rationale. Optional: confidence, uncertainty_notes, revises_step + revision_reason, is_final_step, branch_from + branch_id + branch_name, tools_used, dependencies, session_id, pinned, execution_ref.\n\nEach field is a SEPARATE top-level JSON parameter — `outcome` is its own parameter, NOT a section inside `thought`. Same for `rationale`, `next_action`, etc.\n\nReturns: structuredContent shaped by outputSchema (RecordStepOutput) — step_number, total_steps, optional excerpts, recent_steps rollup, warnings, branches_summary.\n\nPitfalls:\n\n❌ Do NOT serialize the rest of the call inside `thought` using XML tags:\n  \"thought\": \"Reasoning here. </thought><outcome>The result</outcome><rationale>...</rationale>\"\nThe harness reads `</thought>` as the end of `thought`; sibling parameters never reach the server. The most common failure in production traces — even Claude Code itself does this when not warned. The server will auto-recover when possible and tell you exactly what happened.\n\n✅ DO pass each as its own parameter:\n  { \"thought\": \"Reasoning here.\", \"outcome\": \"The result\", \"rationale\": \"...\" }\n\nOther pitfalls: step numbers are project-global, never reuse. To continue an EXISTING branch, pass branch_from + the branch_id from a prior response — inventing a new id creates a sub-branch. Use `execution_ref` to link this reasoning step to a resolute-mcp entity (e.g. \"task:auth-refactor\", \"action:42\").",
        annotations(
            title = "Record reasoning step",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn think_record_step(
        &self,
        Parameters(step): Parameters<DeliberateStep>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = {
            let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
            engine.process_step(step)
        };
        match result {
            Ok(ok) => match serde_json::from_str::<serde_json::Value>(&ok.text) {
                Ok(v) => Ok(CallToolResult::structured(v)),
                Err(_) => Ok(CallToolResult::success(vec![Content::text(ok.text)])),
            },
            Err(err) => match serde_json::from_str::<serde_json::Value>(&err.text) {
                Ok(v) => Ok(CallToolResult::structured_error(v)),
                Err(_) => Ok(CallToolResult::error(vec![Content::text(err.text)])),
            },
        }
    }

    #[tool(
        name = "think_engine_status",
        description = "Inspect engine state — config, counts, version, optionally per-session detail. Use to check whether persistence is on, which session is active, and how big the trace is before making big decisions.\n\nInputs: see inputSchema. Optional `verbose: true` folds in pinned[] and sessions[] arrays.\n\nReturns: structuredContent shaped by outputSchema (EngineStatusOutput).\n\nPitfalls: status reflects the live process — call again after env-var changes won't show new values until the server restarts.",
        annotations(
            title = "Engine snapshot",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_engine_status(
        &self,
        Parameters(args): Parameters<StatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let v = engine.status_snapshot(args.verbose.unwrap_or(false));
        Ok(CallToolResult::structured(v))
    }

    #[tool(
        name = "think_export_trace",
        description = "Export the trace in a human-readable format. Pick `markdown` (default, badged steps) for review, `json` for downstream tooling, `console` for ANSI-styled terminal output, `tree` for branch structure.\n\nInputs: see inputSchema. Optional `format`: \"markdown\" | \"json\" | \"console\" | \"tree\".\n\nReturns: text content. NOTE: this tool intentionally has no outputSchema because the shape depends on the chosen format.\n\nPitfalls: `console` includes ANSI escapes — only useful if the consumer renders them. For programmatic consumers, prefer `json`.",
        annotations(
            title = "Export trace",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_export_trace(
        &self,
        Parameters(args): Parameters<ExportArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let fmt_raw = args.format.as_deref().map(str::trim).unwrap_or("");
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let text = match fmt_raw {
            "" | "markdown" | "md" => engine.export_history(OutputFormat::Markdown),
            "json" => engine.export_history(OutputFormat::Json),
            "console" | "ansi" | "terminal" => engine.export_history(OutputFormat::Console),
            "tree" | "branches" => engine.branch_tree(),
            other => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Unknown format \"{other}\". Use: markdown, json, console, tree."
                ))]));
            }
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        name = "think_get_step",
        description = "Fetch one step by its project-global step_number. Use to inspect a specific step, including branch steps.\n\nInputs: see inputSchema. Required: `step_number`. Optional: `resolve_latest: true` walks revised_by forward to return the live canonical revision instead of the historical original.\n\nReturns: structuredContent — a DeliberateStep object (see outputSchema).\n\nPitfalls: returns a structured error envelope (error_kind=\"step_not_found\") when no step exists. Branch steps share the project-global numbering — there's no separate index.",
        annotations(
            title = "Fetch step",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_get_step(
        &self,
        Parameters(args): Parameters<StepLookupArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let found = if args.resolve_latest.unwrap_or(false) {
            engine.latest_revision_of(args.step_number)
        } else {
            engine.step_by_number(args.step_number)
        };
        match found {
            Some(s) => match serde_json::to_value(&s) {
                Ok(v) => Ok(CallToolResult::structured(v)),
                Err(_) => Ok(Self::structured_err(
                    "serialization_failed",
                    "Failed to serialize step",
                )),
            },
            None => Ok(Self::structured_err(
                "step_not_found",
                format!("No step #{} found.", args.step_number),
            )),
        }
    }

    #[tool(
        name = "think_search_trace",
        description = "Case-insensitive substring search across every step's thought, outcome, context, purpose, rationale, and next_action. Use when the trace has grown past the recent_steps window and you need to locate an earlier claim.\n\nInputs: see inputSchema. Required: `query`. Optional: `limit` (default 10, min 1).\n\nReturns: structuredContent shaped by outputSchema (SearchTraceOutput) — query, match_count, matches[] with per-hit excerpt and matched_field.\n\nPitfalls: substring search is literal, not semantic. Lowercase the needle yourself if your query mixes case sensitivity expectations. Excerpts are ~60 chars around the match — use think_get_step for full text.",
        annotations(
            title = "Search trace",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_search_trace(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(10).max(1) as usize;
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let hits = engine.search_steps(&args.query, limit);
        // The typed SearchTraceOutput is used only for schema generation
        // (compile-time). The actual structuredContent assembles the
        // engine's `Vec<Value>` directly so any drift between the typed
        // shape and engine output produces a visible mismatch rather
        // than a silent shape change at the deserialize boundary.
        let body = serde_json::to_value(SearchTraceOutput {
            query: args.query.clone(),
            match_count: hits.len() as u32,
            matches: Vec::new(),
        })
        .unwrap_or(serde_json::Value::Null);
        let body = if let serde_json::Value::Object(mut map) = body {
            map.insert("matches".into(), serde_json::Value::Array(hits));
            serde_json::Value::Object(map)
        } else {
            serde_json::json!({
                "query": args.query,
                "match_count": 0,
                "matches": [],
            })
        };
        Ok(CallToolResult::structured(body))
    }

    #[tool(
        name = "think_step_impact",
        description = "Dependency + revision graph around a step. Use BEFORE revising a load-bearing step — answers \"if I change this, what else re-breaks?\".\n\nInputs: see inputSchema. Required: `step_number`.\n\nReturns: structuredContent shaped by outputSchema (StepImpactOutput) — upstream (direct + transitive deps), downstream (direct + transitive dependents, partitioned by relation label), revision_chain (original → revised-by → ...), branches_from.\n\nPitfalls: returns structured error envelope (error_kind=\"step_not_found\") when no step exists. Transitive walks are capped at 256 nodes — pathological cycles get truncated, not infinite-looped.",
        annotations(
            title = "Step impact graph",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_step_impact(
        &self,
        Parameters(args): Parameters<ImpactArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.impact_of(args.step_number) {
            Ok(v) => Ok(CallToolResult::structured(v)),
            Err(e) => Ok(Self::structured_err("step_not_found", e)),
        }
    }

    #[tool(
        name = "think_pin_step",
        description = "Pin a step so it stays in recent_steps rollups even after the chronological window moves past it. Use for the original problem statement, confirmed root causes, anchoring conclusions. Pass pinned=false to unpin.\n\nInputs: see inputSchema. Required: `step_number`. Optional: `pinned` (defaults to true).\n\nReturns: structuredContent shaped by outputSchema (PinStepOutput) — step_number, was_pinned, now_pinned.\n\nPitfalls: pinning is local to the trace; pinned status persists with the session file when persistence is enabled.",
        annotations(
            title = "Pin/unpin step",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_pin_step(
        &self,
        Parameters(args): Parameters<PinArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let want_pinned = args.pinned.unwrap_or(true);
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.pin_step(args.step_number, want_pinned) {
            Ok(prev) => {
                let out = PinStepOutput {
                    step_number: args.step_number,
                    was_pinned: prev,
                    now_pinned: want_pinned,
                };
                match serde_json::to_value(out) {
                    Ok(v) => Ok(CallToolResult::structured(v)),
                    Err(_) => Ok(Self::structured_err(
                        "serialization_failed",
                        "Failed to serialize pin result",
                    )),
                }
            }
            Err(e) => Ok(Self::structured_err("step_not_found", e)),
        }
    }

    #[tool(
        name = "think_revise_estimate",
        description = "Adjust `estimated_total` on the most recently recorded step, in place — no new step is appended. Use when you realize the work will take more (or fewer) steps than you initially declared.\n\nInputs: see inputSchema. Required: `estimated_total` (>= 1). Optional: `reason` (free text, surfaced back in the response).\n\nReturns: structuredContent shaped by outputSchema (ReviseEstimateOutput) — previous, new_estimate, reason.\n\nPitfalls: fails (error_kind=\"no_steps\") if called before any think_record_step. Only the LAST step's estimated_total is touched; older steps keep their historical estimates.",
        annotations(
            title = "Adjust total estimate",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false,
        )
    )]
    pub async fn think_revise_estimate(
        &self,
        Parameters(args): Parameters<ReviseEstimateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.revise_estimate(args.estimated_total) {
            Ok((previous, new_estimate)) => {
                let reason = args
                    .reason
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let out = ReviseEstimateOutput {
                    previous,
                    new_estimate,
                    reason,
                };
                match serde_json::to_value(out) {
                    Ok(v) => Ok(CallToolResult::structured(v)),
                    Err(_) => Ok(Self::structured_err(
                        "serialization_failed",
                        "Failed to serialize estimate revision",
                    )),
                }
            }
            Err(e) => Ok(Self::structured_err("no_steps", e)),
        }
    }

    #[tool(
        name = "think_set_branch_status",
        description = "Mark a branch \"active\", \"merged\", or \"abandoned\". Steps stay; only the branch label changes. When status=\"merged\", optionally pass merged_into=<step_number> to record which synthesis step aggregated the branch back into the main line.\n\nInputs: see inputSchema. Required: `branch_id`, `status` (one of: active, merged, abandoned). Optional: `merged_into` (step number).\n\nReturns: structuredContent shaped by outputSchema (SetBranchStatusOutput) — branch_id, previous_status, new_status, merged_into.\n\nPitfalls: unknown branch_id returns structured error envelope with error_kind=\"unknown_branch\" (and lists known ids). Moving away from \"merged\" clears any prior merged_into pointer so stale pointers don't survive.",
        annotations(
            title = "Set branch status",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_set_branch_status(
        &self,
        Parameters(args): Parameters<BranchStatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        match engine.set_branch_status(&args.branch_id, &args.status, args.merged_into) {
            Ok((prev, new)) => {
                let merged_into = if args.status.trim().eq_ignore_ascii_case("merged") {
                    args.merged_into
                } else {
                    None
                };
                let out = SetBranchStatusOutput {
                    branch_id: args.branch_id.clone(),
                    previous_status: prev.to_string(),
                    new_status: new.to_string(),
                    merged_into,
                };
                match serde_json::to_value(out) {
                    Ok(v) => Ok(CallToolResult::structured(v)),
                    Err(_) => Ok(Self::structured_err(
                        "serialization_failed",
                        "Failed to serialize branch status",
                    )),
                }
            }
            Err(e) => Ok(Self::structured_err("unknown_branch", e)),
        }
    }

    #[tool(
        name = "think_trace_checkpoint",
        description = "Trace-wide metacognitive snapshot. Aggregates per-step warnings into patterns that only show up across the whole history — open hypotheses with no validation downstream, stale branches, the confidence trend, revisions whose dependents didn't acknowledge them, and steps that transitively depend on something refuted later.\n\nInputs: none.\n\nReturns: structuredContent shaped by outputSchema (TraceCheckpointOutput) — open_hypotheses, stale_branches, confidence_trend, revised_but_undefended, refuted_chain_alerts.\n\nPitfalls: confidence_trend reports `insufficient_data` when fewer than 2 confidence values exist; not an error. Call this periodically on long traces — per-step warnings can't see these whole-trace patterns.",
        annotations(
            title = "Trace checkpoint",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_trace_checkpoint(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        let v = engine.checkpoint_snapshot();
        Ok(CallToolResult::structured(v))
    }

    #[tool(
        name = "think_wipe_trace",
        description = "Wipe the trace: steps, branches, sessions, active-session pointer, and persisted files. DESTRUCTIVE — there is no undo, and on-disk session files are deleted too.\n\nInputs: none.\n\nReturns: structuredContent shaped by outputSchema (WipeTraceOutput) — { cleared: true }.\n\nPitfalls: this is intentionally a separate, destructively-named tool. Do not call to \"start fresh\" on a session — use a new session_id instead, which leaves the old trace recoverable.",
        annotations(
            title = "Wipe trace",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false,
        )
    )]
    pub async fn think_wipe_trace(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut engine = self.engine.lock().map_err(|_| Self::poisoned())?;
        engine.clear_history();
        match serde_json::to_value(WipeTraceOutput { cleared: true }) {
            Ok(v) => Ok(CallToolResult::structured(v)),
            Err(_) => Ok(CallToolResult::success(vec![Content::text(
                "Deliberation history cleared.".to_string(),
            )])),
        }
    }
}
