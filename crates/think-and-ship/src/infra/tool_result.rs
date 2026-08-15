//! Cascade-safe MCP tool-result construction.
//!
//! Claude Code cancels *every sibling tool call in a parallel batch* when one
//! of them errors (anthropics/claude-code#22264). A tool reports an error two
//! ways: a JSON-RPC `-32602` (killed at the `infra::coerce` layer) or a
//! `CallToolResult` with `is_error: true` — which is what `rmcp`'s
//! `CallToolResult::structured_error` sets. So a perfectly ordinary logical
//! failure ("no chunk with that id") becomes the errored sibling that wipes a
//! whole batch.
//!
//! This module is the single home (SRP) for the rule: **a tool result is never
//! marked `is_error: true`.** A logical failure is returned as a *successful*
//! result whose payload loudly says it failed —
//! `{ ok: false, error_kind, message }` in `structured_content`, and a
//! `"⚠ <kind>: <message>"` line in the text content so a human/agent reading
//! prose can't miss it. The tool *succeeded at telling the caller the request
//! was invalid*; it produced no errored sibling, so nothing cascades. The
//! caller pattern-matches `ok`/`error_kind` exactly as before.
//!
//! Open/Closed: services delegate their `err_structured`/`structured_err`
//! helpers here instead of each re-deriving the envelope, so the guarantee
//! holds uniformly and new tools inherit it for free.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};

/// A successful tool result carrying structured JSON (mirrors
/// `CallToolResult::structured`, kept here so call sites have one import).
#[must_use]
pub fn ok(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}

/// A logical failure that is **not** an MCP error: `is_error` stays `false`,
/// so it can never be the errored sibling that cancels a parallel batch.
///
/// The payload is loud in both channels:
/// - `structured_content`: `{ "ok": false, "error_kind": kind, "message": msg }`
///   for callers that parse the tool's output schema.
/// - text content: `"⚠ <kind>: <msg>"` so prose readers see the failure too.
#[must_use]
pub fn soft_error(error_kind: &str, message: impl Into<String>) -> CallToolResult {
    let message = message.into();
    let structured = json!({
        "ok": false,
        "error_kind": error_kind,
        "message": message,
    });
    // `CallToolResult` is #[non_exhaustive], so build via the constructor
    // (which sets is_error:false) and replace the default text with our loud
    // "⚠ kind: message" line. The is_error:false is the load-bearing bit.
    let mut result = CallToolResult::structured(structured);
    result.content = vec![ContentBlock::text(format!("⚠ {error_kind}: {message}"))];
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_error_is_not_an_mcp_error() {
        let r = soft_error("not_found", "no chunk 'windows-support'");
        // The load-bearing invariant: never is_error == true.
        assert_eq!(r.is_error, Some(false));
    }

    #[test]
    fn soft_error_carries_machine_readable_envelope() {
        let r = soft_error("invalid_args", "id is required");
        let sc = r.structured_content.expect("structured_content present");
        assert_eq!(sc["ok"], json!(false));
        assert_eq!(sc["error_kind"], json!("invalid_args"));
        assert_eq!(sc["message"], json!("id is required"));
    }

    #[test]
    fn soft_error_is_loud_in_text_channel() {
        let r = soft_error("blocked", "deps unsatisfied");
        let text = match &r.content[0] {
            rmcp::model::ContentBlock::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("blocked"));
        assert!(text.contains("deps unsatisfied"));
    }

    #[test]
    fn ok_is_a_success_result() {
        let r = ok(json!({"value": 1}));
        assert_eq!(r.is_error, Some(false));
        assert_eq!(r.structured_content.unwrap()["value"], json!(1));
    }
}
