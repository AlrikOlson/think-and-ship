//! Approval gates — agent work paused on a question only a human can answer.
//!
//! A gate is a CLOUD record (`family: ship, kind: gate`), not local state: its
//! whole purpose is to surface in the webapp and be answered from a browser,
//! so without a connected workspace a gate cannot exist — opening one then
//! resolves to the declared default immediately, in words. The same law
//! `mcp::elicit` encodes locally (a pause must never hang a headless session)
//! holds here structurally: every gate carries `expires_at` and `default_key`,
//! so ANY reader resolves an unanswered gate identically once the clock
//! passes expiry — the engine, the backend answer route, and the webapp
//! cannot disagree about what an unanswered gate means.
//!
//! Wire lifecycle (docs/UNIFIED_CONTRACT.md §Approval gates):
//! `pending` → `answered` (the backend's answer route stamps who/when from
//! the authenticated session; first answer wins). `expired` is DERIVED, not
//! written: the one writer of answers is the backend route, the one writer of
//! the gate itself is the opening engine, and the clock decides expiry — so
//! no reader ever waits on a state write to know an unanswered gate's
//! outcome. A stored `expired` stamp is honored if one ever appears, but
//! nothing in this codebase writes one.

use serde::{Deserialize, Serialize};

/// One choice a human can pick. `key` is the stable wire token the agent
/// branches on; `label` is the plain-language text the browser shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOption {
    pub key: String,
    pub label: String,
}

/// The recorded human decision. Stamped by the backend answer route from the
/// authenticated session — a browser-supplied identity can never override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateAnswer {
    pub choice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub decided_by: String,
    pub decided_at: String,
}

/// The gate record payload (the `record` of a `ship`/`gate` envelope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    pub id: String,
    /// The one plain-language sentence the human answers.
    pub question: String,
    /// Plain-prose context: what happens on each answer, what was verified.
    #[serde(default)]
    pub body: String,
    pub options: Vec<GateOption>,
    /// The safe answer an unanswered gate resolves to at `expires_at`.
    pub default_key: String,
    /// `pending` | `answered` | `expired`.
    pub state: String,
    pub opened_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<GateAnswer>,
}

/// Bounds on a gate's timeout: a human needs a real chance to see it, and an
/// abandoned gate must not outlive a working week.
pub const MIN_TIMEOUT_SECS: i64 = 30;
pub const MAX_TIMEOUT_SECS: i64 = 7 * 24 * 3600;
pub const DEFAULT_TIMEOUT_SECS: i64 = 3600;

/// Validate the option set + default and build a fresh pending gate.
/// Errors are plain sentences suitable for a soft tool error.
pub fn open(
    id: String,
    question: &str,
    body: &str,
    options: Vec<GateOption>,
    default_key: &str,
    timeout_secs: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Gate, String> {
    if question.trim().is_empty() {
        return Err("`question` is required — the one sentence a human answers".into());
    }
    if options.len() < 2 {
        return Err("`options` needs at least two choices (e.g. yes / no)".into());
    }
    for o in &options {
        if o.key.trim().is_empty() || o.label.trim().is_empty() {
            return Err("every option needs a non-empty `key` and `label`".into());
        }
    }
    let mut keys: Vec<&str> = options.iter().map(|o| o.key.as_str()).collect();
    keys.sort_unstable();
    if keys.windows(2).any(|w| w[0] == w[1]) {
        return Err("option `key`s must be unique".into());
    }
    if !options.iter().any(|o| o.key == default_key) {
        return Err(format!(
            "`default` must be one of the option keys ({})",
            keys.join(", ")
        ));
    }
    let timeout = timeout_secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS);
    Ok(Gate {
        id,
        question: question.trim().to_string(),
        body: body.trim().to_string(),
        options,
        default_key: default_key.to_string(),
        state: "pending".into(),
        opened_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: (now + chrono::Duration::seconds(timeout))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        answer: None,
    })
}

/// How a gate stands right now. Pure — derived from the record + the clock, so
/// every reader (engine, backend, browser) resolves identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Pending {
        seconds_left: i64,
    },
    Answered(GateAnswer),
    /// Unanswered past `expires_at` — the declared default applies.
    Expired {
        choice: String,
    },
}

/// Resolve a gate record (as fetched from the cloud) against `now`.
/// A malformed record is an error in words, never a panic.
pub fn resolve(
    record: &serde_json::Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Resolution, String> {
    let gate: Gate = serde_json::from_value(record.clone())
        .map_err(|e| format!("the record is not a well-formed gate: {e}"))?;
    if let Some(answer) = gate.answer {
        return Ok(Resolution::Answered(answer));
    }
    let expires = chrono::DateTime::parse_from_rfc3339(&gate.expires_at)
        .map_err(|e| format!("the gate's `expires_at` is not a valid timestamp: {e}"))?;
    let seconds_left = (expires.with_timezone(&chrono::Utc) - now).num_seconds();
    if gate.state == "expired" || seconds_left <= 0 {
        return Ok(Resolution::Expired {
            choice: gate.default_key,
        });
    }
    Ok(Resolution::Pending { seconds_left })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn yes_no() -> Vec<GateOption> {
        vec![
            GateOption {
                key: "yes".into(),
                label: "Yes, go ahead".into(),
            },
            GateOption {
                key: "no".into(),
                label: "No, hold".into(),
            },
        ]
    }

    fn t0() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 31, 12, 0, 0).unwrap()
    }

    #[test]
    fn open_validates_question_options_and_default() {
        assert!(open("g".into(), " ", "", yes_no(), "no", 60, t0()).is_err());
        assert!(open("g".into(), "q?", "", vec![], "no", 60, t0()).is_err());
        assert!(
            open("g".into(), "q?", "", yes_no(), "maybe", 60, t0())
                .unwrap_err()
                .contains("default"),
            "a default outside the option set must be named as the problem"
        );
        let dup = vec![
            GateOption {
                key: "x".into(),
                label: "A".into(),
            },
            GateOption {
                key: "x".into(),
                label: "B".into(),
            },
        ];
        assert!(open("g".into(), "q?", "", dup, "x", 60, t0()).is_err());
    }

    #[test]
    fn open_clamps_the_timeout_into_bounds() {
        let g = open("g".into(), "q?", "", yes_no(), "no", 1, t0()).unwrap();
        assert_eq!(g.expires_at, "2026-07-31T12:00:30Z");
        let g = open("g".into(), "q?", "", yes_no(), "no", i64::MAX, t0()).unwrap();
        assert_eq!(g.expires_at, "2026-08-07T12:00:00Z");
    }

    #[test]
    fn a_fresh_gate_is_pending_with_the_clock_running() {
        let g = open("g".into(), "q?", "ctx", yes_no(), "no", 600, t0()).unwrap();
        let record = serde_json::to_value(&g).unwrap();
        let r = resolve(&record, t0() + chrono::Duration::seconds(100)).unwrap();
        assert_eq!(r, Resolution::Pending { seconds_left: 500 });
    }

    #[test]
    fn an_answer_wins_over_everything_including_expiry() {
        // The backend refuses answers past expiry, so an answer in the record
        // is always legitimate — even a reader with a skewed clock must honor
        // it rather than second-guess with Expired.
        let mut g = open("g".into(), "q?", "", yes_no(), "no", 60, t0()).unwrap();
        g.state = "answered".into();
        g.answer = Some(GateAnswer {
            choice: "yes".into(),
            note: None,
            decided_by: "Dana".into(),
            decided_at: "2026-07-31T12:00:30Z".into(),
        });
        let record = serde_json::to_value(&g).unwrap();
        let r = resolve(&record, t0() + chrono::Duration::days(2)).unwrap();
        match r {
            Resolution::Answered(a) => {
                assert_eq!(a.choice, "yes");
                assert_eq!(a.decided_by, "Dana");
            }
            other => panic!("expected Answered, got {other:?}"),
        }
    }

    #[test]
    fn an_unanswered_gate_past_expiry_resolves_to_the_declared_default() {
        let g = open("g".into(), "q?", "", yes_no(), "no", 60, t0()).unwrap();
        let record = serde_json::to_value(&g).unwrap();
        let r = resolve(&record, t0() + chrono::Duration::seconds(61)).unwrap();
        assert_eq!(
            r,
            Resolution::Expired {
                choice: "no".into()
            }
        );
    }

    #[test]
    fn a_materialized_expired_state_resolves_expired_even_before_a_skewed_clock() {
        // Once any reader has stamped `expired`, the resolution is settled —
        // a reader whose clock still sits before expires_at must not flip it
        // back to pending.
        let mut g = open("g".into(), "q?", "", yes_no(), "no", 600, t0()).unwrap();
        g.state = "expired".into();
        let record = serde_json::to_value(&g).unwrap();
        let r = resolve(&record, t0() + chrono::Duration::seconds(10)).unwrap();
        assert_eq!(
            r,
            Resolution::Expired {
                choice: "no".into()
            }
        );
    }

    #[test]
    fn a_malformed_record_is_an_error_in_words_not_a_panic() {
        let r = resolve(&json!({ "id": "g" }), t0());
        assert!(r.unwrap_err().contains("not a well-formed gate"));
        let mut g = open("g".into(), "q?", "", yes_no(), "no", 60, t0()).unwrap();
        g.expires_at = "not-a-time".into();
        let r = resolve(&serde_json::to_value(&g).unwrap(), t0());
        assert!(r.unwrap_err().contains("expires_at"));
    }
}
