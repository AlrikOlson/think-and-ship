//! Default session id resolution.
//!
//! Both tool families share the same default session id so that a fresh
//! conversation in a project sees its own past traces — one stable id per
//! project, not one per server spawn.

use std::env;

use super::project_id::resolve_project_id;

const DEFAULT_SESSION_ID_VAR: &str = "THINK_AND_SHIP_DEFAULT_SESSION_ID";

/// Resolve the default `session_id` at startup.
///
/// Precedence:
///   1. `THINK_AND_SHIP_DEFAULT_SESSION_ID` (explicit override).
///   2. The resolved `project_id` — stable across server spawns so traces
///      accumulate across conversations in the same project.
///
/// An explicit, per-call `session_id` argument on a tool always wins over
/// the default returned here.
pub fn resolve_default_session_id() -> String {
    if let Ok(raw) = env::var(DEFAULT_SESSION_ID_VAR) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    resolve_project_id(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // SAFETY: env mutation in Rust 2024 is unsafe because parallel tests
    // can race on the global env table. Both branches of this test touch
    // the same var, so they're folded into one sequential test rather
    // than relying on cargo-test ordering.
    #[test]
    fn env_var_resolution() {
        unsafe { env::set_var(DEFAULT_SESSION_ID_VAR, "manual-session-id") };
        assert_eq!(resolve_default_session_id(), "manual-session-id");

        unsafe { env::set_var(DEFAULT_SESSION_ID_VAR, "   ") };
        let fallback = resolve_default_session_id();
        assert!(!fallback.is_empty());
        assert_ne!(fallback, "   ");

        unsafe { env::remove_var(DEFAULT_SESSION_ID_VAR) };
        let bare = resolve_default_session_id();
        assert!(!bare.is_empty());
    }
}
