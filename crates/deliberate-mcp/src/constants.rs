//! Validation constants and lookup tables.

pub const VALID_PREFIXES: &[&str] = &[
    "OK, I ",
    "But ",
    "Wait ",
    "Therefore ",
    "I see the issue now. ",
    "I have completed ",
];

pub const VALID_PURPOSES: &[&str] = &[
    "analysis",
    "action",
    "reflection",
    "decision",
    "summary",
    "validation",
    "exploration",
    "hypothesis",
    "correction",
    "planning",
];

pub const COMPLETION_PHRASES: &[&str] = &["i have completed", "task completed", "solution found"];

pub const REQUIRED_STEP_FIELDS: &[&str] = &[
    "step_number",
    "estimated_total",
    "purpose",
    "context",
    "thought",
    "outcome",
    "next_action",
    "rationale",
];

pub const CONFIDENCE_MIN: f64 = 0.0;
pub const CONFIDENCE_MAX: f64 = 1.0;
pub const LOW_CONFIDENCE_THRESHOLD: f64 = 0.5;

/// Number of `process_step` calls between batched session-expiry sweeps.
pub const SESSION_CLEANUP_INTERVAL: u32 = 10;

/// Check whether `purpose` (case-insensitive) is one of the standard purposes.
pub fn is_valid_purpose(purpose: &str) -> bool {
    let lower = purpose.to_ascii_lowercase();
    VALID_PURPOSES.iter().any(|p| *p == lower)
}
