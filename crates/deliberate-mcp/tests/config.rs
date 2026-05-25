//! Mirrors `tests/config.test.ts`. Env tests run serially because they mutate
//! shared process state.

use std::sync::Mutex;

use deliberate_mcp::config::{
    DeliberateConfig, OutputFormat, PROJECT_SEP, load_config, namespace_session_id,
    resolve_project_id,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    keys: Vec<&'static str>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        // Clear all env keys this module touches so each test starts clean.
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = Self {
            keys: Vec::new(),
            _lock: lock,
        };
        for key in [
            "DELIBERATE_STRICT_MODE",
            "MAX_HISTORY_SIZE",
            "DELIBERATE_OUTPUT_FORMAT",
            "DELIBERATE_NO_COLOR",
            "DELIBERATE_SESSION_TIMEOUT",
            "DELIBERATE_MAX_BRANCH_DEPTH",
            "DELIBERATE_ENABLE_SESSIONS",
            "DELIBERATE_AUTO_SESSION",
            "DELIBERATE_DEFAULT_SESSION_ID",
            "DELIBERATE_PROJECT_NAME",
        ] {
            // SAFETY: tests are single-threaded thanks to ENV_LOCK.
            unsafe { std::env::remove_var(key) };
            guard.keys.push(key);
        }
        guard
    }

    fn set(&mut self, key: &'static str, value: &str) {
        if !self.keys.contains(&key) {
            self.keys.push(key);
        }
        // SAFETY: tests are single-threaded thanks to ENV_LOCK.
        unsafe { std::env::set_var(key, value) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for key in &self.keys {
            // SAFETY: tests are single-threaded thanks to ENV_LOCK.
            unsafe { std::env::remove_var(key) };
        }
    }
}

#[test]
fn default_config_is_flexible() {
    let _g = EnvGuard::new();
    let c = DeliberateConfig::default();
    assert!(!c.validation.strict_mode);
    assert!(!c.validation.require_thought_prefix);
    assert!(!c.validation.require_rationale_prefix);
    assert!(c.validation.allow_custom_purpose);
}

#[test]
fn default_features_enabled() {
    let _g = EnvGuard::new();
    let c = DeliberateConfig::default();
    assert!(c.features.enable_revisions);
    assert!(c.features.enable_branching);
    assert!(c.features.enable_confidence);
    assert!(c.features.enable_structured_actions);
}

#[test]
fn default_sessions_disabled() {
    let _g = EnvGuard::new();
    let c = DeliberateConfig::default();
    assert!(!c.features.enable_sessions);
}

#[test]
fn default_system_values() {
    let _g = EnvGuard::new();
    let c = DeliberateConfig::default();
    assert_eq!(c.system.max_history_size, 100);
    assert_eq!(c.system.max_branch_depth, 5);
    assert_eq!(c.system.session_timeout, 60);
}

#[test]
fn load_config_with_no_env_matches_default() {
    let _g = EnvGuard::new();
    let c = load_config();
    let d = DeliberateConfig::default();
    assert_eq!(c.validation.strict_mode, d.validation.strict_mode);
    assert_eq!(c.system.max_history_size, d.system.max_history_size);
    assert_eq!(c.display.output_format, d.display.output_format);
    assert_eq!(c.display.color_output, d.display.color_output);
}

#[test]
fn strict_mode_enables_all_strict_validations() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_STRICT_MODE", "true");
    let c = load_config();
    assert!(c.validation.strict_mode);
    assert!(c.validation.require_thought_prefix);
    assert!(c.validation.require_rationale_prefix);
    assert!(!c.validation.allow_custom_purpose);
}

#[test]
fn strict_mode_false_keeps_flexible() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_STRICT_MODE", "false");
    let c = load_config();
    assert!(!c.validation.strict_mode);
}

#[test]
fn max_history_size_from_env() {
    let mut g = EnvGuard::new();
    g.set("MAX_HISTORY_SIZE", "50");
    assert_eq!(load_config().system.max_history_size, 50);
}

#[test]
fn max_history_size_large_value() {
    let mut g = EnvGuard::new();
    g.set("MAX_HISTORY_SIZE", "1000");
    assert_eq!(load_config().system.max_history_size, 1000);
}

#[test]
fn max_history_size_nan_falls_back_to_default() {
    let mut g = EnvGuard::new();
    g.set("MAX_HISTORY_SIZE", "abc");
    assert_eq!(load_config().system.max_history_size, 100);
}

#[test]
fn max_history_size_zero_falls_back_to_default() {
    let mut g = EnvGuard::new();
    g.set("MAX_HISTORY_SIZE", "0");
    assert_eq!(load_config().system.max_history_size, 100);
}

#[test]
fn max_history_size_negative_falls_back_to_default() {
    let mut g = EnvGuard::new();
    g.set("MAX_HISTORY_SIZE", "-5");
    assert_eq!(load_config().system.max_history_size, 100);
}

#[test]
fn output_format_json() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_OUTPUT_FORMAT", "json");
    assert_eq!(load_config().display.output_format, OutputFormat::Json);
}

#[test]
fn output_format_markdown() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_OUTPUT_FORMAT", "markdown");
    assert_eq!(load_config().display.output_format, OutputFormat::Markdown);
}

#[test]
fn output_format_console() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_OUTPUT_FORMAT", "console");
    assert_eq!(load_config().display.output_format, OutputFormat::Console);
}

#[test]
fn output_format_invalid_falls_back_to_console() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_OUTPUT_FORMAT", "invalid");
    assert_eq!(load_config().display.output_format, OutputFormat::Console);
}

#[test]
fn output_format_case_insensitive() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_OUTPUT_FORMAT", "JSON");
    assert_eq!(load_config().display.output_format, OutputFormat::Json);
}

#[test]
fn no_color_disables_colors() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_NO_COLOR", "true");
    assert!(!load_config().display.color_output);
}

#[test]
fn colors_enabled_by_default() {
    let _g = EnvGuard::new();
    assert!(load_config().display.color_output);
}

#[test]
fn session_timeout_from_env() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_SESSION_TIMEOUT", "30");
    assert_eq!(load_config().system.session_timeout, 30);
}

#[test]
fn max_branch_depth_from_env() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_MAX_BRANCH_DEPTH", "3");
    assert_eq!(load_config().system.max_branch_depth, 3);
}

#[test]
fn sessions_enabled_via_env() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_ENABLE_SESSIONS", "true");
    assert!(load_config().features.enable_sessions);
}

#[test]
fn sessions_false_keeps_disabled() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_ENABLE_SESSIONS", "false");
    assert!(!load_config().features.enable_sessions);
}

#[test]
fn sessions_disabled_when_unset() {
    let _g = EnvGuard::new();
    assert!(!load_config().features.enable_sessions);
}

#[test]
fn auto_session_id_is_stable_basename_plus_path_hash() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_AUTO_SESSION", "true");
    let c = load_config();
    let id = c
        .features
        .default_session_id
        .expect("auto-session should produce a default id");
    let cwd = std::env::current_dir().unwrap();
    let path = cwd.canonicalize().unwrap_or(cwd);
    let basename = path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_lowercase();
    // New format: just `<basename>-<6hex>`. No timestamp, no random
    // suffix — same project always produces the same session id, so
    // history accumulates across all spawns.
    let expected_prefix = format!("{basename}-");
    assert!(
        id.starts_with(&expected_prefix),
        "expected id to start with {expected_prefix:?}, got {id:?}",
    );
    let hash_part = &id[expected_prefix.len()..];
    assert_eq!(hash_part.len(), 6, "expected 6-hex hash suffix, got {id:?}");
    assert!(
        hash_part.chars().all(|c| c.is_ascii_hexdigit()),
        "hash suffix should be hex: {id:?}",
    );
}

#[test]
fn auto_session_id_is_identical_across_load_config_calls() {
    // Two consecutive load_config()s from the same cwd must produce
    // EXACTLY the same id — that's the contract that makes the
    // session persistent across server restarts.
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_AUTO_SESSION", "true");
    let id1 = load_config().features.default_session_id.unwrap();
    let id2 = load_config().features.default_session_id.unwrap();
    assert_eq!(id1, id2, "auto-session id must be stable: {id1} vs {id2}");
}

#[test]
fn auto_session_id_honors_explicit_project_name_env() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_AUTO_SESSION", "true");
    g.set("DELIBERATE_PROJECT_NAME", "Acme/API Service!");
    let c = load_config();
    let id = c.features.default_session_id.expect("auto-session id");
    // Explicit override — used verbatim after sanitization.
    assert_eq!(id, "acme-api-service");
}

#[test]
fn auto_session_id_falls_back_when_project_name_is_empty() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_AUTO_SESSION", "true");
    g.set("DELIBERATE_PROJECT_NAME", "/////");
    let c = load_config();
    let id = c.features.default_session_id.expect("auto-session id");
    let cwd = std::env::current_dir().unwrap();
    let path = cwd.canonicalize().unwrap_or(cwd);
    let basename = path.file_name().unwrap().to_str().unwrap().to_lowercase();
    assert!(
        id.starts_with(&format!("{basename}-")),
        "expected fallback to cwd basename {basename:?}, got {id:?}",
    );
}

#[test]
fn multiple_env_vars_compose() {
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_STRICT_MODE", "true");
    g.set("MAX_HISTORY_SIZE", "200");
    g.set("DELIBERATE_OUTPUT_FORMAT", "markdown");
    g.set("DELIBERATE_SESSION_TIMEOUT", "120");
    g.set("DELIBERATE_MAX_BRANCH_DEPTH", "10");
    g.set("DELIBERATE_ENABLE_SESSIONS", "true");
    let c = load_config();
    assert!(c.validation.strict_mode);
    assert_eq!(c.system.max_history_size, 200);
    assert_eq!(c.display.output_format, OutputFormat::Markdown);
    assert_eq!(c.system.session_timeout, 120);
    assert_eq!(c.system.max_branch_depth, 10);
    assert!(c.features.enable_sessions);
}

// ── Project namespacing (round-3 fix) ─────────────────────────────────────

#[test]
fn enable_sessions_alone_now_produces_default_session_id() {
    // Previously the auto default required `DELIBERATE_AUTO_SESSION=true`.
    // That left projects whose .mcp.json forgot the env writing to a
    // shared `_default.json` across cwds. Now any time sessions are
    // enabled, the project-derived default is filled in.
    let mut g = EnvGuard::new();
    g.set("DELIBERATE_ENABLE_SESSIONS", "true");
    let c = load_config();
    let id = c
        .features
        .default_session_id
        .expect("default session id should be filled in once sessions enabled");
    assert_eq!(id, resolve_project_id());
}

#[test]
fn namespace_session_id_leaves_bare_project_alone() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    assert_eq!(namespace_session_id(&p, &p), p);
}

#[test]
fn namespace_session_id_is_idempotent_on_namespaced_input() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    let once = namespace_session_id(&p, "phase3-chunk1");
    assert_eq!(once, format!("{p}{PROJECT_SEP}phase3-chunk1"));
    let twice = namespace_session_id(&p, &once);
    assert_eq!(once, twice, "double-namespacing should be a no-op");
}

#[test]
fn namespace_session_id_passes_legacy_rotation_through() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    // Legacy auto-rotation files look like `<project>-YYYYMMDD-HHMMSS-XXXX`.
    let legacy = format!("{p}-20260520-035214-lega");
    assert_eq!(namespace_session_id(&p, &legacy), legacy);
}

#[test]
fn namespace_session_id_prefixes_bare_custom_name() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    let resolved = namespace_session_id(&p, "alpha-demo");
    assert_eq!(resolved, format!("{p}{PROJECT_SEP}alpha-demo"));
}

#[test]
fn namespace_session_id_empty_returns_bare_project() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    assert_eq!(namespace_session_id(&p, ""), p);
    assert_eq!(namespace_session_id(&p, "   "), p);
}

#[test]
fn namespace_session_id_clamps_to_128() {
    let _g = EnvGuard::new();
    let p = resolve_project_id();
    // Build a suffix long enough to force truncation regardless of how
    // long the project id happens to be at test time.
    let long = "x".repeat(200);
    let out = namespace_session_id(&p, &long);
    assert!(out.len() <= 128, "expected <=128, got {} ({out:?})", out.len());
    assert!(
        out.starts_with(&format!("{p}{PROJECT_SEP}")),
        "expected project prefix to survive truncation: {out:?}"
    );
}
