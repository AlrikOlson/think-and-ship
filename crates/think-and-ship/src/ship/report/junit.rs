//! The JUnit XML adapter — the baseline report format.
//!
//! JUnit XML has no single normative schema; what exists is the de-facto
//! Ant/Jenkins dialect every runner emits a superset of. This adapter reads
//! only the intersection that is stable across emitters, verified against
//! first-party docs (read 2026-08-15):
//!
//! - cargo-nextest, `[profile.<name>.junit] path = ...`
//!   (<https://nexte.st/docs/machine-readable/junit/>): one `<testsuite>` per
//!   test binary, one `<testcase>` per test, a `<failure>` child on failure.
//! - pytest, `--junit-xml=PATH`: same element vocabulary, `<skipped>` for
//!   skips, `<error>` for collection/setup errors.
//!
//! Counting walks the `<testcase>` elements rather than trusting the
//! `tests=`/`failures=` attributes on `<testsuite>`: the attributes are
//! emitter-summarized and the per-case walk is what yields the failing test
//! NAMES — the addressable thing the record exists to carry.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use super::{FormatRegistration, MAX_FAILURES, TestFailure, TestResults, scrub_message};

/// The one string this adapter answers to, bound to the parser it names.
pub const FORMAT: &str = "junit";

pub const REGISTRATION: FormatRegistration = FormatRegistration { key: FORMAT, parse };

#[derive(PartialEq)]
enum CaseStatus {
    Passed,
    Failed,
    Skipped,
}

struct CaseState {
    name: String,
    status: CaseStatus,
    message: Option<String>,
}

fn attr(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.try_get_attribute(key).ok().flatten().and_then(|a| {
        a.normalized_value(XmlVersion::default())
            .ok()
            .map(|v| v.into_owned())
    })
}

fn case_name(e: &BytesStart<'_>) -> String {
    let name = attr(e, "name").unwrap_or_default();
    match attr(e, "classname").filter(|c| !c.is_empty()) {
        Some(class) => format!("{class}::{name}"),
        None => name,
    }
}

fn secs_attr(e: &BytesStart<'_>) -> Option<f64> {
    attr(e, "time").and_then(|t| t.trim().parse::<f64>().ok())
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_ms(secs: f64) -> u64 {
    if secs.is_finite() && secs > 0.0 {
        (secs * 1000.0).round() as u64
    } else {
        0
    }
}

/// Parse JUnit XML content into a summary. Errors on malformed XML or on a
/// document with no `<testsuite>`/`<testsuites>` element — an empty suite is
/// a valid report, a random XML file is not.
fn parse(content: &str) -> Result<TestResults, String> {
    let mut reader = Reader::from_str(content);
    reader.config_mut().trim_text(true);

    let mut results = TestResults::default();
    let mut saw_suite = false;
    let mut current: Option<CaseState> = None;
    let mut in_failure_body = false;
    // Duration, best source first: sum of per-testcase `time` attrs; else the
    // `<testsuites>` wrapper's own `time`; else the sum over `<testsuite>`s.
    let mut case_secs = 0.0_f64;
    let mut saw_case_time = false;
    let mut suite_secs = 0.0_f64;
    let mut saw_suite_time = false;
    let mut suites_secs: Option<f64> = None;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("XML error at byte {}: {e}", reader.buffer_position()))?;
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(event, Event::Empty(_));
                match e.name().as_ref() {
                    b"testsuites" => {
                        saw_suite = true;
                        if suites_secs.is_none() {
                            suites_secs = secs_attr(e);
                        }
                    }
                    b"testsuite" => {
                        saw_suite = true;
                        if let Some(s) = secs_attr(e) {
                            suite_secs += s;
                            saw_suite_time = true;
                        }
                    }
                    b"testcase" => {
                        if let Some(s) = secs_attr(e) {
                            case_secs += s;
                            saw_case_time = true;
                        }
                        let state = CaseState {
                            name: case_name(e),
                            status: CaseStatus::Passed,
                            message: None,
                        };
                        if is_empty {
                            tally(&mut results, state);
                        } else {
                            current = Some(state);
                        }
                    }
                    b"failure" | b"error" => {
                        if let Some(case) = current.as_mut() {
                            case.status = CaseStatus::Failed;
                            if case.message.is_none() {
                                case.message = attr(e, "message").filter(|m| !m.trim().is_empty());
                            }
                            in_failure_body = !is_empty && case.message.is_none();
                        }
                    }
                    b"skipped" => {
                        if let Some(case) = current.as_mut()
                            && case.status == CaseStatus::Passed
                        {
                            case.status = CaseStatus::Skipped;
                        }
                    }
                    _ => {}
                }
            }
            // Some emitters put the failure message in the element body
            // rather than the `message` attribute; take the first text/CDATA
            // chunk there when the attribute was absent.
            Event::Text(ref t) if in_failure_body => {
                if let Some(case) = current.as_mut()
                    && case.message.is_none()
                    && let Ok(text) = t.xml_content(XmlVersion::default())
                    && !text.trim().is_empty()
                {
                    case.message = Some(text.into_owned());
                    in_failure_body = false;
                }
            }
            Event::CData(ref c) if in_failure_body => {
                if let Some(case) = current.as_mut()
                    && case.message.is_none()
                {
                    let text = String::from_utf8_lossy(c).into_owned();
                    if !text.trim().is_empty() {
                        case.message = Some(text);
                        in_failure_body = false;
                    }
                }
            }
            Event::End(ref e) => match e.name().as_ref() {
                b"testcase" => {
                    if let Some(case) = current.take() {
                        tally(&mut results, case);
                    }
                    in_failure_body = false;
                }
                b"failure" | b"error" => in_failure_body = false,
                _ => {}
            },
            _ => {}
        }
    }

    if !saw_suite {
        return Err("no <testsuite> or <testsuites> element found — not a JUnit report".into());
    }

    let secs = if saw_case_time {
        Some(case_secs)
    } else if suites_secs.is_some() {
        suites_secs
    } else if saw_suite_time {
        Some(suite_secs)
    } else {
        None
    };
    results.duration_ms = secs.map(to_ms);

    Ok(results)
}

fn tally(results: &mut TestResults, case: CaseState) {
    results.total += 1;
    match case.status {
        CaseStatus::Passed => results.passed += 1,
        CaseStatus::Skipped => results.skipped += 1,
        CaseStatus::Failed => {
            results.failed += 1;
            if results.failures.len() < MAX_FAILURES {
                results.failures.push(TestFailure {
                    name: case.name,
                    message: case.message.as_deref().map(scrub_message),
                });
            }
        }
    }
}
