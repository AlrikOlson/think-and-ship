//! 2026-style MCP wire-shape tests for `ThinkService`.
//!
//! Asserts the tools/list and tools/call surface meet the post-2025-06-18
//! spec expectations: every tool carries `ToolAnnotations`, JSON-returning
//! tools advertise `output_schema`, and call results emit
//! `structured_content` so 2026 clients can validate and pattern-match
//! without parsing prose.

use std::collections::BTreeSet;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::domain::{NextAction, ThinkStep};
use think_and_ship::think::engine::core::ReasoningServer;
use think_and_ship::think::mcp::args::{
    ExportArgs, ImpactArgs, NoArgs, PinArgs, ReviseEstimateArgs, SearchArgs, StepLookupArgs,
};
use think_and_ship::think::mcp::service::ThinkService;
use think_and_ship::think::output_schemas::output_schema_for;

fn svc() -> ThinkService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    ThinkService::new(ReasoningServer::new(cfg))
}

fn base_step(n: u32) -> ThinkStep {
    ThinkStep {
        step_number: n,
        estimated_total: 3,
        purpose: "analysis".into(),
        context: "Test context".into(),
        thought: "Test thought".into(),
        outcome: "Test outcome".into(),
        next_action: NextAction::Text("Test next action".into()),
        rationale: "Test rationale".into(),
        confidence: None,
        uncertainty_notes: None,
        revises_step: None,
        revision_reason: None,
        revised_by: None,
        is_final_step: None,
        branch_from: None,
        branch_id: None,
        branch_name: None,
        tools_used: None,
        dependencies: None,
        timestamp: None,
        duration_ms: None,
        session_id: None,
        pinned: None,
        cwd: None,
        execution_ref: None,
        record_id: None,
    }
}

fn structured(result: &CallToolResult) -> &serde_json::Value {
    result
        .structured_content
        .as_ref()
        .expect("expected structured_content on the CallToolResult")
}

// ─── Tool list shape ────────────────────────────────────────────────────

const EXPECTED_TOOL_NAMES: &[&str] = &[
    "think_record_step",
    "think_engine_status",
    "think_export_trace",
    "think_get_step",
    "think_search_trace",
    "think_step_impact",
    "think_pin_step",
    "think_revise_estimate",
    "think_set_branch_status",
    "think_trace_checkpoint",
    "think_wipe_trace",
];

#[test]
fn tools_list_has_exactly_the_think_canonicals() {
    let s = svc();
    let names: BTreeSet<String> = s
        .list_tools_view()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    // Set EQUALITY, not containment: any name this family stops serving, and
    // any name it starts serving without being told to, both fail here. That
    // subsumes checking a hand-maintained list of names it must not serve.
    let expected: BTreeSet<String> = EXPECTED_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        names, expected,
        "tool list should expose exactly the 11 think_* canonical names"
    );
}

#[test]
fn tools_list_carries_annotations() {
    let s = svc();
    for tool in s.list_tools_view() {
        let ann = tool
            .annotations
            .unwrap_or_else(|| panic!("tool {:?} should carry ToolAnnotations", tool.name));
        // Every tool gets a human-readable title.
        assert!(
            ann.title.is_some(),
            "tool {:?} missing annotations.title",
            tool.name
        );
        // Every tool declares all four hint booleans. (Some may be None,
        // but we set all four on every tool, so check at least the
        // open_world_hint = false invariant — none of our tools touch
        // external systems.)
        assert_eq!(
            ann.open_world_hint,
            Some(false),
            "tool {:?} should have openWorldHint=false (engine-local)",
            tool.name
        );
    }
}

#[test]
fn tools_list_carries_hints() {
    let s = svc();
    let tools: std::collections::HashMap<String, _> = s
        .list_tools_view()
        .into_iter()
        .map(|t| (t.name.to_string(), t))
        .collect();

    // think_engine_status: read-only, idempotent.
    let status = tools.get("think_engine_status").unwrap();
    let ann = status.annotations.as_ref().unwrap();
    assert_eq!(ann.read_only_hint, Some(true));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(true));

    // think_record_step: mutates, not destructive, not idempotent.
    let record = tools.get("think_record_step").unwrap();
    let ann = record.annotations.as_ref().unwrap();
    assert_eq!(ann.read_only_hint, Some(false));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(false));

    // think_wipe_trace: destructive — the load-bearing hint.
    let wipe = tools.get("think_wipe_trace").unwrap();
    let ann = wipe.annotations.as_ref().unwrap();
    assert_eq!(
        ann.destructive_hint,
        Some(true),
        "wipe must signal destructive=true for client confirmation gating"
    );
}

#[test]
fn tools_list_attaches_output_schema_to_json_returning_tools() {
    let s = svc();
    let tools = s.list_tools_view();
    for tool in &tools {
        let expects_schema = output_schema_for(&tool.name).is_some();
        if expects_schema {
            assert!(
                tool.output_schema.is_some(),
                "tool {:?} should have output_schema attached",
                tool.name
            );
        }
    }
    // Spot-check: think_export_trace returns format-dependent text,
    // so it intentionally has no output_schema.
    let exp = tools
        .iter()
        .find(|t| t.name == "think_export_trace")
        .unwrap();
    assert!(
        exp.output_schema.is_none(),
        "think_export_trace returns text — should NOT have output_schema"
    );
}

#[test]
fn tool_descriptions_carry_pitfalls_marker() {
    // arXiv:2602.14878 (Feb 2026) found descriptions with explicit
    // pitfall/gotcha sections score highest on agent selection accuracy.
    // Every tool in our surface should advertise its pitfalls.
    let s = svc();
    for tool in s.list_tools_view() {
        let desc = tool
            .description
            .as_deref()
            .unwrap_or_else(|| panic!("tool {:?} missing description", tool.name));
        assert!(
            desc.to_ascii_lowercase().contains("pitfall"),
            "tool {:?} description missing 'Pitfall' section: {desc}",
            tool.name
        );
    }
}

// ─── Call results carry structured_content ──────────────────────────────

#[tokio::test]
async fn record_step_call_returns_structured_content() {
    let s = svc();
    let result = s
        .think_record_step(Parameters(base_step(1)))
        .await
        .expect("record_step should not return ErrorData");
    assert_eq!(result.is_error, Some(false).or(None));
    let sc = structured(&result);
    assert_eq!(sc["step_number"], 1);
    assert_eq!(sc["estimated_total"], 3);
    assert_eq!(sc["total_steps"], 1);
}

// `engine_status_call_returns_structured_content` used to live here. It called
// the handler directly, and the handler now takes the request's own
// `RequestContext` so it can report the live client (roadmap chunk
// `mcp-stderr-observability-gap`) — which is not constructible without a wire.
// The claim was not weakened, it MOVED: `tests/client_capability_view.rs`
// asserts the same version/persistence/sessions_enabled fields against a real
// client over a real duplex, in
// `the_engine_fields_are_untouched_by_the_client_block`.

#[tokio::test]
async fn get_step_call_returns_structured_step() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_get_step(Parameters(StepLookupArgs {
            step_number: 1,
            resolve_latest: None,
        }))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["step_number"], 1);
    assert_eq!(sc["purpose"], "analysis");
}

#[tokio::test]
async fn get_step_missing_emits_soft_error_envelope() {
    let s = svc();
    let result = s
        .think_get_step(Parameters(StepLookupArgs {
            step_number: 999,
            resolve_latest: None,
        }))
        .await
        .unwrap();
    // A logical "not found" carries the error_kind envelope BUT must NOT be
    // marked is_error — otherwise it cancels sibling tool calls in a parallel
    // batch (anthropics/claude-code#22264). See infra::tool_result::soft_error.
    assert_eq!(
        result.is_error,
        Some(false),
        "logical failure must stay is_error:false (cascade-safe)"
    );
    let sc = structured(&result);
    assert_eq!(sc["ok"], false);
    assert_eq!(sc["error_kind"], "step_not_found");
    assert!(sc["message"].as_str().unwrap().contains("999"));
}

#[tokio::test]
async fn pin_step_call_returns_structured() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_pin_step(Parameters(PinArgs {
            step_number: 1,
            pinned: Some(true),
        }))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["step_number"], 1);
    assert_eq!(sc["was_pinned"], false);
    assert_eq!(sc["now_pinned"], true);
}

#[tokio::test]
async fn revise_estimate_call_returns_structured() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_revise_estimate(Parameters(ReviseEstimateArgs {
            estimated_total: 7,
            reason: Some("Scope expanded".into()),
        }))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["previous"], 3);
    assert_eq!(sc["new_estimate"], 7);
    assert_eq!(sc["reason"], "Scope expanded");
}

#[tokio::test]
async fn step_impact_call_returns_structured() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_step_impact(Parameters(ImpactArgs { step_number: 1 }))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["step_number"], 1);
    assert!(sc["upstream"].is_object());
    assert!(sc["downstream"].is_object());
    assert!(sc["revision_chain"].is_array());
}

#[tokio::test]
async fn search_trace_call_returns_structured() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_search_trace(Parameters(SearchArgs {
            query: "thought".into(),
            limit: Some(5),
        }))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["query"], "thought");
    assert!(sc["matches"].is_array());
}

#[tokio::test]
async fn trace_checkpoint_call_returns_structured() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_trace_checkpoint(Parameters(NoArgs {}))
        .await
        .unwrap();
    let sc = structured(&result);
    assert!(sc["open_hypotheses"].is_array());
    assert!(sc["stale_branches"].is_array());
    assert!(sc["confidence_trend"].is_string());
}

#[tokio::test]
async fn wipe_trace_call_returns_structured_and_clears() {
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s.think_wipe_trace(Parameters(NoArgs {})).await.unwrap();
    let sc = structured(&result);
    assert_eq!(sc["cleared"], true);
    // Engine should now be empty. Probed through `think_search_trace` rather
    // than `think_engine_status`: the latter now takes the request's own
    // `RequestContext` (see the note above `get_step_call_returns_structured_step`)
    // and cannot be called without a wire. A trace whose only step had purpose
    // "analysis" and which now matches nothing is the same emptiness claim.
    let found = s
        .think_search_trace(Parameters(SearchArgs {
            query: "analysis".into(),
            limit: None,
        }))
        .await
        .unwrap();
    assert_eq!(structured(&found)["match_count"], 0);
}

#[tokio::test]
async fn export_trace_returns_text_no_structured_content() {
    // The one tool that intentionally returns format-dependent text:
    // structuredContent must be absent so clients know to treat as text.
    let s = svc();
    s.think_record_step(Parameters(base_step(1))).await.unwrap();
    let result = s
        .think_export_trace(Parameters(ExportArgs {
            format: Some("json".into()),
        }))
        .await
        .unwrap();
    assert!(
        result.structured_content.is_none(),
        "think_export_trace should return text only, not structured"
    );
    assert!(!result.content.is_empty(), "should have text content");
}
