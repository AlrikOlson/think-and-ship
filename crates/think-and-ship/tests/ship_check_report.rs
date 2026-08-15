//! The `ship_check` structured-report contract (structured-test-results).
//!
//! A check recorded with a `command` keeps the real exit code; a `report`
//! argument adds a parsed summary NEXT TO that exit code, never in place of
//! it. These tests hold the three rules in priority order: the exit code stays
//! the source of truth for `verified`, every report problem degrades to
//! exactly the pre-report behaviour with the record saying parsing did not
//! happen, and only a machine-readable file the runner was asked for is ever
//! read — plus the registry gates that keep the supported-format list from
//! drifting from what is registered.

use rmcp::handler::server::wrapper::Parameters;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::ship::mcp::ShipService;
use think_and_ship::ship::mcp::handlers::{CheckArgs, ReportArg};
use think_and_ship::ship::report::{
    self, FORMATS, MAX_FAILURES, ReportRecord, TestResults, read_report,
};

fn junit_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="0">
  <testsuite name="my-binary" tests="4" failures="1" errors="0" skipped="1">
    <testcase name="passes_quickly" classname="my_binary" time="0.010"/>
    <testcase name="also_passes" classname="my_binary" time="0.500">
    </testcase>
    <testcase name="gets_skipped" classname="my_binary" time="0.001">
      <skipped/>
    </testcase>
    <testcase name="fails_loudly" classname="my_binary" time="1.200">
      <failure message="assertion failed: left == right">stack trace here</failure>
    </testcase>
  </testsuite>
</testsuites>
"#
}

fn write_report(dir: &tempfile::TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path.to_string_lossy().into_owned()
}

fn parse_fixture(content: &str) -> TestResults {
    let dir = tempfile::tempdir().unwrap();
    let path = write_report(&dir, "junit.xml", content);
    let (record, results) = read_report("junit", &path);
    assert!(record.parsed, "fixture should parse: {:?}", record.error);
    results.unwrap()
}

// ── The parsed summary ─────────────────────────────────────────────

#[test]
fn junit_fixture_yields_counts_duration_and_the_failing_test_by_name() {
    let results = parse_fixture(junit_fixture());
    assert_eq!(results.total, 4);
    assert_eq!(results.passed, 2);
    assert_eq!(results.failed, 1);
    assert_eq!(results.skipped, 1);
    // Per-testcase times sum: 0.010 + 0.500 + 0.001 + 1.200 = 1.711s.
    assert_eq!(results.duration_ms, Some(1711));
    // The addressable thing the record exists to carry: WHICH test failed.
    assert_eq!(results.failures.len(), 1);
    assert_eq!(results.failures[0].name, "my_binary::fails_loudly");
    assert_eq!(
        results.failures[0].message.as_deref(),
        Some("assertion failed: left == right")
    );
}

#[test]
fn a_failure_message_in_the_element_body_is_read_when_the_attribute_is_absent() {
    let results = parse_fixture(
        r#"<testsuite><testcase name="t"><failure>expected 3, got 4</failure></testcase></testsuite>"#,
    );
    assert_eq!(
        results.failures[0].message.as_deref(),
        Some("expected 3, got 4")
    );
}

#[test]
fn an_all_green_run_keeps_just_the_counts() {
    let results =
        parse_fixture(r#"<testsuite><testcase name="a"/><testcase name="b"/></testsuite>"#);
    assert_eq!((results.total, results.passed), (2, 2));
    assert!(results.failures.is_empty(), "no per-test detail for green");
}

#[test]
fn suite_level_time_is_the_fallback_when_testcases_carry_none() {
    let results = parse_fixture(r#"<testsuite time="8.431"><testcase name="a"/></testsuite>"#);
    assert_eq!(results.duration_ms, Some(8431));
}

// ── Secrets and storage bounds ─────────────────────────────────────

#[test]
fn failure_messages_are_redacted_before_they_reach_the_record() {
    let results = parse_fixture(
        r#"<testsuite><testcase name="t"><failure message="request failed: token=lin_api_a1b2c3d4e5f6"/></testcase></testsuite>"#,
    );
    let message = results.failures[0].message.as_deref().unwrap();
    assert!(
        !message.contains("lin_api_a1b2c3d4e5f6"),
        "the interpolated credential must not survive into the record: {message}"
    );
    assert!(message.contains("[REDACTED]"), "{message}");
}

#[test]
fn failure_detail_is_bounded_not_inlined_wholesale() {
    // MAX_FAILURES + 5 failing cases: the counts stay honest, the stored
    // per-test list is capped.
    let cases: String = (0..MAX_FAILURES + 5)
        .map(|i| format!(r#"<testcase name="t{i}"><failure message="boom"/></testcase>"#))
        .collect();
    let results = parse_fixture(&format!("<testsuite>{cases}</testsuite>"));
    assert_eq!(results.failed as usize, MAX_FAILURES + 5);
    assert_eq!(results.failures.len(), MAX_FAILURES);

    // And one message is truncated rather than stored whole.
    let long = "x".repeat(report::FAILURE_MESSAGE_CHARS * 3);
    let results = parse_fixture(&format!(
        r#"<testsuite><testcase name="t"><failure message="{long}"/></testcase></testsuite>"#
    ));
    let stored = results.failures[0].message.as_deref().unwrap();
    assert!(
        stored.chars().count() < report::FAILURE_MESSAGE_CHARS + 40,
        "stored {} chars",
        stored.chars().count()
    );
    assert!(stored.contains("truncated"), "{stored}");
}

// ── The degradation contract ───────────────────────────────────────

fn degraded(record: &ReportRecord, results: &Option<TestResults>, needle: &str) {
    assert!(!record.parsed, "the record must say parsing did not happen");
    assert!(results.is_none(), "no results without a parse");
    let error = record.error.as_deref().unwrap_or_default();
    assert!(
        error.contains(needle),
        "{error:?} should mention {needle:?}"
    );
}

#[test]
fn every_report_problem_degrades_and_says_so() {
    let dir = tempfile::tempdir().unwrap();

    // Missing file: record it and carry on, per the parse-failure rule.
    let missing = dir.path().join("nope.xml").to_string_lossy().into_owned();
    let (record, results) = read_report("junit", &missing);
    degraded(&record, &results, "not found");

    // Malformed content.
    let garbage = write_report(&dir, "garbage.xml", "definitely { not xml");
    let (record, results) = read_report("junit", &garbage);
    degraded(&record, &results, "did not parse");

    // Well-formed XML that is not a JUnit report.
    let wrong = write_report(&dir, "wrong.xml", "<html><body/></html>");
    let (record, results) = read_report("junit", &wrong);
    degraded(&record, &results, "did not parse");
}

// ── The registry gates (the tracker/ shape) ────────────────────────

#[test]
fn the_unknown_format_refusal_reads_its_list_from_the_registry() {
    let (record, results) = read_report("trx", "unused.trx");
    degraded(&record, &results, &report::known_list());
    for registration in FORMATS {
        let error = record.error.as_deref().unwrap();
        assert!(error.contains(registration.key), "{error}");
    }
}

/// The module doc claims junit is the only registered format; this reads the
/// table rather than trusting the sentence, and fails when a format is added
/// without moving the doc.
#[test]
fn the_registered_formats_are_exactly_the_ones_the_module_doc_names() {
    let keys: Vec<&str> = FORMATS.iter().map(|f| f.key).collect();
    assert_eq!(
        keys,
        vec!["junit"],
        "update crate::ship::report's module doc"
    );
    let mut deduped = keys.clone();
    deduped.dedup();
    assert_eq!(keys, deduped, "a format registered twice is unreachable");
}

// ── The MCP seam ───────────────────────────────────────────────────

fn service() -> ShipService {
    ShipService::new(ShipEngine::new("test-report-abc123".into()))
}

async fn active_task(svc: &ShipService) {
    use rmcp::handler::server::wrapper::Parameters;
    use think_and_ship::ship::mcp::handlers::{PlanAction, PlanArgs, SetObjectiveArgs, StartArgs};
    svc.ship_set_objective(Parameters(SetObjectiveArgs {
        description: "structured results".into(),
        acceptance_criteria: vec![],
        constraints: vec![],
        scope: String::new(),
    }))
    .await
    .unwrap();
    svc.ship_plan(Parameters(PlanArgs {
        action: PlanAction::Add,
        task_id: "gate".into(),
        title: Some("run the gate".into()),
        task_type: None,
        estimate: None,
        after: None,
        think_branch: None,
    }))
    .await
    .unwrap();
    svc.ship_start(Parameters(StartArgs {
        task_id: "gate".into(),
    }))
    .await
    .unwrap();
}

fn check_args(command: Option<&str>, report: Option<ReportArg>) -> CheckArgs {
    CheckArgs {
        task_id: None,
        check_type: think_and_ship::ship::domain::check::CheckType::Test,
        name: "suite".into(),
        passed: command.is_none().then_some(true),
        details: String::new(),
        required: true,
        command: command.map(String::from),
        report,
    }
}

fn structured(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    result.structured_content.clone().expect("structured")
}

#[tokio::test]
async fn ship_check_stores_the_parsed_summary_next_to_the_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_report(&dir, "junit.xml", junit_fixture());
    let svc = service();
    active_task(&svc).await;

    let result = svc
        .ship_check(Parameters(check_args(
            Some("echo ok"),
            Some(ReportArg {
                format: "junit".into(),
                path,
            }),
        )))
        .await
        .unwrap();
    let check = structured(&result);

    // The exit code remains the source of truth for passed/verified…
    assert_eq!(check["passed"], serde_json::json!(true));
    assert_eq!(check["verified"], serde_json::json!(true));
    assert_eq!(check["exit_code"], serde_json::json!(0));
    // …and the structured detail sits next to it.
    assert_eq!(check["report"]["parsed"], serde_json::json!(true));
    assert_eq!(check["report"]["format"], serde_json::json!("junit"));
    assert_eq!(check["results"]["total"], serde_json::json!(4));
    assert_eq!(check["results"]["failed"], serde_json::json!(1));
    assert_eq!(
        check["results"]["failures"][0]["name"],
        serde_json::json!("my_binary::fails_loudly")
    );
}

#[tokio::test]
async fn a_report_problem_never_fails_the_check_or_flips_verified() {
    let svc = service();
    active_task(&svc).await;

    let result = svc
        .ship_check(Parameters(check_args(
            Some("echo ok"),
            Some(ReportArg {
                format: "junit".into(),
                path: "definitely/not/there/junit.xml".into(),
            }),
        )))
        .await
        .unwrap();
    let check = structured(&result);

    // Exactly today's behaviour: green command stays a green verified check.
    assert_eq!(check["passed"], serde_json::json!(true));
    assert_eq!(check["verified"], serde_json::json!(true));
    assert_eq!(check["exit_code"], serde_json::json!(0));
    // And the record says parsing did not happen.
    assert_eq!(check["report"]["parsed"], serde_json::json!(false));
    assert!(
        check["report"]["error"]
            .as_str()
            .unwrap()
            .contains("not found")
    );
    assert!(check.get("results").is_none() || check["results"].is_null());
}

#[tokio::test]
async fn a_check_without_a_report_is_byte_for_byte_the_old_shape() {
    let svc = service();
    active_task(&svc).await;
    let result = svc
        .ship_check(Parameters(check_args(Some("echo ok"), None)))
        .await
        .unwrap();
    let check = structured(&result);
    assert!(
        check.get("report").is_none() && check.get("results").is_none(),
        "no report requested — no report keys on the record: {check}"
    );
}
