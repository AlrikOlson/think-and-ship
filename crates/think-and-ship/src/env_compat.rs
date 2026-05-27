//! Legacy environment variable translation.
//!
//! Accepts the `DELIBERATE_*` and `RESOLUTE_*` vars that v0.1.x deployments
//! still set, and maps each to its `THINK_AND_SHIP_*` equivalent at startup.
//! When both are set, the new var wins and the legacy var is ignored.
//! Each legacy var seen produces a single `tracing::warn` line so operators
//! know to update their MCP config.

use std::env;

use tracing::warn;

/// Translation table: every legacy env var the unified server still
/// honors, paired with the canonical `THINK_AND_SHIP_*` name it maps to.
/// Order is deterministic and matches the architecture doc.
const LEGACY_MAPPINGS: &[(&str, &str)] = &[
    // think (deliberate-mcp) family
    ("DELIBERATE_PERSIST", "THINK_AND_SHIP_PERSIST"),
    ("DELIBERATE_DATA_DIR", "THINK_AND_SHIP_DATA_DIR"),
    ("DELIBERATE_BROADCAST_PATH", "THINK_AND_SHIP_BROADCAST_PATH"),
    ("DELIBERATE_PROJECT_NAME", "THINK_AND_SHIP_PROJECT_NAME"),
    ("DELIBERATE_AUTO_SESSION", "THINK_AND_SHIP_AUTO_SESSION"),
    ("DELIBERATE_DEFAULT_SESSION_ID", "THINK_AND_SHIP_DEFAULT_SESSION_ID"),
    // ship (resolute-mcp) family
    ("RESOLUTE_PERSIST", "THINK_AND_SHIP_PERSIST"),
    ("RESOLUTE_DATA_DIR", "THINK_AND_SHIP_DATA_DIR"),
    ("RESOLUTE_BROADCAST_PATH", "THINK_AND_SHIP_BROADCAST_PATH"),
    ("RESOLUTE_PROJECT_NAME", "THINK_AND_SHIP_PROJECT_NAME"),
];

/// Apply the translation table to the process environment. Returns the
/// list of legacy var names that were translated (for tests / logging).
pub fn translate_legacy_env_vars() -> Vec<&'static str> {
    let mut translated = Vec::new();
    for (legacy, canonical) in LEGACY_MAPPINGS {
        let Ok(value) = env::var(legacy) else {
            continue;
        };
        if env::var(canonical).is_ok() {
            warn!(
                "legacy env var {legacy} ignored: {canonical} is already set"
            );
            continue;
        }
        warn!("legacy env var {legacy} mapped to {canonical} (deprecated; will stop working in v0.3.0)");
        // SAFETY: this runs once at startup, before any worker threads
        // are spawned. The Rust 2024 contract for `env::set_var` requires
        // serial access; serve() invokes this before spawning the rmcp
        // server.
        unsafe { env::set_var(canonical, &value) };
        translated.push(*legacy);
    }
    translated
}

#[cfg(test)]
mod tests {
    use super::*;

    // SAFETY: each test mutates the process env. They all touch a different
    // legacy var so they don't race on the same key, but cargo test runs
    // them in parallel by default — combining the cases into one sequential
    // test eliminates the race.

    #[test]
    fn translate_maps_legacy_var_when_canonical_unset() {
        unsafe {
            env::remove_var("THINK_AND_SHIP_PERSIST");
            env::set_var("DELIBERATE_PERSIST", "true");
        }
        let translated = translate_legacy_env_vars();
        assert!(
            translated.contains(&"DELIBERATE_PERSIST"),
            "expected DELIBERATE_PERSIST to be translated, got {translated:?}"
        );
        assert_eq!(env::var("THINK_AND_SHIP_PERSIST").as_deref(), Ok("true"));
        unsafe {
            env::remove_var("DELIBERATE_PERSIST");
            env::remove_var("THINK_AND_SHIP_PERSIST");
        }
    }

    #[test]
    fn translate_skips_when_canonical_already_set() {
        unsafe {
            env::set_var("THINK_AND_SHIP_DATA_DIR", "/canonical");
            env::set_var("DELIBERATE_DATA_DIR", "/legacy");
        }
        let translated = translate_legacy_env_vars();
        assert!(
            !translated.contains(&"DELIBERATE_DATA_DIR"),
            "DELIBERATE_DATA_DIR should be ignored when canonical is set"
        );
        // Canonical value unchanged.
        assert_eq!(env::var("THINK_AND_SHIP_DATA_DIR").as_deref(), Ok("/canonical"));
        unsafe {
            env::remove_var("DELIBERATE_DATA_DIR");
            env::remove_var("THINK_AND_SHIP_DATA_DIR");
        }
    }

    #[test]
    fn translate_is_noop_when_no_legacy_set() {
        // Ensure no legacy vars from other tests leaked in.
        for (legacy, _) in LEGACY_MAPPINGS {
            unsafe { env::remove_var(legacy) };
        }
        let translated = translate_legacy_env_vars();
        assert!(translated.is_empty());
    }
}
