//! Read-only MCP resource projections (mcp-resources).
//!
//! The unified server exposes ambient workspace state as MCP resources so
//! clients can READ it without tool-call ceremony:
//!
//!   roadmap://view            the roadmap export (markdown)
//!   decisions://pinned        pinned think steps — the durable conclusions
//!   digest://since/{window}   recent activity (steps + chunk movement)
//!
//! Everything here is a pure projection over engine state — reads never
//! mutate or persist. The digest is a LOCAL projection (the richer
//! buildDigest lives in the cloud backend); resources must work offline.
//! Subscriptions are deliberately absent: the 2026-07-28 MCP release
//! candidate moves to a stateless protocol core, so push semantics wait for
//! the final spec.

use chrono::{DateTime, Duration, Utc};

use crate::roadmap::domain::Chunk;
use crate::think::domain::ThinkStep;

pub(crate) const ROADMAP_URI: &str = "roadmap://view";
pub(crate) const PINNED_URI: &str = "decisions://pinned";
pub(crate) const DIGEST_PREFIX: &str = "digest://since/";
pub(crate) const DIGEST_DEFAULT_URI: &str = "digest://since/24h";
pub(crate) const MARKDOWN: &str = "text/markdown";

/// Parse a digest window suffix: `<n>h` (hours) or `<n>d` (days), n >= 1.
pub(crate) fn parse_window(spec: &str) -> Option<Duration> {
    if !spec.is_ascii() || spec.len() < 2 {
        return None;
    }
    let (num, unit) = spec.split_at(spec.len() - 1);
    let n: i64 = num.parse().ok()?;
    if n < 1 {
        return None;
    }
    match unit {
        "h" => Some(Duration::hours(n)),
        "d" => Some(Duration::days(n)),
        _ => None,
    }
}

fn parse_ts(ts: Option<&str>) -> Option<DateTime<Utc>> {
    ts.and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// Markdown view of every pinned step — the trace's load-bearing conclusions.
pub(crate) fn pinned_markdown(steps: &[ThinkStep]) -> String {
    let mut out = String::from("# Pinned decisions\n");
    let mut count = 0usize;
    for step in steps.iter().filter(|s| s.pinned == Some(true)) {
        count += 1;
        out.push_str(&format!(
            "\n## think:{} — {}\n",
            step.step_number, step.purpose
        ));
        if let Some(ts) = &step.timestamp {
            out.push_str(&format!("_{ts}_\n"));
        }
        if !step.outcome.is_empty() {
            out.push_str(&format!("\n{}\n", step.outcome));
        }
    }
    if count == 0 {
        out.push_str("\n(no pinned steps yet)\n");
    }
    out
}

/// Markdown digest of activity since `now - window`: reasoning steps recorded
/// and roadmap chunks that moved. A compact local "while you were away".
pub(crate) fn digest_markdown(
    steps: &[ThinkStep],
    chunks: &[Chunk],
    now: DateTime<Utc>,
    window: Duration,
) -> String {
    let since = now - window;
    let mut out = format!(
        "# Digest since {} (window: {})\n",
        since.to_rfc3339(),
        human_window(window)
    );

    let recent_steps: Vec<&ThinkStep> = steps
        .iter()
        .filter(|s| parse_ts(s.timestamp.as_deref()).is_some_and(|t| t >= since))
        .collect();
    out.push_str(&format!("\n## Reasoning ({} steps)\n", recent_steps.len()));
    for step in &recent_steps {
        out.push_str(&format!("- think:{} {}\n", step.step_number, step.purpose));
    }

    let moved: Vec<&Chunk> = chunks
        .iter()
        .filter(|c| parse_ts(Some(c.updated_at.as_str())).is_some_and(|t| t >= since))
        .collect();
    out.push_str(&format!("\n## Roadmap movement ({} chunks)\n", moved.len()));
    for chunk in &moved {
        out.push_str(&format!(
            "- [{:?}] {} — {}\n",
            chunk.status, chunk.id, chunk.title
        ));
    }

    if recent_steps.is_empty() && moved.is_empty() {
        out.push_str("\n(quiet — nothing recorded in this window)\n");
    }
    out
}

fn human_window(window: Duration) -> String {
    let hours = window.num_hours();
    if hours % 24 == 0 && hours >= 24 {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(n: u32, ts: &str, pinned: bool) -> ThinkStep {
        // Every field is serde-defaulted; an empty object is the canonical
        // "blank step" without the struct needing a Default impl.
        let mut s: ThinkStep = serde_json::from_str("{}").expect("blank step");
        s.step_number = n;
        s.purpose = format!("purpose {n}");
        s.outcome = format!("outcome {n}");
        s.timestamp = Some(ts.into());
        s.pinned = pinned.then_some(true);
        s
    }

    #[test]
    fn window_parses_hours_and_days_and_rejects_junk() {
        assert_eq!(parse_window("24h"), Some(Duration::hours(24)));
        assert_eq!(parse_window("7d"), Some(Duration::days(7)));
        assert_eq!(parse_window("1h"), Some(Duration::hours(1)));
        for bad in ["", "h", "0h", "-1d", "24x", "abc", "24", "∞h"] {
            assert_eq!(parse_window(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn pinned_view_includes_only_pinned_steps() {
        let steps = vec![
            step(1, "2026-06-01T00:00:00+00:00", true),
            step(2, "2026-06-02T00:00:00+00:00", false),
            step(3, "2026-06-03T00:00:00+00:00", true),
        ];
        let md = pinned_markdown(&steps);
        assert!(md.contains("think:1"));
        assert!(!md.contains("think:2"));
        assert!(md.contains("think:3"));
    }

    #[test]
    fn digest_filters_by_window() {
        let now = DateTime::parse_from_rfc3339("2026-06-10T12:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let steps = vec![
            step(1, "2026-06-01T00:00:00+00:00", false), // outside 24h
            step(2, "2026-06-10T08:00:00+00:00", false), // inside
        ];
        let md = digest_markdown(&steps, &[], now, Duration::hours(24));
        assert!(!md.contains("think:1"));
        assert!(md.contains("think:2"));
        assert!(md.contains("Reasoning (1 steps)"));
    }
}
