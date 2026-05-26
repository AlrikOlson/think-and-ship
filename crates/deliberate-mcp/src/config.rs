//! Runtime configuration, mirroring the original TypeScript `loadConfig` semantics.

use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Console,
    Json,
    Markdown,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Console => "console",
            OutputFormat::Json => "json",
            OutputFormat::Markdown => "markdown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "console" => Some(OutputFormat::Console),
            "json" => Some(OutputFormat::Json),
            "markdown" => Some(OutputFormat::Markdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ValidationConfig {
    pub require_thought_prefix: bool,
    pub require_rationale_prefix: bool,
    pub allow_custom_purpose: bool,
    pub strict_mode: bool,
}

#[derive(Debug, Clone)]
pub struct FeaturesConfig {
    pub enable_revisions: bool,
    pub enable_branching: bool,
    pub enable_confidence: bool,
    pub enable_structured_actions: bool,
    pub enable_sessions: bool,
    /// When set, the server fills in this `session_id` on any incoming
    /// step that doesn't carry one. Combined with the on-disk session
    /// file naming, this lets each MCP-server-process spawn show up as
    /// its own session in the viewer without the agent having to
    /// remember to pass a `session_id` on every call.
    ///
    /// Resolved in this order at startup:
    ///   1. `DELIBERATE_DEFAULT_SESSION_ID` env var (explicit override)
    ///   2. `DELIBERATE_AUTO_SESSION=true` → generated `auto-YYYYMMDD-HHMMSS-XXXX`
    ///   3. None (caller must pass `session_id` explicitly to use a session)
    ///
    /// Setting either env implies `enable_sessions = true`.
    pub default_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct DisplayConfig {
    pub color_output: bool,
    pub output_format: OutputFormat,
}

#[derive(Debug, Clone, Copy)]
pub struct SystemConfig {
    pub max_history_size: usize,
    pub max_branch_depth: u32,
    /// Session inactivity timeout, in minutes.
    pub session_timeout: u64,
    /// How many prior steps to include in each step response's `recent_steps`
    /// rollup. Higher values keep more orientation in-band at the cost of tokens.
    pub recent_steps_limit: usize,
}

#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    /// When true, sessions and the default history are loaded from disk on
    /// startup and written after every mutation. Default false (in-memory only).
    pub enabled: bool,
    /// Directory where session files live. Defaults to
    /// `${XDG_DATA_HOME:-$HOME/.local/share}/deliberate-mcp`.
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct BroadcastConfig {
    /// When set, the server binds a Unix domain socket here and emits a
    /// newline-delimited JSON frame for every trace mutation. Passive
    /// observers (the desktop viewer, log scrapers) subscribe by
    /// connecting. Unset = no broadcast, no socket, no overhead.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct DeliberateConfig {
    pub validation: ValidationConfig,
    pub features: FeaturesConfig,
    pub display: DisplayConfig,
    pub system: SystemConfig,
    pub persistence: PersistenceConfig,
    pub broadcast: BroadcastConfig,
}

impl Default for DeliberateConfig {
    fn default() -> Self {
        Self {
            validation: ValidationConfig {
                require_thought_prefix: false,
                require_rationale_prefix: false,
                allow_custom_purpose: true,
                strict_mode: false,
            },
            features: FeaturesConfig {
                enable_revisions: true,
                enable_branching: true,
                enable_confidence: true,
                enable_structured_actions: true,
                enable_sessions: false,
                default_session_id: None,
            },
            display: DisplayConfig {
                color_output: true,
                output_format: OutputFormat::Console,
            },
            system: SystemConfig {
                max_history_size: 100,
                max_branch_depth: 5,
                session_timeout: 60,
                recent_steps_limit: 3,
            },
            persistence: PersistenceConfig {
                enabled: false,
                data_dir: default_data_dir(),
            },
            broadcast: BroadcastConfig { path: None },
        }
    }
}

/// Resolve the default data dir following the XDG Base Directory spec, with
/// a sensible fallback for systems that don't set `XDG_DATA_HOME`.
fn default_data_dir() -> PathBuf {
    if let Ok(custom) = env::var("DELIBERATE_DATA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("deliberate-mcp");
        }
    }
    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("deliberate-mcp");
        }
    }
    // Last-resort fallback for environments without HOME (e.g. some CI). The
    // engine still works in memory-only mode; persistence just won't load.
    env::temp_dir().join("deliberate-mcp")
}

/// Parse a non-empty env value as an integer `>= min`, returning `None` otherwise.
fn parse_int_env<T: std::str::FromStr + PartialOrd + Copy>(
    value: Option<&str>,
    min: T,
) -> Option<T> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = raw.parse::<T>().ok()?;
    if parsed < min { None } else { Some(parsed) }
}

/// Build a config from `Default` and the env, matching the TS `loadConfig` behavior.
pub fn load_config() -> DeliberateConfig {
    let mut config = DeliberateConfig::default();

    if env::var("DELIBERATE_STRICT_MODE").as_deref() == Ok("true") {
        config.validation.strict_mode = true;
        config.validation.require_thought_prefix = true;
        config.validation.require_rationale_prefix = true;
        config.validation.allow_custom_purpose = false;
    }

    if let Some(v) = parse_int_env::<usize>(env::var("MAX_HISTORY_SIZE").ok().as_deref(), 1) {
        config.system.max_history_size = v;
    }

    if let Ok(raw) = env::var("DELIBERATE_OUTPUT_FORMAT") {
        match OutputFormat::parse(&raw) {
            Some(fmt) => config.display.output_format = fmt,
            None => {
                eprintln!(
                    "⚠️ Invalid DELIBERATE_OUTPUT_FORMAT '{raw}', using default 'console'. Valid options: console, json, markdown"
                );
            }
        }
    }

    if env::var("DELIBERATE_NO_COLOR").as_deref() == Ok("true") {
        config.display.color_output = false;
    }

    if let Some(v) = parse_int_env::<u64>(env::var("DELIBERATE_SESSION_TIMEOUT").ok().as_deref(), 1) {
        config.system.session_timeout = v;
    }

    if let Some(v) = parse_int_env::<u32>(env::var("DELIBERATE_MAX_BRANCH_DEPTH").ok().as_deref(), 1) {
        config.system.max_branch_depth = v;
    }

    if env::var("DELIBERATE_ENABLE_SESSIONS").as_deref() == Ok("true") {
        config.features.enable_sessions = true;
    }

    if let Some(v) =
        parse_int_env::<usize>(env::var("DELIBERATE_RECENT_STEPS_LIMIT").ok().as_deref(), 1)
    {
        config.system.recent_steps_limit = v;
    }

    if env::var("DELIBERATE_PERSIST").as_deref() == Ok("true") {
        config.persistence.enabled = true;
    }
    // `default_data_dir()` already honors DELIBERATE_DATA_DIR — re-evaluate here so
    // a test that sets the env after Default::default() runs picks it up.
    config.persistence.data_dir = default_data_dir();

    if let Ok(raw) = env::var("DELIBERATE_BROADCAST_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            config.broadcast.path = Some(PathBuf::from(trimmed));
        }
    }

    // Project-derived default session is now always-on whenever sessions
    // are enabled. The previous opt-in (`DELIBERATE_AUTO_SESSION=true`)
    // produced silent data corruption: agents in projects without that
    // env wrote to a shared `_default.json` regardless of cwd. We keep
    // the env var accepted for back-compat (still enables sessions) but
    // no longer require it for the default to apply.
    if env::var("DELIBERATE_AUTO_SESSION").as_deref() == Ok("true") {
        config.features.enable_sessions = true;
    }
    if config.features.enable_sessions && config.features.default_session_id.is_none() {
        config.features.default_session_id = Some(generate_auto_session_id());
    }
    if let Ok(raw) = env::var("DELIBERATE_DEFAULT_SESSION_ID") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // Leave whatever auto-session produced in place.
        } else if !is_safe_session_id(trimmed) {
            eprintln!(
                "⚠️ Ignoring DELIBERATE_DEFAULT_SESSION_ID={trimmed:?} — must match [A-Za-z0-9_.-], ≤128 chars"
            );
        } else {
            config.features.enable_sessions = true;
            config.features.default_session_id = Some(trimmed.to_string());
        }
    }

    config
}

/// Generate the default session_id at server startup. Format is just
/// the resolved project identifier — `<basename>-<6hex>` from the cwd
/// path, or a sanitized `DELIBERATE_PROJECT_NAME` override. NO timestamp,
/// NO random suffix.
///
/// The original design appended a timestamp+random suffix so each
/// server spawn got its own session. That fragmented every project
/// into N sessions over time and broke persistence from the agent's
/// point of view — a fresh conversation couldn't see its own past
/// reasoning. The corrected design uses one stable id per project,
/// loaded on every spawn, so the trace accumulates across all
/// conversations a user ever has in that project.
fn generate_auto_session_id() -> String {
    resolve_project_id()
}

/// Separator between the project id and a caller-supplied custom name.
/// Two underscores so it can't be confused with a single `_` inside an
/// otherwise-bare custom name (e.g. `my_session`).
pub const PROJECT_SEP: &str = "__";

/// Canonical project identifier for the current process. Delegates to
/// `think_and_ship_core::resolve_project_id` so all servers in the
/// workspace produce the same identity for the same working directory.
pub fn resolve_project_id() -> String {
    think_and_ship_core::resolve_project_id(None)
}

/// Rewrite a caller-supplied session id so it lands inside the current
/// project's namespace on disk. Idempotent: passing an already-prefixed
/// id (whether `<project>__<rest>` or the legacy `<project>-<…>` form
/// emitted by older auto-rotation) returns the input unchanged. The
/// final output is always valid against `is_safe_session_id`.
pub fn namespace_session_id(project_id: &str, raw: &str) -> String {
    let raw = raw.trim();
    // Empty / equal-to-project ⇒ the bare project session.
    if raw.is_empty() || raw == project_id {
        return project_id.to_string();
    }
    // Already namespaced under this project (idempotent).
    let sep_prefix = format!("{project_id}{PROJECT_SEP}");
    if raw.starts_with(&sep_prefix) {
        return raw.to_string();
    }
    // Legacy auto-rotation: `<project>-YYYYMMDD-HHMMSS-<4>`. Keep as-is.
    let legacy_prefix = format!("{project_id}-");
    if raw.starts_with(&legacy_prefix) {
        return raw.to_string();
    }
    // Otherwise prefix and re-sanitize. The combined id is clamped to
    // the safe-id length (128 chars) by truncating the suffix.
    let mut combined = sep_prefix;
    combined.push_str(raw);
    if combined.len() > 128 {
        combined.truncate(128);
        // Don't leave a dangling separator after truncation.
        while combined.ends_with('-') || combined.ends_with('.') {
            combined.pop();
        }
    }
    combined
}


/// Same allowlist as persistence::is_safe_session_id. Kept here so the
/// config layer can validate before passing values downstream.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && id != "."
        && id != ".."
}
