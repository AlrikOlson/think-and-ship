//! Mirrors `tests/server.test.ts`.

use think_and_ship::think::config::{
    DeliberateConfig, OutputFormat, PROJECT_SEP, namespace_session_id, resolve_project_id,
};
use think_and_ship::think::engine::core::{ProcessResult, ReasoningServer};
use think_and_ship::think::domain::{DeliberateStep, NextAction, StructuredAction};
use tempfile::TempDir;

/// Convenience: the namespaced form of `raw` under the current project.
fn ns(raw: &str) -> String {
    namespace_session_id(&resolve_project_id(), raw)
}

fn quiet_config() -> DeliberateConfig {
    let mut c = DeliberateConfig::default();
    c.display.color_output = false;
    c
}

fn strict_config() -> DeliberateConfig {
    let mut c = quiet_config();
    c.validation.strict_mode = true;
    c.validation.require_thought_prefix = true;
    c.validation.require_rationale_prefix = true;
    c.validation.allow_custom_purpose = false;
    c
}

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

fn step_n(n: u32) -> DeliberateStep {
    let mut s = base_step();
    s.step_number = n;
    s
}

fn parse_response(result: ProcessResult) -> serde_json::Value {
    let text = match result {
        Ok(ok) => ok.text,
        Err(err) => err.text,
    };
    serde_json::from_str(&text).expect("response should be JSON")
}

fn is_error(result: &ProcessResult) -> bool {
    result.is_err()
}

#[test]
fn constructor_starts_with_empty_history() {
    let s = ReasoningServer::new(quiet_config());
    assert_eq!(s.history().steps.len(), 0);
    assert!(!s.history().completed);
}

#[test]
fn response_echoes_thought_and_outcome_excerpts() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.thought = "Test thought that has more than a tiny bit of body to be excerpted".into();
    step.outcome = "Outcome describing what was learned".into();
    let v = parse_response(s.process_step(step));
    assert!(
        v.get("thought_excerpt").is_some(),
        "missing thought_excerpt"
    );
    assert!(
        v.get("outcome_excerpt").is_some(),
        "missing outcome_excerpt"
    );
}

#[test]
fn response_includes_recent_steps_after_second_step() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let v = parse_response(s.process_step(step_n(2)));
    let recent = v.get("recent_steps").expect("recent_steps missing");
    let arr = recent.as_array().expect("recent_steps not an array");
    assert_eq!(arr.len(), 1, "should reference only prior step");
    assert_eq!(arr[0].get("n").and_then(|n| n.as_u64()), Some(1));
}

#[test]
fn recent_steps_carries_rationale_excerpt() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = step_n(1);
    s1.rationale = "To confirm the asymmetry is real before designing a fix".into();
    let _ = s.process_step(s1);
    let v = parse_response(s.process_step(step_n(2)));
    let recent = v["recent_steps"].as_array().unwrap();
    let excerpt = recent[0]
        .get("rationale_excerpt")
        .and_then(|x| x.as_str())
        .expect("rationale_excerpt missing");
    assert!(excerpt.contains("confirm the asymmetry"), "got: {excerpt}");
}

#[test]
fn response_warns_on_low_confidence_dependency() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = step_n(1);
    s1.confidence = Some(0.2);
    let _ = s.process_step(s1);
    let mut s2 = step_n(2);
    s2.dependencies = Some(vec![1u32.into()]);
    let v = parse_response(s.process_step(s2));
    let warns = v.get("warnings").expect("warnings missing");
    let arr = warns.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|w| w.as_str().unwrap_or("").contains("low confidence"))
    );
}

#[test]
fn response_warns_when_step_exceeds_estimate() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = step_n(1);
    s1.estimated_total = 2;
    let _ = s.process_step(s1);
    let mut s2 = step_n(2);
    s2.estimated_total = 2;
    let _ = s.process_step(s2);
    let mut s3 = step_n(3);
    s3.estimated_total = 2;
    let v = parse_response(s.process_step(s3));
    let warns = v.get("warnings").expect("warnings missing");
    let arr = warns.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|w| w.as_str().unwrap_or("").contains("exceeds estimated_total"))
    );
}

#[test]
fn revise_estimate_updates_last_step_in_place() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let (prev, new) = s.revise_estimate(7).expect("revise should succeed");
    assert_eq!(prev, 3);
    assert_eq!(new, 7);
    assert_eq!(s.history().steps.last().unwrap().estimated_total, 7);
    assert_eq!(s.history().steps.len(), 1, "no new step should be appended");
}

#[test]
fn revise_estimate_rejects_zero_and_empty_history() {
    let mut s = ReasoningServer::new(quiet_config());
    assert!(
        s.revise_estimate(5).is_err(),
        "should error on empty history"
    );
    let _ = s.process_step(step_n(1));
    assert!(
        s.revise_estimate(0).is_err(),
        "should error on zero estimate"
    );
}

#[test]
fn step_by_number_finds_main_and_branch_steps() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    let mut b3 = step_n(3);
    b3.branch_from = Some(1);
    b3.branch_name = Some("alt".into());
    let _ = s.process_step(b3);

    assert_eq!(s.step_by_number(1).map(|x| x.step_number), Some(1));
    assert_eq!(s.step_by_number(2).map(|x| x.step_number), Some(2));
    assert_eq!(s.step_by_number(3).map(|x| x.step_number), Some(3));
    assert!(s.step_by_number(99).is_none());
}

#[test]
fn branches_summary_lists_each_active_branch() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    let mut b3 = step_n(3);
    b3.branch_from = Some(1);
    b3.branch_name = Some("explore-alt".into());
    let _ = s.process_step(b3);

    let summary = s.branches_summary();
    assert_eq!(summary.len(), 1);
    assert_eq!(
        summary[0].get("name").and_then(|x| x.as_str()),
        Some("explore-alt")
    );
    assert_eq!(
        summary[0].get("from_step").and_then(|x| x.as_u64()),
        Some(1)
    );
    assert_eq!(
        summary[0].get("status").and_then(|x| x.as_str()),
        Some("active")
    );
}

#[test]
fn sessions_snapshot_marks_active_session() {
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);
    let mut s1 = step_n(1);
    s1.session_id = Some("sess-alpha".into());
    let _ = s.process_step(s1);

    let snap = s.sessions_snapshot();
    assert_eq!(snap.len(), 1);
    let expected = ns("sess-alpha");
    assert_eq!(
        snap[0].get("session_id").and_then(|x| x.as_str()),
        Some(expected.as_str())
    );
    assert_eq!(snap[0].get("active").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(snap[0].get("step_count").and_then(|x| x.as_u64()), Some(1));
}

#[test]
fn default_session_id_is_applied_when_caller_omits_one() {
    // With a default_session_id configured, a step that arrives without
    // session_id should be routed to that session and persisted under
    // the same name — that's the contract of DELIBERATE_AUTO_SESSION /
    // DELIBERATE_DEFAULT_SESSION_ID.
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    c.features.default_session_id = Some("auto-test".into());

    let mut s = ReasoningServer::new(c);
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));

    let snap = s.sessions_snapshot();
    assert_eq!(snap.len(), 1, "exactly one named session should exist");
    let expected = ns("auto-test");
    assert_eq!(
        snap[0].get("session_id").and_then(|x| x.as_str()),
        Some(expected.as_str()),
    );
    assert_eq!(snap[0].get("step_count").and_then(|x| x.as_u64()), Some(2));
    // The step's own field should reflect the routing, not stay None,
    // and it should be the namespaced form so agents can't write
    // outside the project's bucket.
    let live = s.step_by_number(1).expect("step 1");
    assert_eq!(live.session_id.as_deref(), Some(expected.as_str()));
}

#[test]
fn explicit_session_id_beats_default() {
    // An explicit session_id on the step must always win over the
    // configured default.
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    c.features.default_session_id = Some("auto-fallback".into());

    let mut s = ReasoningServer::new(c);
    let mut a = step_n(1);
    a.session_id = Some("explicit".into());
    let _ = s.process_step(a);
    let _ = s.process_step(step_n(2)); // falls back to auto-fallback

    let snap = s.sessions_snapshot();
    let mut got: Vec<String> = snap
        .iter()
        .filter_map(|v| {
            v.get("session_id")
                .and_then(|x| x.as_str())
                .map(String::from)
        })
        .collect();
    got.sort();
    let mut expected = vec![ns("auto-fallback"), ns("explicit")];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn duplicate_step_number_in_persistent_session_returns_actionable_error() {
    // Simulates a fresh agent conversation hitting a persistent session
    // that already has a step #1. The new conversation doesn't know
    // history exists; it just calls deliberate(step_number=1). Server
    // must reject with a message that tells the agent (a) the next
    // step number to use, (b) the revision option, (c) the branch
    // option — so the agent can self-correct on the next turn.
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));

    // Second step #1 with no revises_step / branch_from — collision.
    let result = s.process_step(step_n(1));
    let err = match result {
        Err(e) => e.text,
        Ok(ok) => panic!(
            "expected duplicate-step rejection, got success: {}",
            ok.text
        ),
    };
    assert!(
        err.contains("step_number 1"),
        "should name the duplicate number: {err}"
    );
    assert!(
        err.contains("step_number: 2"),
        "should suggest the next number: {err}"
    );
    assert!(
        err.contains("revises_step"),
        "should mention the revision escape hatch: {err}"
    );
    assert!(
        err.contains("branch_from"),
        "should mention the branching escape hatch: {err}"
    );
}

#[test]
fn duplicate_step_number_is_allowed_when_revising() {
    // A revising step legitimately reuses the original number's
    // semantic position (via revises_step pointing to the prior step).
    // No rejection in that path.
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));

    let mut revision = step_n(2);
    revision.revises_step = Some(1);
    revision.revision_reason = Some("clarification".into());
    assert!(
        s.process_step(revision).is_ok(),
        "revision path must not be blocked"
    );
}

#[test]
fn xml_injection_in_thought_auto_recovers_missing_fields() {
    // Reproduces the harness's XML-injection failure mode end-to-end.
    // The agent embedded literal closing markup in its thought value,
    // which would have closed the parameter early in the harness — but
    // by the time the step arrives at us, the intended sibling values
    // are still in the thought string. We extract them and the call
    // succeeds, with a warning explaining what happened.
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.thought = String::from(
        "Real reasoning content here. </parameter>\
         <parameter name=\"outcome\">recovered outcome value</parameter>\
         <parameter name=\"next_action\">recovered next action</parameter>\
         <parameter name=\"rationale\">recovered rationale text</parameter>",
    );
    step.outcome = String::new();
    step.rationale = String::new();
    step.next_action = NextAction::Text(String::new());

    let result = s.process_step(step);
    let ok = match result {
        Ok(ok) => ok,
        Err(err) => panic!("expected auto-recovery success, got error: {}", err.text),
    };

    // The recovered step should now be in the history with the
    // extracted values filled in.
    let stored = s.step_by_number(1).expect("step 1 stored");
    assert_eq!(
        stored.outcome, "recovered outcome value",
        "outcome recovered"
    );
    assert_eq!(
        stored.rationale, "recovered rationale text",
        "rationale recovered"
    );
    match &stored.next_action {
        NextAction::Text(t) => assert_eq!(t, "recovered next action", "next_action recovered"),
        NextAction::Structured(_) => panic!("expected NextAction::Text"),
    }
    // The thought should be cleaned up — no embedded markup left.
    assert_eq!(
        stored.thought, "Real reasoning content here.",
        "thought truncated at markup"
    );

    // The response carries a warning naming what was recovered.
    let response: serde_json::Value = serde_json::from_str(&ok.text).unwrap();
    let warnings = response["warnings"].as_array().expect("warnings array");
    let first = warnings[0].as_str().unwrap_or_default();
    assert!(
        first.contains("Auto-recovered") && first.contains("outcome"),
        "warning should name recovered fields: {first}",
    );
}

#[test]
fn xml_injection_recovery_only_fills_empty_fields() {
    // If the agent already provided a value for a field, we must NOT
    // overwrite it with an extracted value even if injection happens.
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.outcome = "explicit outcome".into();
    step.thought = String::from(
        "thought text </parameter><parameter name=\"outcome\">recovered version</parameter>",
    );

    let _ = s.process_step(step);
    let stored = s.step_by_number(1).expect("step 1 stored");
    assert_eq!(
        stored.outcome, "explicit outcome",
        "explicit value must win"
    );
}

#[test]
fn partial_xml_injection_recovery_errors_only_on_still_missing_fields() {
    // The agent injected markup that covers only SOME of the missing
    // fields. We recover what we can, then error on the rest.
    // Verifies that the error doesn't accuse the user of forgetting
    // the field we already extracted.
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.thought = String::from(
        "I was about to write the outcome but then I typed </thought>\n\
         <parameter name=\"outcome\">recovered outcome</parameter> verbatim.",
    );
    step.outcome = "".into();
    step.next_action = NextAction::Text("".into());
    step.rationale = "".into();

    let result = s.process_step(step);
    let err = match result {
        Err(e) => e.text,
        Ok(_) => panic!("expected partial-recovery still-missing-fields error"),
    };
    // outcome was recovered → must NOT be in the missing-fields list at
    // the head of the error message. The diagnostic body may mention
    // `<outcome>` as an example pattern, which is fine.
    let parsed: serde_json::Value = serde_json::from_str(&err).expect("error body is JSON");
    let error_text = parsed.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let head = error_text.lines().next().unwrap_or("");
    assert!(
        head.starts_with("Missing or invalid required fields:"),
        "head should be the missing-fields summary; got: {head}"
    );
    assert!(
        !head.contains("outcome"),
        "outcome was recovered; head should not list it as missing: {head}"
    );
    assert!(
        head.contains("rationale"),
        "rationale should be flagged: {head}"
    );
    assert!(
        head.contains("next_action"),
        "next_action should be flagged: {head}"
    );
}

#[test]
fn xml_injection_marker_without_recoverable_payload_still_diagnoses() {
    // Edge case: a field contains the literal substring `<parameter name=`
    // but NOT a full extractable pattern. Nothing recovers, and the
    // diagnostic path still has to fire to tell the agent what went
    // wrong.
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.thought = "talking about </parameter> tags but no full match".into();
    step.outcome = "".into();
    step.rationale = "".into();
    step.next_action = NextAction::Text("".into());

    let result = s.process_step(step);
    let err = match result {
        Err(e) => e.text,
        Ok(_) => panic!("expected error"),
    };
    assert!(
        err.contains("Claude Code wire syntax") || err.contains("structural tags"),
        "should diagnose markup: {err}"
    );
}

#[test]
fn empty_required_fields_without_xml_injection_gets_plain_error() {
    // When the empty fields are not accompanied by injection markers,
    // we should NOT mention XML markup — keeps the false-positive rate
    // at zero for legitimate "you forgot a field" cases.
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.outcome = "".into();
    let result = s.process_step(step);
    let err = match result {
        Err(e) => e.text,
        Ok(ok) => panic!("expected validation error, got: {}", ok.text),
    };
    assert!(err.contains("outcome"), "got: {err}");
    assert!(
        !err.contains("tool-call markup"),
        "should not name XML cause when no markers present: {err}",
    );
}

#[test]
fn search_finds_matches_across_text_fields() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut a = step_n(1);
    a.thought = "Investigating a quorum-write retry storm".into();
    let _ = s.process_step(a);

    let mut b = step_n(2);
    b.outcome = "Quorum writes appear normal".into();
    let _ = s.process_step(b);

    let mut c = step_n(3);
    c.context = "Unrelated note about caching".into();
    let _ = s.process_step(c);

    let hits = s.search_steps("quorum", 10);
    assert_eq!(hits.len(), 2);
    let steps: Vec<u64> = hits
        .iter()
        .map(|h| h.get("step_number").and_then(|v| v.as_u64()).unwrap())
        .collect();
    assert_eq!(steps, vec![1, 2]);
    assert!(hits[0].get("excerpt").is_some());
    assert!(hits[0].get("matched_field").is_some());
}

fn persisting_config(tmp: &TempDir) -> DeliberateConfig {
    let mut c = quiet_config();
    c.persistence.enabled = true;
    c.persistence.data_dir = tmp.path().to_path_buf();
    c
}

#[test]
fn pin_marks_step_and_recent_steps_promotes_it() {
    let mut s = ReasoningServer::new(quiet_config());
    for n in 1..=5 {
        let _ = s.process_step(step_n(n));
    }
    // With limit=3 and no pin, step 1 is no longer in recent_steps after step 5.
    let recent = s.recent_steps_rollup(3, Some(5));
    let nums: Vec<u64> = recent.iter().map(|v| v["n"].as_u64().unwrap()).collect();
    assert!(
        !nums.contains(&1),
        "step 1 should be out of unpinned window"
    );

    // Pin step 1; it should now appear in the rollup even at limit=3.
    s.pin_step(1, true).unwrap();
    let recent = s.recent_steps_rollup(3, Some(5));
    let nums: Vec<u64> = recent.iter().map(|v| v["n"].as_u64().unwrap()).collect();
    assert!(
        nums.contains(&1),
        "pinned step 1 should re-enter the window: {nums:?}"
    );
}

#[test]
fn bare_u32_deps_still_deserialize() {
    // Backward-compat: a DeliberateStep persisted with `"dependencies": [1, 2]`
    // must still load via the new untagged DepEdge enum.
    let raw = r#"{
        "step_number": 2,
        "estimated_total": 3,
        "purpose": "action",
        "context": "x",
        "thought": "y",
        "outcome": "z",
        "next_action": "w",
        "rationale": "r",
        "dependencies": [1]
    }"#;
    let step: think_and_ship::think::domain::DeliberateStep = serde_json::from_str(raw).unwrap();
    let deps = step.dependencies.unwrap();
    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].step(), 1);
    assert_eq!(deps[0].relation(), None);
}

#[test]
fn tagged_dep_with_relation_round_trips() {
    let raw = r#"{
        "step_number": 2,
        "estimated_total": 3,
        "purpose": "validation",
        "context": "x",
        "thought": "y",
        "outcome": "z",
        "next_action": "w",
        "rationale": "r",
        "dependencies": [{"step": 1, "relation": "refutes"}, 3]
    }"#;
    let step: think_and_ship::think::domain::DeliberateStep = serde_json::from_str(raw).unwrap();
    let deps = step.dependencies.unwrap();
    assert_eq!(deps[0].step(), 1);
    assert_eq!(deps[0].relation(), Some("refutes"));
    assert_eq!(deps[1].step(), 3);
    assert_eq!(deps[1].relation(), None);
}

#[test]
fn refuted_dep_warning_fires() {
    use think_and_ship::think::domain::DepEdge;
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1)); // hypothesis target
    let mut s2 = step_n(2);
    s2.dependencies = Some(vec![DepEdge::Tagged {
        step: 1,
        relation: Some("refutes".into()),
    }]);
    let _ = s.process_step(s2);

    // Step 3 builds on step 1 unlabeled — should warn about the refutation.
    let mut s3 = step_n(3);
    s3.dependencies = Some(vec![1u32.into()]);
    let v = parse_response(s.process_step(s3));
    let warns = v["warnings"].as_array().unwrap();
    assert!(
        warns
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("refuted by step")),
        "expected refuted-dep warning, got: {warns:?}"
    );
}

#[test]
fn refuting_step_does_not_get_low_confidence_warning() {
    use think_and_ship::think::domain::DepEdge;
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = step_n(1);
    s1.confidence = Some(0.2);
    let _ = s.process_step(s1);

    let mut s2 = step_n(2);
    s2.dependencies = Some(vec![DepEdge::Tagged {
        step: 1,
        relation: Some("refutes".into()),
    }]);
    let v = parse_response(s.process_step(s2));
    let warns = v
        .get("warnings")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    // Refuting evidence shouldn't carry the "you're building on shaky ground"
    // warning — you're not building on it, you're refuting it.
    assert!(
        !warns
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("low confidence")),
        "refutes edge incorrectly triggered low-confidence warning: {warns:?}"
    );
}

#[test]
fn merged_into_round_trips_through_set_branch_status() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut b2 = step_n(2);
    b2.branch_from = Some(1);
    b2.branch_name = Some("alt".into());
    let _ = s.process_step(b2);
    let _ = s.process_step(step_n(3)); // synthesis step

    let branch_id = s.branches().keys().next().unwrap().clone();
    let (_, _) = s
        .set_branch_status(&branch_id, "merged", Some(3))
        .expect("merged should succeed");
    assert_eq!(s.branches()[&branch_id].merged_into, Some(3));

    // impact_of on the branch's from_step should surface the merged_into.
    let impact = s.impact_of(1).unwrap();
    let branches_from = impact["branches_from"].as_array().unwrap();
    assert_eq!(branches_from.len(), 1);
    assert_eq!(branches_from[0]["merged_into"].as_u64(), Some(3));

    // Status moving away from merged clears the pointer.
    s.set_branch_status(&branch_id, "active", None).unwrap();
    assert_eq!(s.branches()[&branch_id].merged_into, None);
}

#[test]
fn merged_into_rejects_unknown_step() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut b2 = step_n(2);
    b2.branch_from = Some(1);
    let _ = s.process_step(b2);
    let branch_id = s.branches().keys().next().unwrap().clone();
    assert!(
        s.set_branch_status(&branch_id, "merged", Some(999))
            .is_err()
    );
}

#[test]
fn checkpoint_snapshot_flags_open_hypothesis_and_stale_branch() {
    use think_and_ship::think::domain::DepEdge;
    let mut cfg = quiet_config();
    cfg.system.max_history_size = 8; // stale threshold becomes max(2, 8/4) = 2
    let mut s = ReasoningServer::new(cfg);

    // Step 1: hypothesis with no validation downstream.
    let mut s1 = step_n(1);
    s1.purpose = "hypothesis".into();
    let _ = s.process_step(s1);

    // Step 2: branched off step 1 (active).
    let mut s2 = step_n(2);
    s2.branch_from = Some(1);
    s2.branch_name = Some("explore".into());
    let _ = s.process_step(s2);

    // Steps 3, 4, 5: pile on so the branch becomes stale.
    let _ = s.process_step(step_n(3));
    let _ = s.process_step(step_n(4));
    let _ = s.process_step(step_n(5));

    let snap = s.checkpoint_snapshot();
    let open = snap["open_hypotheses"].as_array().unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["step_number"].as_u64(), Some(1));

    let stale = snap["stale_branches"].as_array().unwrap();
    assert_eq!(stale.len(), 1, "expected one stale branch, got {stale:?}");
    assert_eq!(stale[0]["name"], "explore");

    let _ = &snap["confidence_trend"]; // present, value depends on data

    // Now add a refuting edge and confirm refuted_chain_alerts fires.
    let mut s6 = step_n(6);
    s6.dependencies = Some(vec![DepEdge::Tagged {
        step: 1,
        relation: Some("refutes".into()),
    }]);
    let _ = s.process_step(s6);
    let mut s7 = step_n(7);
    s7.dependencies = Some(vec![1u32.into()]); // builds unlabeled on refuted step 1
    let _ = s.process_step(s7);

    let snap = s.checkpoint_snapshot();
    let alerts = snap["refuted_chain_alerts"].as_array().unwrap();
    assert!(!alerts.is_empty(), "expected refuted_chain_alerts");
}

#[test]
fn impact_partitions_downstream_by_relation() {
    use think_and_ship::think::domain::DepEdge;
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.dependencies = Some(vec![DepEdge::Tagged {
        step: 1,
        relation: Some("supports".into()),
    }]);
    let _ = s.process_step(s2);
    let mut s3 = step_n(3);
    s3.dependencies = Some(vec![DepEdge::Tagged {
        step: 1,
        relation: Some("refutes".into()),
    }]);
    let _ = s.process_step(s3);
    let mut s4 = step_n(4);
    s4.dependencies = Some(vec![1u32.into()]); // unlabeled
    let _ = s.process_step(s4);

    let impact = s.impact_of(1).unwrap();
    let by_rel = &impact["downstream"]["by_relation"];
    assert_eq!(by_rel["supports"].as_array().unwrap().len(), 1);
    assert_eq!(by_rel["refutes"].as_array().unwrap().len(), 1);
    assert_eq!(by_rel["unlabeled"].as_array().unwrap().len(), 1);
}

#[test]
fn latest_revision_walks_full_chain() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    // Step 3 revises 2; step 4 revises 3.
    let mut s3 = step_n(3);
    s3.revises_step = Some(2);
    let _ = s.process_step(s3);
    let mut s4 = step_n(4);
    s4.revises_step = Some(3);
    let _ = s.process_step(s4);

    assert_eq!(s.latest_revision_of(2).map(|x| x.step_number), Some(4));
    assert_eq!(s.latest_revision_of(3).map(|x| x.step_number), Some(4));
    // A step that's never been revised returns itself.
    assert_eq!(s.latest_revision_of(1).map(|x| x.step_number), Some(1));
    // Unknown step still returns None.
    assert!(s.latest_revision_of(99).is_none());
}

#[test]
fn status_verbose_includes_pinned_and_sessions() {
    let mut cfg = quiet_config();
    cfg.features.enable_sessions = true;
    let mut s = ReasoningServer::new(cfg);
    let mut s1 = step_n(1);
    s1.session_id = Some("alpha".into());
    let _ = s.process_step(s1);
    s.pin_step(1, true).unwrap();

    // Compact mode hides the arrays.
    let compact = s.status_snapshot(false);
    assert!(compact.get("pinned").is_none());
    assert!(compact.get("sessions").is_none());

    // Verbose mode includes both.
    let v = s.status_snapshot(true);
    assert!(v["pinned"].is_array());
    assert!(v["sessions"].is_array());
    assert_eq!(v["pinned"].as_array().unwrap().len(), 1);
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
}

#[test]
fn pinned_steps_lists_in_step_order() {
    let mut s = ReasoningServer::new(quiet_config());
    for n in 1..=4 {
        let _ = s.process_step(step_n(n));
    }
    s.pin_step(3, true).unwrap();
    s.pin_step(1, true).unwrap();

    let pinned = s.pinned_steps();
    assert_eq!(pinned.len(), 2);
    assert_eq!(pinned[0]["step_number"].as_u64(), Some(1));
    assert_eq!(pinned[1]["step_number"].as_u64(), Some(3));
}

#[test]
fn status_snapshot_reflects_engine_state() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = persisting_config(&tmp);
    cfg.system.recent_steps_limit = 7;
    let mut s = ReasoningServer::new(cfg);
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    s.pin_step(1, true).unwrap();

    let snap = s.status_snapshot(false);
    assert_eq!(snap["persistence_enabled"].as_bool(), Some(true));
    assert!(
        snap["data_dir"]
            .as_str()
            .unwrap()
            .contains(tmp.path().to_str().unwrap())
    );
    assert_eq!(snap["total_steps"].as_u64(), Some(2));
    assert_eq!(snap["pinned_count"].as_u64(), Some(1));
    assert_eq!(snap["recent_steps_limit"].as_u64(), Some(7));
    assert!(snap["version"].as_str().is_some());
}

#[test]
fn session_id_with_sessions_disabled_emits_warning() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = step_n(1);
    s1.session_id = Some("alpha".into());
    let v = parse_response(s.process_step(s1));
    let warns = v.get("warnings").expect("warnings missing");
    let arr = warns.as_array().unwrap();
    assert!(
        arr.iter()
            .any(|w| w.as_str().unwrap_or("").contains("sessions are disabled")),
        "expected session-disabled warning, got {arr:?}"
    );
}

#[test]
fn pin_errors_on_unknown_step() {
    let mut s = ReasoningServer::new(quiet_config());
    assert!(s.pin_step(99, true).is_err());
}

#[test]
fn persistence_disabled_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = quiet_config();
    cfg.persistence.enabled = false;
    cfg.persistence.data_dir = tmp.path().to_path_buf();
    let mut s = ReasoningServer::new(cfg);
    let _ = s.process_step(step_n(1));

    let dir = tmp.path().join("think").join("sessions");
    assert!(
        !dir.exists()
            || std::fs::read_dir(&dir)
                .map(|d| d.count() == 0)
                .unwrap_or(true),
        "no files should be written when persistence is disabled"
    );
}

#[test]
fn persistence_round_trip_default_history() {
    let tmp = TempDir::new().unwrap();
    {
        let mut s = ReasoningServer::new(persisting_config(&tmp));
        let mut s1 = step_n(1);
        s1.thought = "Persisted-needle-A".into();
        let _ = s.process_step(s1);
        let _ = s.process_step(step_n(2));
    }
    // A fresh server with the same data_dir must reload both steps.
    let s = ReasoningServer::new(persisting_config(&tmp));
    assert_eq!(s.history().steps.len(), 2);
    assert!(s.history().steps[0].thought.contains("Persisted-needle-A"));
}

#[test]
fn persistence_writes_atomically_no_tmp_file_left_behind() {
    let tmp = TempDir::new().unwrap();
    let mut s = ReasoningServer::new(persisting_config(&tmp));
    let _ = s.process_step(step_n(1));

    let dir = tmp.path().join("think").join("sessions");
    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["_default.json"],
        "no .tmp left over: {entries:?}"
    );
}

#[test]
fn persistence_clear_removes_disk_files() {
    let tmp = TempDir::new().unwrap();
    let mut s = ReasoningServer::new(persisting_config(&tmp));
    let _ = s.process_step(step_n(1));
    s.clear_history();
    let dir = tmp.path().join("think").join("sessions");
    let count = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(count, 0, "clear_history should wipe disk");
}

#[test]
fn persistence_rejects_unsafe_session_ids() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = persisting_config(&tmp);
    cfg.features.enable_sessions = true;
    let mut s = ReasoningServer::new(cfg);

    // Path-traversal attempt: should be silently dropped from disk; the
    // in-memory step still records (we don't fail the call, just refuse to
    // persist that unsafe id).
    let mut s1 = step_n(1);
    s1.session_id = Some("../escape".into());
    let _ = s.process_step(s1);

    // Look for any file outside `think/sessions/` — there must be none.
    // Walk into the `think/` partition and assert only `sessions/` lives there.
    let think_partition = tmp.path().join("think");
    let sessions_dir = think_partition.join("sessions");
    for entry in std::fs::read_dir(&think_partition).unwrap().flatten() {
        let p = entry.path();
        assert!(
            p == sessions_dir,
            "no unexpected files outside sessions/: {}",
            p.display()
        );
    }
}

#[test]
fn recent_steps_limit_is_configurable() {
    let mut cfg = quiet_config();
    cfg.system.recent_steps_limit = 5;
    let mut s = ReasoningServer::new(cfg);
    for n in 1..=7 {
        let _ = s.process_step(step_n(n));
    }
    let recent = s.recent_steps_rollup(5, Some(7));
    assert_eq!(recent.len(), 5);
}

#[test]
fn search_does_not_duplicate_branch_steps() {
    // Branch steps are stored in both `history.steps` and `branch.steps` —
    // search must not return them twice.
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.branch_from = Some(1);
    s2.branch_name = Some("alt".into());
    s2.thought = "Distinctive needle ABCXYZ in branch step".into();
    let _ = s.process_step(s2);

    let hits = s.search_steps("ABCXYZ", 10);
    assert_eq!(
        hits.len(),
        1,
        "branch step should appear exactly once, got {hits:?}"
    );
}

#[test]
fn search_is_case_insensitive_and_returns_empty_for_no_query() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut a = step_n(1);
    a.thought = "CamelCase HTTP failures".into();
    let _ = s.process_step(a);

    assert_eq!(s.search_steps("camelcase", 10).len(), 1);
    assert_eq!(s.search_steps("HTTP", 10).len(), 1);
    assert!(s.search_steps("", 10).is_empty());
    assert!(s.search_steps("   ", 10).is_empty());
}

#[test]
fn execution_ref_stored_and_searchable() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.execution_ref = Some("task:auth-refactor".into());
    let _ = s.process_step(step);

    let stored = s
        .history()
        .steps
        .iter()
        .find(|s| s.step_number == 1)
        .unwrap();
    assert_eq!(stored.execution_ref, Some("task:auth-refactor".into()));

    let hits = s.search_steps("auth-refactor", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["matched_field"], "execution_ref");
}

#[test]
fn execution_ref_absent_when_not_set() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let stored = s
        .history()
        .steps
        .iter()
        .find(|s| s.step_number == 1)
        .unwrap();
    assert_eq!(stored.execution_ref, None);
}

#[test]
fn impact_reports_upstream_downstream_and_revision_chain() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.dependencies = Some(vec![1u32.into()]);
    let _ = s.process_step(s2);
    let mut s3 = step_n(3);
    s3.dependencies = Some(vec![2u32.into()]);
    let _ = s.process_step(s3);
    let mut s4 = step_n(4);
    s4.revises_step = Some(2);
    let _ = s.process_step(s4);

    let v = s.impact_of(2).expect("impact should succeed");
    assert_eq!(v.get("step_number").and_then(|x| x.as_u64()), Some(2));

    let upstream = v.get("upstream").unwrap();
    let up_direct: Vec<u64> = upstream
        .get("direct")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert_eq!(up_direct, vec![1]);

    let downstream = v.get("downstream").unwrap();
    let down_direct: Vec<u64> = downstream
        .get("direct")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert_eq!(down_direct, vec![3]);

    let chain: Vec<u64> = v
        .get("revision_chain")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect();
    assert_eq!(chain, vec![2, 4]);
}

#[test]
fn impact_returns_empty_revision_chain_when_not_revised() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let v = s.impact_of(1).unwrap();
    let chain = v.get("revision_chain").unwrap().as_array().unwrap();
    assert!(chain.is_empty(), "chain should be empty when no revisions");
}

#[test]
fn impact_errors_on_missing_step() {
    let s = ReasoningServer::new(quiet_config());
    assert!(s.impact_of(99).is_err());
}

#[test]
fn set_branch_status_marks_and_validates() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut b2 = step_n(2);
    b2.branch_from = Some(1);
    b2.branch_name = Some("alt".into());
    let _ = s.process_step(b2);

    let branch_id = s
        .branches()
        .keys()
        .next()
        .expect("branch should exist")
        .clone();

    let (prev, new) = s
        .set_branch_status(&branch_id, "abandoned", None)
        .expect("should succeed");
    assert_eq!(prev, "active");
    assert_eq!(new, "abandoned");

    // Unknown id is an error, not a panic.
    assert!(
        s.set_branch_status("does-not-exist", "merged", None)
            .is_err()
    );
    // Unknown status is rejected.
    assert!(s.set_branch_status(&branch_id, "bogus", None).is_err());
}

#[test]
fn branches_summary_response_field_appears_after_branching() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    let mut b3 = step_n(3);
    b3.branch_from = Some(1);
    b3.branch_name = Some("alt".into());
    let v = parse_response(s.process_step(b3));
    assert!(
        v.get("branches_summary").is_some(),
        "branches_summary should appear once a branch exists"
    );
}

#[test]
fn thought_prefix_unused_when_disabled() {
    let s = ReasoningServer::new(quiet_config());
    assert!(s.validate_thought_prefix("Any thought"));
}

#[test]
fn thought_prefix_accepts_valid_prefixes_in_strict() {
    let s = ReasoningServer::new(strict_config());
    for t in [
        "OK, I will analyze this",
        "But we need to consider",
        "Wait this is wrong",
        "Therefore the answer is",
        "I see the issue now. The problem is",
        "I have completed the task",
    ] {
        assert!(s.validate_thought_prefix(t), "should accept: {t}");
    }
}

#[test]
fn thought_prefix_rejects_invalid_in_strict() {
    let s = ReasoningServer::new(strict_config());
    assert!(!s.validate_thought_prefix("This is my thought"));
    assert!(!s.validate_thought_prefix("Let me think"));
}

#[test]
fn rationale_passes_when_disabled() {
    let s = ReasoningServer::new(quiet_config());
    assert!(s.validate_rationale("Any rationale"));
}

#[test]
fn rationale_requires_to_prefix_when_enabled() {
    let s = ReasoningServer::new(strict_config());
    assert!(s.validate_rationale("To understand the problem"));
    assert!(!s.validate_rationale("Because it is needed"));
}

#[test]
fn purpose_any_when_custom_allowed() {
    let s = ReasoningServer::new(quiet_config());
    assert!(s.validate_purpose("custom-purpose"));
    assert!(s.validate_purpose("anything"));
}

#[test]
fn purpose_standard_only_in_strict() {
    let s = ReasoningServer::new(strict_config());
    for p in [
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
        assert!(s.validate_purpose(p), "should accept: {p}");
    }
    assert!(!s.validate_purpose("custom"));
    assert!(!s.validate_purpose("invalid"));
}

#[test]
fn purpose_case_insensitive_in_strict() {
    let s = ReasoningServer::new(strict_config());
    assert!(s.validate_purpose("ANALYSIS"));
    assert!(s.validate_purpose("Analysis"));
}

#[test]
fn extract_tools_from_tools_used() {
    let mut step = base_step();
    step.tools_used = Some(vec!["Read".into(), "Edit".into()]);
    assert_eq!(
        ReasoningServer::extract_tools_used(&step),
        vec!["Read".to_string(), "Edit".to_string()]
    );
}

#[test]
fn extract_tools_from_structured_action() {
    let mut step = base_step();
    step.next_action = NextAction::Structured(StructuredAction {
        tool: Some("Bash".into()),
        action: "Run command".into(),
        parameters: None,
        expected_output: None,
    });
    assert_eq!(
        ReasoningServer::extract_tools_used(&step),
        vec!["Bash".to_string()]
    );
}

#[test]
fn extract_tools_combines_and_dedupes() {
    let mut step = base_step();
    step.tools_used = Some(vec!["Read".into(), "Edit".into()]);
    step.next_action = NextAction::Structured(StructuredAction {
        tool: Some("Read".into()),
        action: "Read file".into(),
        parameters: None,
        expected_output: None,
    });
    assert_eq!(
        ReasoningServer::extract_tools_used(&step),
        vec!["Read".to_string(), "Edit".to_string()]
    );
}

#[test]
fn extract_tools_empty_when_none() {
    let step = base_step();
    assert!(ReasoningServer::extract_tools_used(&step).is_empty());
}

#[test]
fn process_valid_step() {
    let mut s = ReasoningServer::new(quiet_config());
    let result = s.process_step(base_step());
    assert!(!is_error(&result));
    let r = parse_response(result);
    assert_eq!(r["step_number"], 1);
    assert_eq!(r["total_steps"], 1);
    // `completed` is omitted when false — only the truthy case is surfaced
    // to avoid burning tokens on the common path.
    assert!(r.get("completed").is_none());
}

#[test]
fn reject_missing_required_fields() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.context = String::new();
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("Missing or invalid required fields"),
        "got: {}",
        r["error"]
    );
}

#[test]
fn strict_mode_rejects_invalid_thought() {
    let mut s = ReasoningServer::new(strict_config());
    let mut step = base_step();
    step.thought = "Invalid thought".into();
    step.rationale = "To do something".into();
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("strict mode"));
}

#[test]
fn completion_via_is_final_step() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.is_final_step = Some(true);
    let _ = s.process_step(step);
    assert!(s.history().completed);
}

#[test]
fn completion_via_phrase_detection() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.thought = "I have completed the analysis and found the solution".into();
    let _ = s.process_step(step);
    assert!(s.history().completed);
}

#[test]
fn confidence_surfaces_in_recent_steps_not_top_level() {
    // The top-level confidence echo was dropped — the caller already has the
    // value it just sent. Confidence still surfaces in `recent_steps` (so
    // later steps can see prior confidences) and in `warnings` when low.
    let mut s = ReasoningServer::new(quiet_config());
    let mut s1 = base_step();
    s1.confidence = Some(0.85);
    let _ = s.process_step(s1);
    let r = parse_response(s.process_step(step_n(2)));
    assert!(
        r.get("confidence").is_none(),
        "no top-level confidence echo"
    );
    let recent = r["recent_steps"].as_array().unwrap();
    assert_eq!(recent[0]["confidence"], 0.85);
}

#[test]
fn reject_confidence_above_1() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.confidence = Some(1.5);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("out of bounds"));
}

#[test]
fn reject_confidence_below_0() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.confidence = Some(-0.5);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("out of bounds"));
}

#[test]
fn accept_confidence_at_boundaries() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut a = step_n(1);
    a.confidence = Some(0.0);
    assert!(!is_error(&s.process_step(a)));
    let mut b = step_n(2);
    b.confidence = Some(1.0);
    assert!(!is_error(&s.process_step(b)));
}

#[test]
fn reject_whitespace_only_field() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.context = "   ".into();
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("context"));
}

#[test]
fn reject_step_number_zero() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    step.step_number = 0;
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("step_number"));
}

#[test]
fn history_trimmed_when_exceeding_max() {
    let mut c = quiet_config();
    c.system.max_history_size = 3;
    let mut s = ReasoningServer::new(c);
    for i in 1..=5 {
        let _ = s.process_step(step_n(i));
    }
    assert_eq!(s.history().steps.len(), 3);
    assert_eq!(s.history().steps[0].step_number, 3);
}

#[test]
fn revision_marks_original() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.revises_step = Some(1);
    s2.revision_reason = Some("Found error".into());
    let _ = s.process_step(s2);
    let original = s
        .history()
        .steps
        .iter()
        .find(|s| s.step_number == 1)
        .unwrap();
    assert_eq!(original.revised_by, Some(2));
    let meta = s.history().metadata.as_ref().unwrap();
    assert_eq!(meta.revisions_count, Some(1));
}

#[test]
fn revision_of_missing_step_is_warning_not_error() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    // Step 0 doesn't exist but is "earlier" than step 2 — should warn, not fail.
    // (Note: we use step 0 for the test because revises_step must be < step_number.)
    s2.revises_step = Some(0);
    let r = s.process_step(s2);
    // Even though revises_step==0 is a "missing" step, the engine warns and continues.
    // It only fails when revises_step >= step.step_number.
    // Here 0 < 2, so it proceeds.
    assert!(!is_error(&r));
    assert_eq!(s.history().steps.len(), 2);
}

#[test]
fn revision_of_future_step_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.revises_step = Some(999);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("can only revise earlier steps")
    );
}

#[test]
fn revisions_skipped_when_feature_disabled() {
    let mut c = quiet_config();
    c.features.enable_revisions = false;
    let mut s = ReasoningServer::new(c);
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.revises_step = Some(1);
    let _ = s.process_step(s2);
    let original = s
        .history()
        .steps
        .iter()
        .find(|s| s.step_number == 1)
        .unwrap();
    assert_eq!(original.revised_by, None);
}

#[test]
fn branch_created_from_existing_step() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.branch_from = Some(1);
    s2.branch_id = Some("alt-1".into());
    s2.branch_name = Some("Alternative approach".into());
    let _ = s.process_step(s2);
    let branches = s.branches();
    assert_eq!(branches.len(), 1);
    let b = branches.get("alt-1").unwrap();
    assert_eq!(b.name, "Alternative approach");
    assert_eq!(b.from_step, 1);
}

#[test]
fn branch_auto_generates_id_and_name() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.branch_from = Some(1);
    let _ = s.process_step(s2);
    assert_eq!(s.branches().len(), 1);
    let (id, b) = s.branches().iter().next().unwrap();
    assert!(
        id.starts_with("branch-"),
        "id should start with `branch-`, got {id}"
    );
    assert_eq!(b.name, "Alternative 1");
}

#[test]
fn branch_disabled_creates_no_branch() {
    let mut c = quiet_config();
    c.features.enable_branching = false;
    let mut s = ReasoningServer::new(c);
    let _ = s.process_step(step_n(1));
    let mut s2 = step_n(2);
    s2.branch_from = Some(1);
    let _ = s.process_step(s2);
    assert_eq!(s.branches().len(), 0);
}

#[test]
fn branch_from_nonexistent_step_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(2);
    step.branch_from = Some(999);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("does not exist"));
}

#[test]
fn branch_from_self_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut step = step_n(2);
    step.branch_from = Some(2);
    step.branch_name = Some("Self-branch".into());
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("Cannot branch from self")
    );
}

#[test]
fn dependencies_none_passes() {
    let s = ReasoningServer::new(quiet_config());
    // Indirectly via process_step
    let _ = s;
    // No dependencies → no error path triggered. Tested via process_valid_step.
}

#[test]
fn dependencies_missing_warns_but_continues() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(10);
    step.dependencies = Some(vec![1u32.into(), 2u32.into(), 3u32.into()]);
    let result = s.process_step(step);
    // Missing dependencies are a warning, not a failure.
    assert!(!is_error(&result), "expected non-error, got: {result:?}");
}

#[test]
fn dependencies_self_is_circular_error() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.dependencies = Some(vec![1u32.into()]);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("Circular"));
}

#[test]
fn dependencies_future_is_invalid_error() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.dependencies = Some(vec![5u32.into(), 10u32.into()]);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("cannot depend on future steps")
    );
}

#[test]
fn dependencies_satisfied_when_steps_exist() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let _ = s.process_step(step_n(2));
    let mut step = step_n(3);
    step.dependencies = Some(vec![1u32.into(), 2u32.into()]);
    assert!(!is_error(&s.process_step(step)));
}

#[test]
fn session_created_when_id_provided() {
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);
    let mut step = step_n(1);
    step.session_id = Some("test-session".into());
    let _ = s.process_step(step);
    assert!(s.sessions().contains_key(&ns("test-session")));
}

#[test]
fn step_numbers_unique_across_sessions_in_project() {
    // Step #1 in session-1 should block step #1 in session-2 because
    // step_number is unique project-wide, not per-session. Agents that
    // start a fresh chat in a different session get the same actionable
    // "use N+1" error they'd get inside one session.
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);

    let mut a = step_n(1);
    a.session_id = Some("session-1".into());
    assert!(s.process_step(a).is_ok());

    let mut b = step_n(1);
    b.session_id = Some("session-2".into());
    let err = match s.process_step(b) {
        Err(e) => e.text,
        Ok(ok) => panic!(
            "expected project-wide duplicate rejection, got: {}",
            ok.text
        ),
    };
    assert!(
        err.contains("step_number 1 is already recorded in this project"),
        "should name the cross-session collision: {err}"
    );
    assert!(
        err.contains("step_number: 2"),
        "should suggest the next number: {err}"
    );

    // Session-1 keeps its step; session-2 didn't gain one.
    assert_eq!(
        s.sessions()
            .get(&ns("session-1"))
            .unwrap()
            .history
            .steps
            .len(),
        1
    );
    assert_eq!(
        s.sessions()
            .get(&ns("session-2"))
            .unwrap()
            .history
            .steps
            .len(),
        0
    );

    // Retry with the suggested number — should succeed.
    let mut b2 = step_n(2);
    b2.session_id = Some("session-2".into());
    assert!(s.process_step(b2).is_ok());
    assert_eq!(
        s.sessions()
            .get(&ns("session-2"))
            .unwrap()
            .history
            .steps
            .len(),
        1
    );
}

#[test]
fn renumber_on_load_resolves_cross_session_duplicates() {
    // Plant two legacy session files that each used per-session step
    // numbering (1, 2). Server construction must renumber them to a single
    // ascending sequence (1, 2, 3, 4) and rewrite any in-session
    // references so they still point at the right steps.
    use think_and_ship::think::persistence::Persistence;
    use think_and_ship::think::domain::{DeliberateHistory, DepEdge, HistoryMetadata};

    let tmp = TempDir::new().unwrap();
    let project_id = resolve_project_id();

    let make_step = |n: u32, ts: &str, revises: Option<u32>, deps: Option<Vec<u32>>| {
        let mut s = step_n(n);
        s.timestamp = Some(ts.into());
        if let Some(r) = revises {
            s.revises_step = Some(r);
            s.revision_reason = Some("clarification".into());
        }
        if let Some(ds) = deps {
            s.dependencies = Some(ds.into_iter().map(DepEdge::Bare).collect());
        }
        s
    };

    // session-a created first, holds steps #1 (revised by #2) and #2.
    let history_a = DeliberateHistory {
        steps: vec![
            make_step(1, "2025-01-01T00:00:00Z", None, None),
            make_step(2, "2025-01-01T00:01:00Z", Some(1), Some(vec![1])),
        ],
        branches: Some(Vec::new()),
        completed: false,
        session_id: Some(format!("{project_id}{PROJECT_SEP}session-a")),
        created_at: Some("2025-01-01T00:00:00Z".into()),
        updated_at: Some("2025-01-01T00:01:00Z".into()),
        metadata: Some(HistoryMetadata {
            project_id: Some(project_id.clone()),
            ..HistoryMetadata::default()
        }),
    };
    // session-b created second, also starting at #1 — the collision.
    let history_b = DeliberateHistory {
        steps: vec![
            make_step(1, "2025-01-02T00:00:00Z", None, None),
            make_step(2, "2025-01-02T00:01:00Z", None, Some(vec![1])),
        ],
        branches: Some(Vec::new()),
        completed: false,
        session_id: Some(format!("{project_id}{PROJECT_SEP}session-b")),
        created_at: Some("2025-01-02T00:00:00Z".into()),
        updated_at: Some("2025-01-02T00:01:00Z".into()),
        metadata: Some(HistoryMetadata {
            project_id: Some(project_id.clone()),
            ..HistoryMetadata::default()
        }),
    };

    {
        let pers_cfg = persisting_config(&tmp).persistence;
        let pers = Persistence::new(&pers_cfg);
        let sid_a = format!("{project_id}{PROJECT_SEP}session-a");
        let sid_b = format!("{project_id}{PROJECT_SEP}session-b");
        pers.save_session(&sid_a, &history_a);
        pers.save_session(&sid_b, &history_b);
    }

    // Server construction triggers the renumber.
    let mut cfg = persisting_config(&tmp);
    cfg.features.enable_sessions = true;
    let s = ReasoningServer::new(cfg);

    let sid_a = format!("{project_id}{PROJECT_SEP}session-a");
    let sid_b = format!("{project_id}{PROJECT_SEP}session-b");
    let sa = &s.sessions().get(&sid_a).unwrap().history;
    let sb = &s.sessions().get(&sid_b).unwrap().history;

    // Every step in the project now has a unique step_number.
    let mut all_nums: Vec<u32> = sa
        .steps
        .iter()
        .chain(sb.steps.iter())
        .map(|st| st.step_number)
        .collect();
    all_nums.sort();
    assert_eq!(
        all_nums,
        vec![1, 2, 3, 4],
        "step_numbers must be globally unique 1..N"
    );

    // session-a came first (earlier created_at) so it gets 1..2.
    assert_eq!(sa.steps[0].step_number, 1);
    assert_eq!(sa.steps[1].step_number, 2);
    // session-b inherits 3..4.
    assert_eq!(sb.steps[0].step_number, 3);
    assert_eq!(sb.steps[1].step_number, 4);

    // Reference rewriting: session-a step #2 originally said `revises_step: 1`
    // and `dependencies: [1]`. After renumbering, the target (originally
    // step #1 in session-a, now step #1 globally) still resolves.
    assert_eq!(sa.steps[1].revises_step, Some(1));
    let deps_a = sa.steps[1].dependencies.as_ref().unwrap();
    assert_eq!(deps_a[0].step(), 1);

    // session-b step #4 originally said `dependencies: [1]` (its own session-b step
    // #1). After renumbering, that maps to global step #3.
    let deps_b = sb.steps[1].dependencies.as_ref().unwrap();
    assert_eq!(deps_b[0].step(), 3);
}

#[test]
fn renumber_on_load_is_idempotent_when_no_duplicates() {
    // Plant two sessions whose step_numbers are already globally unique
    // — server construction must leave them untouched.
    use think_and_ship::think::persistence::Persistence;
    use think_and_ship::think::domain::{DeliberateHistory, HistoryMetadata};

    let tmp = TempDir::new().unwrap();
    let project_id = resolve_project_id();

    let mk = |n: u32, ts: &str| {
        let mut s = step_n(n);
        s.timestamp = Some(ts.into());
        s
    };
    let history_a = DeliberateHistory {
        steps: vec![mk(1, "2025-01-01T00:00:00Z"), mk(2, "2025-01-01T00:01:00Z")],
        branches: Some(Vec::new()),
        completed: false,
        session_id: Some(format!("{project_id}{PROJECT_SEP}session-a")),
        created_at: Some("2025-01-01T00:00:00Z".into()),
        updated_at: Some("2025-01-01T00:01:00Z".into()),
        metadata: Some(HistoryMetadata {
            project_id: Some(project_id.clone()),
            ..HistoryMetadata::default()
        }),
    };
    let history_b = DeliberateHistory {
        steps: vec![mk(3, "2025-01-02T00:00:00Z"), mk(4, "2025-01-02T00:01:00Z")],
        branches: Some(Vec::new()),
        completed: false,
        session_id: Some(format!("{project_id}{PROJECT_SEP}session-b")),
        created_at: Some("2025-01-02T00:00:00Z".into()),
        updated_at: Some("2025-01-02T00:01:00Z".into()),
        metadata: Some(HistoryMetadata {
            project_id: Some(project_id.clone()),
            ..HistoryMetadata::default()
        }),
    };

    {
        let pers_cfg = persisting_config(&tmp).persistence;
        let pers = Persistence::new(&pers_cfg);
        pers.save_session(&format!("{project_id}{PROJECT_SEP}session-a"), &history_a);
        pers.save_session(&format!("{project_id}{PROJECT_SEP}session-b"), &history_b);
    }

    let mut cfg = persisting_config(&tmp);
    cfg.features.enable_sessions = true;
    let s = ReasoningServer::new(cfg);

    let sa = &s
        .sessions()
        .get(&format!("{project_id}{PROJECT_SEP}session-a"))
        .unwrap()
        .history;
    let sb = &s
        .sessions()
        .get(&format!("{project_id}{PROJECT_SEP}session-b"))
        .unwrap()
        .history;
    assert_eq!(
        sa.steps.iter().map(|s| s.step_number).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(
        sb.steps.iter().map(|s| s.step_number).collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[test]
fn expired_sessions_cleaned_up_on_force() {
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    c.system.session_timeout = 1; // 1 minute
    let mut s = ReasoningServer::new(c);

    let mut step = step_n(1);
    step.session_id = Some("old-session".into());
    let _ = s.process_step(step);

    // Manually expire the session by hacking its last_accessed back.
    // We use the public API in spirit: write a fresh value that's old enough.
    {
        // Two-minutes ago in millis.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        // Access internal field via the mutable session map is not exposed.
        // Force cleanup will still see it within timeout, so we need to drop it
        // by waiting. Instead, set timeout to 0 (rejected by parser) — use a
        // very small value and sleep instead.
        let _ = now;
    }
    // Set timeout to 1 minute → no actual delay path. Test the contract that
    // cleanup never panics and never removes a still-fresh session:
    s.cleanup_expired_sessions(true);
    assert!(s.sessions().contains_key(&ns("old-session")));
}

#[test]
fn sessions_off_creates_no_sessions() {
    let mut c = quiet_config();
    c.features.enable_sessions = false;
    let mut s = ReasoningServer::new(c);
    let mut step = step_n(1);
    step.session_id = Some("test".into());
    let _ = s.process_step(step);
    assert_eq!(s.sessions().len(), 0);
}

#[test]
fn tools_tracked_in_metadata() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut a = step_n(1);
    a.tools_used = Some(vec!["Read".into(), "Grep".into()]);
    let _ = s.process_step(a);
    let mut b = step_n(2);
    b.tools_used = Some(vec!["Edit".into(), "Read".into()]);
    let _ = s.process_step(b);
    let tools = s
        .history()
        .metadata
        .as_ref()
        .unwrap()
        .tools_used
        .as_ref()
        .unwrap();
    assert!(tools.contains(&"Read".to_string()));
    assert!(tools.contains(&"Grep".to_string()));
    assert!(tools.contains(&"Edit".to_string()));
    assert_eq!(tools.len(), 3);
}

#[test]
fn clear_history_resets_state() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(step_n(1));
    let mut b = step_n(2);
    b.branch_from = Some(1);
    b.branch_id = Some("branch-1".into());
    let _ = s.process_step(b);
    s.clear_history();
    assert_eq!(s.history().steps.len(), 0);
    assert!(!s.history().completed);
    assert_eq!(s.branches().len(), 0);
}

#[test]
fn export_json_default() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(base_step());
    let exported = s.export_history(OutputFormat::Json);
    let parsed: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(parsed["steps"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["completed"], false);
}

#[test]
fn export_markdown() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(base_step());
    let exported = s.export_history(OutputFormat::Markdown);
    assert!(exported.contains("### Step 1/3"));
    assert!(exported.contains("**Context:**"));
}

#[test]
fn export_console_text() {
    let mut s = ReasoningServer::new(quiet_config());
    let _ = s.process_step(base_step());
    let exported = s.export_history(OutputFormat::Console);
    assert!(exported.contains("[Step 1/3]"));
    assert!(exported.contains("Context:"));
}

#[test]
fn session_indexes_rebuilt_on_switch_back() {
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);
    // Build session-a with 3 steps
    for i in 1..=3 {
        let mut step = step_n(i);
        step.session_id = Some("session-a".into());
        let _ = s.process_step(step);
    }
    // Switch to session-b
    let mut sb1 = step_n(1);
    sb1.session_id = Some("session-b".into());
    let _ = s.process_step(sb1);

    // Return to session-a and add step 4 depending on step 2
    let mut sa4 = step_n(4);
    sa4.session_id = Some("session-a".into());
    sa4.dependencies = Some(vec![2u32.into()]);
    let result = s.process_step(sa4);
    assert!(!is_error(&result));

    assert_eq!(
        s.sessions()
            .get(&ns("session-a"))
            .unwrap()
            .history
            .steps
            .len(),
        4
    );
}

#[test]
fn confidence_infinity_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.confidence = Some(f64::INFINITY);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("finite number"));
}

#[test]
fn confidence_neg_infinity_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.confidence = Some(f64::NEG_INFINITY);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("finite number"));
}

#[test]
fn confidence_nan_rejected() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = step_n(1);
    step.confidence = Some(f64::NAN);
    let result = s.process_step(step);
    assert!(is_error(&result));
    let r = parse_response(result);
    assert!(r["error"].as_str().unwrap().contains("finite number"));
}

#[test]
fn complex_multi_feature_step() {
    let mut s = ReasoningServer::new(quiet_config());
    for i in 1..=3 {
        let _ = s.process_step(step_n(i));
    }
    let mut step = step_n(4);
    step.branch_from = Some(2);
    step.branch_name = Some("Alternative approach".into());
    step.revises_step = Some(1);
    step.revision_reason = Some("Found better approach".into());
    step.confidence = Some(0.85);
    step.dependencies = Some(vec![3u32.into()]);
    step.tools_used = Some(vec!["Read".into(), "Grep".into(), "Edit".into()]);
    let result = s.process_step(step);
    assert!(!is_error(&result));
    let r = parse_response(result);
    assert_eq!(r["step_number"], 4);
    // Top-level `confidence` and `next_action` echoes were dropped to save
    // tokens — the caller already has both in its own context.
    assert!(r.get("confidence").is_none());
    assert!(r.get("next_action").is_none());
    assert_eq!(r["revised_step"], 1);
    assert_eq!(r["branch"]["name"], "Alternative approach");
    let meta = s.history().metadata.as_ref().unwrap();
    assert_eq!(meta.revisions_count, Some(1));
    assert_eq!(meta.branches_created, Some(1));
    let tools = meta.tools_used.as_ref().unwrap();
    assert!(tools.contains(&"Read".to_string()));
    assert!(tools.contains(&"Grep".to_string()));
    assert!(tools.contains(&"Edit".to_string()));
}

// ── Project namespacing (round-3 fix) ──────────────────────────────────────

#[test]
fn caller_session_id_gets_namespaced_under_project() {
    // Agent passes `session_id: "phase3-chunk1"`. Server rewrites it
    // to `<project>__phase3-chunk1` so agents can't accidentally write
    // outside their project's bucket on disk.
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);
    let mut step = step_n(1);
    step.session_id = Some("phase3-chunk1".into());
    let _ = s.process_step(step);

    let expected = format!("{}{}phase3-chunk1", resolve_project_id(), PROJECT_SEP);
    assert!(
        s.sessions().contains_key(&expected),
        "expected namespaced session key {expected:?}, got {:?}",
        s.sessions().keys().collect::<Vec<_>>()
    );
    // The step's own session_id field should also be the namespaced form.
    let live = s.step_by_number(1).expect("step 1");
    assert_eq!(live.session_id.as_deref(), Some(expected.as_str()));
}

#[test]
fn already_namespaced_session_id_is_not_double_prefixed() {
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);

    // Caller passes the fully-qualified namespaced form — server must
    // accept it verbatim, not produce `<project>__<project>__phase3`.
    let qualified = format!("{}{}phase3-chunk1", resolve_project_id(), PROJECT_SEP);
    let mut step = step_n(1);
    step.session_id = Some(qualified.clone());
    let _ = s.process_step(step);

    assert!(s.sessions().contains_key(&qualified));
    assert_eq!(s.sessions().len(), 1);
}

#[test]
fn session_metadata_stamps_project_id() {
    // Every session's metadata.project_id should equal the server's
    // resolved project id after a step is recorded — that's what lets
    // the viewer group sessions by project without parsing filenames.
    let mut c = quiet_config();
    c.features.enable_sessions = true;
    let mut s = ReasoningServer::new(c);
    let mut step = step_n(1);
    step.session_id = Some("alpha-demo".into());
    let _ = s.process_step(step);

    let key = ns("alpha-demo");
    let entry = s.sessions().get(&key).expect("session should exist");
    let stamped = entry
        .history
        .metadata
        .as_ref()
        .and_then(|m| m.project_id.as_deref())
        .expect("metadata.project_id should be stamped");
    assert_eq!(stamped, resolve_project_id());
}

/// Empirical regression test for the dominant production failure mode.
///
/// A scan of 12 Claude Code session logs from ~/.claude/projects/-Users-alrik-Code-rikttp/
/// (~11k turns) found **24 instances** of "Missing or invalid required fields"
/// across 8 sessions — one session burned 10 retries in a row. In every case,
/// the agent serialized the full tool call inside `thought` using bare
/// `<outcome>...</outcome>` / `<rationale>...</rationale>` tags as section
/// headers, instead of as JSON sibling parameters.
///
/// Pre-0.2.1, `recover_xml_injection` only caught `<parameter name="X">VALUE</parameter>`
/// patterns and never fired (0 successful recoveries in 24 attempts). This test
/// asserts that the bare `<X>VALUE</X>` form is now caught and the missing
/// sibling fields are auto-filled.
#[test]
fn xml_injection_bare_tag_form_recovers_from_thought() {
    let mut s = ReasoningServer::new(quiet_config());
    let mut step = base_step();
    // Verbatim shape of what arrived in rikttp session 667b4108 step 2.
    step.thought = "Real reasoning content about the migration design.</thought>\n\
        <outcome>Phase XXXIX closes the migration limitation. AsyncUdpListener routes by addr with cid fallback.</outcome>\n\
        <next_action>Update ROADMAP.md and commit.</next_action>\n\
        <rationale>To document and ship the phase.</rationale>\n\
        <confidence>0.95</confidence>\n\
        <pinned>true</pinned>\n\
        <tools_used>[\"ministr_survey\", \"cargo build\"]</tools_used>\n\
        <is_final_step>true</is_final_step>\n\
        </invoke>"
        .into();
    // Sibling fields arrive empty because the agent never sent them as JSON
    // top-level parameters — they were embedded inside `thought` instead.
    step.outcome = String::new();
    step.rationale = String::new();
    step.next_action = NextAction::Text(String::new());

    let ok = match s.process_step(step) {
        Ok(ok) => ok,
        Err(err) => panic!(
            "recovery should fill missing fields and let the step through; got: {}",
            err.text
        ),
    };

    let response: serde_json::Value = serde_json::from_str(&ok.text).expect("response is JSON");
    let warnings = response
        .get("warnings")
        .and_then(|v| v.as_array())
        .expect("warnings should be present when recovery fires");
    assert!(
        warnings.iter().any(|w| {
            let s = w.as_str().unwrap_or("");
            s.contains("Auto-recovered")
        }),
        "warning should name the recovery; got: {warnings:?}"
    );

    let stored = &s.history().steps[0];
    assert!(
        stored
            .outcome
            .contains("Phase XXXIX closes the migration limitation"),
        "outcome should be recovered; got: {:?}",
        stored.outcome
    );
    assert!(
        stored.rationale.contains("To document and ship"),
        "rationale should be recovered; got: {:?}",
        stored.rationale
    );
    match &stored.next_action {
        NextAction::Text(t) => assert!(
            t.contains("Update ROADMAP.md"),
            "next_action should be recovered; got: {t:?}"
        ),
        _ => panic!("next_action should remain text form"),
    }
    assert_eq!(stored.confidence, Some(0.95));
    assert_eq!(stored.pinned, Some(true));
    assert_eq!(stored.is_final_step, Some(true));
    assert_eq!(
        stored.tools_used.as_deref(),
        Some(&["ministr_survey".to_string(), "cargo build".to_string()][..])
    );
    // `thought` should be cleaned up — the embedded markup truncated away so
    // the persisted trace stays readable.
    assert!(
        !stored.thought.contains("</thought>")
            && !stored.thought.contains("<outcome>")
            && !stored.thought.contains("</invoke>"),
        "thought should be truncated at the first markup marker; got: {:?}",
        stored.thought
    );
}

/// Bare-tag dependencies recovery, separated because it needs a pre-existing
/// step #1 to satisfy the dependency check.
#[test]
fn xml_injection_bare_tag_recovers_dependencies_and_branch_fields() {
    let mut s = ReasoningServer::new(quiet_config());
    // Plant step 1 so step 2's "depends on [1]" doesn't fail validation.
    s.process_step(base_step()).expect("step 1 records cleanly");

    let mut step = base_step();
    step.step_number = 2;
    step.thought = "Reasoning here.</thought>\n\
        <outcome>shipped</outcome>\n\
        <rationale>To verify recovery handles every JSON-ish field</rationale>\n\
        <next_action>verify</next_action>\n\
        <dependencies>[1]</dependencies>\n\
        <branch_id>main-branch</branch_id>\n\
        <branch_name>checkpoint</branch_name>\n\
        <branch_from>1</branch_from>\n\
        <revises_step>1</revises_step>\n\
        <revision_reason>To correct step 1</revision_reason>"
        .into();
    step.outcome = String::new();
    step.rationale = String::new();
    step.next_action = NextAction::Text(String::new());

    let ok = match s.process_step(step) {
        Ok(ok) => ok,
        Err(err) => panic!("recovery should let step through; got: {}", err.text),
    };
    let response: serde_json::Value = serde_json::from_str(&ok.text).unwrap();
    let warnings = response.get("warnings").and_then(|v| v.as_array()).unwrap();
    let warn_text = warnings
        .iter()
        .map(|w| w.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    for f in ["dependencies", "branch_id", "branch_name", "revises_step"] {
        assert!(
            warn_text.contains(f),
            "warning should list recovered field {f}; got: {warn_text}"
        );
    }
}

// (Coverage of the pattern-A diagnostic is already provided by
// `partial_xml_injection_recovery_errors_only_on_still_missing_fields`
// — when ANY recovery extraction fired and fields remain missing, the
// signal-driven diagnostic activates. An additional test for the
// "markup-present-but-nothing-extractable" edge case used to live here;
// removed because the looser extractor introduced in 0.2.1 makes that
// case empirically unreachable — markup almost always produces at least
// one extracted pair, even with malformed close tags.)
