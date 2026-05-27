//! Recovery from the harness's XML-injection failure mode.
//!
//! Claude Code's tool-call wire format is XML-shaped. When an agent writes
//! literal tool-call markup (`</thought>`, `<parameter name=...>`, etc.)
//! inside a parameter value, the harness silently closes that parameter
//! early and the intended siblings get dropped before they reach us.
//!
//! Two extractors hunt for the lost content:
//!
//! * [`extract_injected_parameters`] catches the literal Claude Code wire
//!   form, `<parameter name="X">VALUE</parameter>`.
//! * [`extract_bare_field_tags`] catches the empirically-dominant form
//!   agents actually produce — bare `<outcome>VALUE</outcome>` tags used
//!   as section headers inside `thought`.
//!
//! Both tolerate unclosed tags at EOF (the agent's value got truncated
//! mid-string). After extraction succeeds,
//! [`truncate_at_markup`] cleans up the source field so the embedded
//! markup doesn't persist as noise in the recorded trace.

/// Field names the recovery layer knows how to fill from extracted
/// `<name>VALUE</name>` pairs. Kept here next to the extractor so the
/// allowlist is in one place.
pub(crate) const RECOVERABLE_FIELD_NAMES: &[&str] = &[
    "thought",
    "outcome",
    "rationale",
    "next_action",
    "context",
    "purpose",
    "confidence",
    "uncertainty_notes",
    "dependencies",
    "tools_used",
    "pinned",
    "is_final_step",
    "branch_id",
    "branch_name",
    "branch_from",
    "revises_step",
    "revision_reason",
    "session_id",
];

/// Markers that indicate the harness's tool-call format leaked into a
/// parameter value. When present in `thought` (or another text field),
/// they're the canonical "I serialized the rest of my tool call as text
/// inside this field" signature — see [`extract_bare_field_tags`].
pub(crate) const TRUNCATION_MARKERS: &[&str] = &["</thought>", "</invoke>", "</parameter>"];

/// Manual byte-level scan for embedded `<parameter name="X">VALUE</parameter>`
/// patterns inside a string. Used by the engine's recovery method to fish
/// the agent's intended siblings out of a corrupted thought.
///
/// Deliberately tolerant: missing `>` or unclosed sections just stop the
/// current capture and move on. Returns `(name, value)` pairs in document
/// order; duplicates within the same source are preserved so callers can
/// decide first-vs-last-wins semantics. The engine uses first-wins (only
/// fills empty fields), so duplicates after the first are harmless.
pub(crate) fn extract_injected_parameters(text: &str) -> Vec<(String, String)> {
    const OPEN: &str = "<parameter name=";
    const CLOSE: &str = "</parameter>";
    let mut out: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let Some(start_rel) = text[pos..].find(OPEN) else {
            break;
        };
        let after_open = pos + start_rel + OPEN.len();
        // Accept either `"NAME"` or `'NAME'` quoting around the name —
        // some agents quote with single quotes when they're trying to
        // avoid escaping issues.
        let quote = text.as_bytes().get(after_open).copied();
        let (name_start, name_end_marker): (usize, char) = match quote {
            Some(b'"') => (after_open + 1, '"'),
            Some(b'\'') => (after_open + 1, '\''),
            _ => {
                pos = after_open;
                continue;
            }
        };
        let Some(name_end_rel) = text[name_start..].find(name_end_marker) else {
            break;
        };
        let name = text[name_start..name_start + name_end_rel].to_string();
        let after_name = name_start + name_end_rel + 1;
        // Skip the `>` after the closing quote.
        let value_start = match text.as_bytes().get(after_name) {
            Some(b'>') => after_name + 1,
            _ => {
                pos = after_name;
                continue;
            }
        };
        let (value_end_abs, advance_past) = match text[value_start..].find(CLOSE) {
            Some(end_rel) => (value_start + end_rel, value_start + end_rel + CLOSE.len()),
            // Unclosed parameter: empirically common when the agent's
            // output got truncated mid-value. Take everything to end of
            // text — better to recover a partial value than nothing.
            None => (text.len(), text.len()),
        };
        let value = text[value_start..value_end_abs].to_string();
        out.push((name, value));
        pos = advance_past;
    }
    out
}

/// Second-pattern extractor: catches the empirically-observed failure
/// mode where the agent puts sibling parameters inside `thought` as
/// bare `<outcome>...</outcome>` / `<rationale>...</rationale>` tags.
/// Only fires AFTER a truncation marker so an agent legitimately quoting
/// markup in prose — without the spillover signature — won't have content
/// false-positive-extracted.
///
/// Returns `(name, value)` pairs in document order for any opening tag
/// matching a known field name (see [`RECOVERABLE_FIELD_NAMES`]). The
/// engine's dispatch is first-wins on empty fields and ignores duplicates.
pub(crate) fn extract_bare_field_tags(text: &str) -> Vec<(String, String)> {
    // Find the earliest truncation marker. Only scan content after it;
    // before the marker is the "real" surviving value.
    let mut scan_start: Option<usize> = None;
    for m in TRUNCATION_MARKERS {
        if let Some(idx) = text.find(m) {
            let after = idx + m.len();
            scan_start = Some(match scan_start {
                Some(prev) => prev.min(after),
                None => after,
            });
        }
    }
    let Some(start) = scan_start else {
        return Vec::new();
    };
    let scan = &text[start..];

    let mut out: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    while pos < scan.len() {
        let Some(lt_rel) = scan[pos..].find('<') else {
            break;
        };
        let lt = pos + lt_rel;
        // Skip closing tags — they can only END a value, not open one.
        if scan.as_bytes().get(lt + 1) == Some(&b'/') {
            pos = lt + 2;
            continue;
        }
        // Find the matching `>` for the opening tag. Only accept a
        // bare `<name>` form (no attributes) so we don't accidentally
        // extract from `<parameter name="X">` — that's handled by the
        // primary extractor.
        let Some(gt_rel) = scan[lt + 1..].find('>') else {
            break;
        };
        let gt = lt + 1 + gt_rel;
        let name = &scan[lt + 1..gt];
        if name.is_empty() || !RECOVERABLE_FIELD_NAMES.contains(&name) {
            pos = gt + 1;
            continue;
        }
        let close = format!("</{name}>");
        let (value_end_abs, advance_past) = match scan[gt + 1..].find(&close) {
            Some(close_rel) => (gt + 1 + close_rel, gt + 1 + close_rel + close.len()),
            // Unclosed tag at EOF — capture everything to end of scan.
            // Same rationale as the primary extractor: agent's value got
            // cut off mid-string; partial recovery beats none.
            None => (scan.len(), scan.len()),
        };
        let value = scan[gt + 1..value_end_abs].to_string();
        out.push((name.to_string(), value));
        pos = advance_past;
    }
    out
}

/// Cut a string off at the first occurrence of any tool-call markup
/// marker. Used after extraction succeeds so the recovered source field
/// doesn't keep the embedded markup as garbage.
pub(crate) fn truncate_at_markup(text: &str) -> String {
    const MARKERS: &[&str] = &[
        "</thought>",
        "</parameter>",
        "<parameter name=",
        "</invoke>",
        "<invoke name=",
    ];
    let mut earliest = text.len();
    for m in MARKERS {
        if let Some(idx) = text.find(m) {
            if idx < earliest {
                earliest = idx;
            }
        }
    }
    text[..earliest].trim_end().to_string()
}
