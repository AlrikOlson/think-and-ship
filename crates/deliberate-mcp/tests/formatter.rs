//! Mirrors `tests/formatter.test.ts`.

use std::collections::BTreeMap;

use deliberate_mcp::formatter::Formatter;
use deliberate_mcp::types::{
    Branch, BranchStatus, DeliberateHistory, DeliberateStep, HistoryMetadata, NextAction,
    StructuredAction,
};
use serde_json::json;

fn base_step() -> DeliberateStep {
    DeliberateStep {
        step_number: 1,
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

#[test]
fn console_basic_no_color() {
    let f = Formatter::new(false);
    let out = f.format_step_console(&base_step());
    assert!(out.contains("[Step 1/3] ANALYSIS"));
    assert!(out.contains("Context: Test context"));
    assert!(out.contains("Thought: Test thought"));
    assert!(out.contains("Outcome: Test outcome"));
    assert!(out.contains("Next: Test next action - Test rationale"));
}

#[test]
fn console_includes_confidence_percentage() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.confidence = Some(0.85);
    let out = f.format_step_console(&step);
    assert!(out.contains("[85%]"), "missing 85% in: {out}");
}

#[test]
fn console_shows_revision_info() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.revises_step = Some(2);
    step.revision_reason = Some("Found error in step 2".into());
    let out = f.format_step_console(&step);
    assert!(out.contains("Revises #2"));
    assert!(out.contains("Revision Reason: Found error in step 2"));
}

#[test]
fn console_shows_branch_info() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.branch_from = Some(1);
    step.branch_name = Some("Alternative approach".into());
    let out = f.format_step_console(&step);
    assert!(out.contains("Branch from #1"));
    assert!(out.contains("(Alternative approach)"));
}

#[test]
fn console_shows_uncertainty() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.uncertainty_notes = Some("Not sure about this approach".into());
    let out = f.format_step_console(&step);
    assert!(out.contains("Uncertainty: Not sure about this approach"));
}

#[test]
fn console_shows_tools_used() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.tools_used = Some(vec!["Read".into(), "Grep".into(), "Edit".into()]);
    let out = f.format_step_console(&step);
    assert!(out.contains("Tools Used: Read, Grep, Edit"));
}

#[test]
fn console_shows_dependencies() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.dependencies = Some(vec![1u32.into(), 2u32.into(), 3u32.into()]);
    let out = f.format_step_console(&step);
    assert!(out.contains("Depends On: Steps 1, 2, 3"));
}

#[test]
fn console_formats_structured_action() {
    let f = Formatter::new(false);
    let mut step = base_step();
    let mut params = BTreeMap::new();
    params.insert("path".into(), json!("/config.ts"));
    step.next_action = NextAction::Structured(StructuredAction {
        tool: Some("Read".into()),
        action: "Read the config file".into(),
        parameters: Some(params),
        expected_output: None,
    });
    let out = f.format_step_console(&step);
    assert!(out.contains("[Read] Read the config file"));
    assert!(out.contains("{\"path\":\"/config.ts\"}"));
}

#[test]
fn console_handles_all_purpose_types() {
    let f = Formatter::new(false);
    for purpose in [
        "analysis",
        "action",
        "reflection",
        "decision",
        "summary",
        "validation",
        "exploration",
        "hypothesis",
        "correction",
        "planning",
    ] {
        let mut step = base_step();
        step.purpose = purpose.into();
        let out = f.format_step_console(&step);
        assert!(
            out.contains(&format!("[Step 1/3] {}", purpose.to_uppercase())),
            "missing uppercase {purpose} in: {out}"
        );
    }
}

#[test]
fn console_handles_custom_purpose() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.purpose = "custom-purpose".into();
    let out = f.format_step_console(&step);
    assert!(out.contains("[Step 1/3] CUSTOM-PURPOSE"));
}

#[test]
fn markdown_basic() {
    let f = Formatter::new(false);
    let out = f.format_step_markdown(&base_step());
    assert!(out.contains("### Step 1/3: ANALYSIS"));
    assert!(out.contains("**Context:** Test context"));
    assert!(out.contains("**Thought:** Test thought"));
    assert!(out.contains("**Outcome:** Test outcome"));
    assert!(out.contains("**Next Action:** Test next action"));
    assert!(out.contains("*Rationale:* Test rationale"));
    assert!(out.contains("---"));
}

#[test]
fn markdown_confidence_badge() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.confidence = Some(0.75);
    let out = f.format_step_markdown(&step);
    assert!(out.contains("![Confidence]"));
    assert!(out.contains("75%"));
}

#[test]
fn markdown_revises_badge() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.revises_step = Some(2);
    let out = f.format_step_markdown(&step);
    assert!(out.contains("![Revises]"));
    assert!(out.contains("step%202"));
}

#[test]
fn markdown_branch_badge() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.branch_from = Some(3);
    let out = f.format_step_markdown(&step);
    assert!(out.contains("![Branch]"));
    assert!(out.contains("from%203"));
}

#[test]
fn markdown_uncertainty_blockquote() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.uncertainty_notes = Some("High uncertainty here".into());
    let out = f.format_step_markdown(&step);
    assert!(out.contains("> ⚠️ **Uncertainty:** High uncertainty here"));
}

#[test]
fn markdown_revision_reason_blockquote() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.revises_step = Some(1);
    step.revision_reason = Some("Previous analysis was incorrect".into());
    let out = f.format_step_markdown(&step);
    assert!(out.contains("> 🔄 **Revision Reason:** Previous analysis was incorrect"));
}

#[test]
fn markdown_tools_used() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.tools_used = Some(vec!["Bash".into(), "Read".into()]);
    let out = f.format_step_markdown(&step);
    assert!(out.contains("**Tools Used:** Bash, Read"));
}

#[test]
fn json_is_valid() {
    let f = Formatter::new(false);
    let out = f.format_step_json(&base_step());
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["step_number"], 1);
    assert_eq!(parsed["estimated_total"], 3);
    assert_eq!(parsed["purpose"], "analysis");
}

#[test]
fn json_includes_optional_fields() {
    let f = Formatter::new(false);
    let mut step = base_step();
    step.confidence = Some(0.9);
    step.revises_step = Some(1);
    step.branch_from = Some(2);
    step.tools_used = Some(vec!["Test".into()]);
    let out = f.format_step_json(&step);
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["confidence"], 0.9);
    assert_eq!(parsed["revises_step"], 1);
    assert_eq!(parsed["branch_from"], 2);
    assert_eq!(parsed["tools_used"], json!(["Test"]));
}

#[test]
fn history_summary_basic() {
    let f = Formatter::new(false);
    let history = DeliberateHistory {
        steps: vec![base_step()],
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_history_summary(&history);
    assert!(out.contains("Deliberation Summary"));
    assert!(out.contains("Total Steps: 1"));
    assert!(out.contains("In Progress"));
}

#[test]
fn history_summary_completed() {
    let f = Formatter::new(false);
    let mut history = DeliberateHistory {
        steps: vec![base_step()],
        branches: None,
        completed: true,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_history_summary(&history);
    history.completed = true;
    assert!(out.contains("✓ Completed"));
}

#[test]
fn history_summary_metadata() {
    let f = Formatter::new(false);
    let history = DeliberateHistory {
        steps: vec![base_step()],
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: Some(HistoryMetadata {
            revisions_count: Some(2),
            branches_created: Some(1),
            total_duration_ms: Some(5000),
            tools_used: Some(vec!["Read".into(), "Edit".into()]),
            project_id: None,
        }),
    };
    let out = f.format_history_summary(&history);
    assert!(out.contains("Revisions: 2"));
    assert!(out.contains("Branches Created: 1"));
    assert!(out.contains("Duration: 5.00s"));
    assert!(out.contains("Tools Used: Read, Edit"));
}

#[test]
fn history_summary_average_confidence() {
    let f = Formatter::new(false);
    let mut s1 = base_step();
    s1.confidence = Some(0.8);
    let mut s2 = base_step();
    s2.step_number = 2;
    s2.confidence = Some(0.6);
    let history = DeliberateHistory {
        steps: vec![s1, s2],
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_history_summary(&history);
    assert!(out.contains("Average Confidence: 70%"));
}

#[test]
fn history_summary_branches_listed() {
    let f = Formatter::new(false);
    let mut child = base_step();
    child.step_number = 2;
    let history = DeliberateHistory {
        steps: vec![base_step()],
        branches: Some(vec![Branch {
            id: "branch-1".into(),
            name: "Alternative A".into(),
            from_step: 1,
            steps: vec![child],
            status: BranchStatus::Active,
            created_at: "2025-01-01T00:00:00Z".into(),
            depth: 1,
            merged_into: None,
        }]),
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_history_summary(&history);
    assert!(out.contains("Branches:"));
    assert!(out.contains("● Alternative A (1 steps)"));
}

#[test]
fn history_summary_branch_status_symbols() {
    let f = Formatter::new(false);
    let history = DeliberateHistory {
        steps: vec![],
        branches: Some(vec![
            Branch {
                id: "a".into(),
                name: "Active Branch".into(),
                from_step: 1,
                steps: vec![],
                status: BranchStatus::Active,
                created_at: "".into(),
                depth: 1,
                merged_into: None,
            },
            Branch {
                id: "m".into(),
                name: "Merged Branch".into(),
                from_step: 1,
                steps: vec![],
                status: BranchStatus::Merged,
                created_at: "".into(),
                depth: 1,
                merged_into: None,
            },
            Branch {
                id: "ab".into(),
                name: "Abandoned Branch".into(),
                from_step: 1,
                steps: vec![],
                status: BranchStatus::Abandoned,
                created_at: "".into(),
                depth: 1,
                merged_into: None,
            },
        ]),
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_history_summary(&history);
    assert!(out.contains("● Active Branch"));
    assert!(out.contains("✓ Merged Branch"));
    assert!(out.contains("✗ Abandoned Branch"));
}

#[test]
fn branch_tree_shows_main_steps() {
    let f = Formatter::new(false);
    let mut s1 = base_step();
    s1.purpose = "analysis".into();
    let mut s2 = base_step();
    s2.step_number = 2;
    s2.purpose = "action".into();
    let history = DeliberateHistory {
        steps: vec![s1, s2],
        branches: None,
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_branch_tree(&history);
    assert!(out.contains("Branch Structure:"));
    assert!(out.contains("Main:"));
    assert!(out.contains("Step 1: analysis"));
    assert!(out.contains("Step 2: action"));
}

#[test]
fn branch_tree_shows_branches() {
    let f = Formatter::new(false);
    let mut branch_step = base_step();
    branch_step.step_number = 2;
    branch_step.purpose = "exploration".into();
    branch_step.branch_id = Some("branch-1".into());
    let history = DeliberateHistory {
        steps: vec![base_step(), branch_step.clone()],
        branches: Some(vec![Branch {
            id: "branch-1".into(),
            name: "Alt approach".into(),
            from_step: 1,
            steps: vec![branch_step],
            status: BranchStatus::Active,
            created_at: "".into(),
            depth: 1,
            merged_into: None,
        }]),
        completed: false,
        session_id: None,
        created_at: None,
        updated_at: None,
        metadata: None,
    };
    let out = f.format_branch_tree(&history);
    assert!(out.contains("Branch: Alt approach"));
}

#[test]
fn color_disabled_emits_no_ansi() {
    let f = Formatter::new(false);
    let out = f.format_step_console(&base_step());
    assert!(!out.contains('\x1b'), "expected no ANSI codes, got: {out}");
    assert!(out.contains("[Step 1/3] ANALYSIS"));
}

#[test]
fn color_enabled_still_carries_content() {
    let f_color = Formatter::new(true);
    let f_no_color = Formatter::new(false);
    let s = base_step();
    let c = f_color.format_step_console(&s);
    let n = f_no_color.format_step_console(&s);
    assert!(c.contains("[Step 1/3]"));
    assert!(n.contains("[Step 1/3]"));
    assert!(!n.contains('\x1b'));
}
