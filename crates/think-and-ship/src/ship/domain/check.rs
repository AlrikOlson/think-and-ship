use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    Test,
    Lint,
    Typecheck,
    Build,
    Review,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    #[serde(rename = "type")]
    pub check_type: CheckType,
    pub name: String,
    pub passed: bool,
    pub details: String,
    pub required: bool,
    /// `true` when `passed` was derived from a command the server actually ran
    /// (not self-reported by the agent). A fabricated green check — the
    /// observed failure mode: a non-compiling change once shipped with a
    /// self-reported "24 tests pass" — is
    /// impossible for a verified check.
    #[serde(default)]
    pub verified: bool,
    /// The command the server executed for a verified check, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Exit code of the executed command (None if it couldn't be spawned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// What happened with the machine-readable report the caller asked for,
    /// when one was asked for. Present even when parsing failed, so the
    /// record says explicitly that parsing did not happen — a parse failure
    /// never fails the check and never flips `verified` (see
    /// [`crate::ship::report`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<crate::ship::report::ReportRecord>,
    /// The structured summary parsed from that report. Additive detail: the
    /// exit code stays the source of truth for `passed`/`verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<crate::ship::report::TestResults>,
    pub timestamp: String,
}
