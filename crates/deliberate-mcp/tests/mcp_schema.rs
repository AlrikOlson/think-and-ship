//! 2026-style MCP wire-shape tests for `DeliberateService`.
//!
//! Asserts the tools/list and tools/call surface meet the post-2025-06-18
//! spec expectations: every tool carries `ToolAnnotations`, JSON-returning
//! tools advertise `output_schema`, and call results emit
//! `structured_content` so 2026 clients can validate and pattern-match
//! without parsing prose.

use std::collections::BTreeSet;

use deliberate_mcp::config::DeliberateConfig;
use deliberate_mcp::output_schemas::output_schema_for;
use deliberate_mcp::server::ReasoningServer;
use deliberate_mcp::tool::DeliberateService;
use deliberate_mcp::types::{
    DeliberateStep, ExportArgs, ImpactArgs, NextAction, NoArgs, PinArgs, ReviseEstimateArgs,
    SearchArgs, StatusArgs, StepLookupArgs,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

fn svc() -> DeliberateService {
    let mut cfg = DeliberateConfig::default();
    cfg.display.color_output = false;
    DeliberateService::new(ReasoningServer::new(cfg))
}

fn base_step(n: u32) -> DeliberateStep {
    DeliberateStep {
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
    "deliberate_record_step",
    "deliberate_engine_status",
    "deliberate_export_trace",
    "deliberate_get_step",
    "deliberate_search_trace",
    "deliberate_step_impact",
    "deliberate_pin_step",
    "deliberate_revise_estimate",
    "deliberate_set_branch_status",
    "deliberate_trace_checkpoint",
    "deliberate_wipe_trace",
];

const OLD_TOOL_NAMES: &[&str] = &[
    "deliberate",
    "deliberate_status",
    "deliberate_export",
    "deliberate_step",
    "deliberate_search",
    "deliberate_impact",
    "deliberate_pin",
    "deliberate_checkpoint",
    "deliberate_clear",
];

#[test]
fn tool_renames_old_names_removed() {
    let s = svc();
    let names: BTreeSet<String> = s
        .list_tools_view()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for old in OLD_TOOL_NAMES {
        assert!(
            !names.contains(*old),
            "old tool name {old:?} should be gone after the 0.2.0 rename, found in: {names:?}"
        );
    }
    let expected: BTreeSet<String> = EXPECTED_TOOL_NAMES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        names, expected,
        "tool list should match the post-rename expected set"
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

    // deliberate_engine_status: read-only, idempotent.
    let status = tools.get("deliberate_engine_status").unwrap();
    let ann = status.annotations.as_ref().unwrap();
    assert_eq!(ann.read_only_hint, Some(true));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(true));

    // deliberate_record_step: mutates, not destructive, not idempotent.
    let record = tools.get("deliberate_record_step").unwrap();
    let ann = record.annotations.as_ref().unwrap();
    assert_eq!(ann.read_only_hint, Some(false));
    assert_eq!(ann.destructive_hint, Some(false));
    assert_eq!(ann.idempotent_hint, Some(false));

    // deliberate_wipe_trace: destructive — the load-bearing hint.
    let wipe = tools.get("deliberate_wipe_trace").unwrap();
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
    // Spot-check: deliberate_export_trace returns format-dependent text,
    // so it intentionally has no output_schema.
    let exp = tools
        .iter()
        .find(|t| t.name == "deliberate_export_trace")
        .unwrap();
    assert!(
        exp.output_schema.is_none(),
        "deliberate_export_trace returns text — should NOT have output_schema"
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
        .deliberate_record_step(Parameters(base_step(1)))
        .await
        .expect("record_step should not return ErrorData");
    assert_eq!(result.is_error, Some(false).or(None));
    let sc = structured(&result);
    assert_eq!(sc["step_number"], 1);
    assert_eq!(sc["estimated_total"], 3);
    assert_eq!(sc["total_steps"], 1);
}

#[tokio::test]
async fn engine_status_call_returns_structured_content() {
    let s = svc();
    let result = s
        .deliberate_engine_status(Parameters(StatusArgs {
            verbose: Some(false),
        }))
        .await
        .expect("engine_status should not return ErrorData");
    let sc = structured(&result);
    assert!(sc["version"].is_string());
    assert!(sc["persistence_enabled"].is_boolean());
    assert!(sc["sessions_enabled"].is_boolean());
}

#[tokio::test]
async fn get_step_call_returns_structured_step() {
    let s = svc();
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_get_step(Parameters(StepLookupArgs {
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
async fn get_step_missing_emits_structured_error_envelope() {
    let s = svc();
    let result = s
        .deliberate_get_step(Parameters(StepLookupArgs {
            step_number: 999,
            resolve_latest: None,
        }))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let sc = structured(&result);
    assert_eq!(sc["error_kind"], "step_not_found");
    assert!(sc["message"].as_str().unwrap().contains("999"));
}

#[tokio::test]
async fn pin_step_call_returns_structured() {
    let s = svc();
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_pin_step(Parameters(PinArgs {
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
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_revise_estimate(Parameters(ReviseEstimateArgs {
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
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_step_impact(Parameters(ImpactArgs { step_number: 1 }))
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
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_search_trace(Parameters(SearchArgs {
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
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_trace_checkpoint(Parameters(NoArgs {}))
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
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_wipe_trace(Parameters(NoArgs {}))
        .await
        .unwrap();
    let sc = structured(&result);
    assert_eq!(sc["cleared"], true);
    // Engine should now be empty.
    let status = s
        .deliberate_engine_status(Parameters(StatusArgs { verbose: None }))
        .await
        .unwrap();
    let status_sc = structured(&status);
    assert_eq!(status_sc["total_steps"], 0);
}

#[tokio::test]
async fn export_trace_returns_text_no_structured_content() {
    // The one tool that intentionally returns format-dependent text:
    // structuredContent must be absent so clients know to treat as text.
    let s = svc();
    s.deliberate_record_step(Parameters(base_step(1)))
        .await
        .unwrap();
    let result = s
        .deliberate_export_trace(Parameters(ExportArgs {
            format: Some("json".into()),
        }))
        .await
        .unwrap();
    assert!(
        result.structured_content.is_none(),
        "deliberate_export_trace should return text only, not structured"
    );
    assert!(!result.content.is_empty(), "should have text content");
}
