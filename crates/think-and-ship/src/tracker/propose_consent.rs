//! Remembered consent for unattended proposals.
//!
//! `THINK_AND_SHIP_TRACKER_PROPOSE` (shipped in 5ca9aa6) is the
//! machine-readable answer to one question: *may a sweep nobody typed write
//! proposals onto the roadmap?* It is default-off and it works — but an env var
//! is a surface almost nobody sets, which is why a whole later change
//! (f822ca4) existed only to make the switch announce itself.
//!
//! This file is the second way to answer the same question: a human is asked
//! once, in the client they are already looking at, and the answer is
//! remembered here. It does not replace the env var — an explicit env value
//! always wins, because a deployment that sets one is stating an intent that a
//! remembered click must not override.
//!
//! The shape is a deliberate clone of [`crate::telemetry::consent`]: a small
//! JSON document under the data dir, written through the crate's one locking
//! seam, newest-`decided_at`-wins on a concurrent write, and a corrupt or
//! missing file failing **closed**. Undecided means off; unreadable means off.
//! There is no state in which losing this file turns an unattended writer on.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::persistence::locked_merge_write;

/// How the remembered value came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposeConsentSource {
    /// Never asked, or asked and not answered. The default: off.
    Undecided,
    /// A human answered the elicitation prompt.
    Human,
}

/// The remembered decision. The default is OFF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposeConsent {
    pub enabled: bool,
    /// ISO-8601 moment of the human's answer; `None` while undecided.
    pub decided_at: Option<String>,
    pub source: ProposeConsentSource,
}

impl Default for ProposeConsent {
    fn default() -> Self {
        Self {
            enabled: false,
            decided_at: None,
            source: ProposeConsentSource::Undecided,
        }
    }
}

impl ProposeConsent {
    /// Whether a human has answered. Drives the "ask at most once" rule: a
    /// decided consent is never asked about again, in either direction.
    #[must_use]
    pub fn is_decided(&self) -> bool {
        matches!(self.source, ProposeConsentSource::Human)
    }
}

/// Persistence error.
#[derive(Debug, thiserror::Error)]
pub enum ProposeConsentError {
    #[error("propose consent io: {0}")]
    Io(#[from] std::io::Error),
}

fn consent_path(data_dir: &Path) -> PathBuf {
    data_dir.join("tracker").join("propose-consent.json")
}

/// Load the remembered decision. A missing or unreadable file is the default
/// (undecided, off) — never an error, so damage can only fail CLOSED.
#[must_use]
pub fn load(data_dir: &Path) -> ProposeConsent {
    std::fs::read_to_string(consent_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record a human's answer (`now` is ISO-8601). Concurrent writers merge
/// newest-`decided_at`-wins, so a stale answer cannot clobber a newer one.
pub fn record(
    data_dir: &Path,
    enabled: bool,
    now: &str,
) -> Result<ProposeConsent, ProposeConsentError> {
    let state = ProposeConsent {
        enabled,
        decided_at: Some(now.to_string()),
        source: ProposeConsentSource::Human,
    };
    locked_merge_write(&consent_path(data_dir), &state, |ours, disk| {
        if disk.decided_at.as_deref() > ours.decided_at.as_deref() {
            disk
        } else {
            ours.clone()
        }
    })?;
    Ok(load(data_dir))
}

/// The two-source resolution, as a pure function so the DECISION is testable
/// and only the `std::env` read stays untestable (the split
/// `unattended_propose_enabled` already uses).
///
/// An explicit env value **wins in both directions**: a deployment that sets
/// `THINK_AND_SHIP_TRACKER_PROPOSE=0` has stated an intent, and a remembered
/// click from months ago must not quietly re-enable an unattended writer. Only
/// when the env var says nothing does the remembered answer speak — and when
/// neither says anything, the answer is off, exactly as it was before this
/// file existed.
#[must_use]
pub fn resolve(env_raw: Option<&str>, remembered: &ProposeConsent) -> bool {
    match env_explicit(env_raw) {
        Some(explicit) => explicit,
        None => remembered.enabled,
    }
}

/// `Some(value)` only when the env var states something this system recognises.
///
/// Deliberately narrower than a boolean parse: an unparseable value is NOT a
/// `Some(false)` that silences a human's remembered yes, it is a `None` that
/// abstains — while still leaving the whole thing off if nothing else speaks.
/// A typo must not be able to flip a decision in either direction.
#[must_use]
pub fn env_explicit(raw: Option<&str>) -> Option<bool> {
    match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
        Some("1") | Some("true") => Some(true),
        Some("0") | Some("false") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_is_off_and_undecided() {
        let dir = TempDir::new().expect("tempdir");
        let state = load(dir.path());
        assert!(!state.enabled);
        assert_eq!(state.source, ProposeConsentSource::Undecided);
        assert!(!state.is_decided());
    }

    #[test]
    fn a_human_answer_persists_in_both_directions() {
        let dir = TempDir::new().expect("tempdir");
        let yes = record(dir.path(), true, "2026-07-27T10:00:00Z").expect("record yes");
        assert!(yes.enabled);
        assert!(yes.is_decided());

        let no = record(dir.path(), false, "2026-07-27T10:05:00Z").expect("record no");
        assert!(!no.enabled);
        // A remembered NO is still DECIDED — that is what stops the re-ask.
        assert!(no.is_decided());
        assert_eq!(load(dir.path()), no);
    }

    #[test]
    fn a_stale_answer_loses_the_merge() {
        let dir = TempDir::new().expect("tempdir");
        record(dir.path(), true, "2026-07-27T10:10:00Z").expect("newer yes");
        let merged = record(dir.path(), false, "2026-07-27T10:00:00Z").expect("stale no");
        assert!(merged.enabled, "stale answer must lose");
    }

    #[test]
    fn a_corrupt_file_fails_closed() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("tracker").join("propose-consent.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{not json").expect("write garbage");
        let state = load(dir.path());
        assert!(!state.enabled, "unreadable consent must be OFF");
        assert!(!state.is_decided(), "unreadable consent must be UNDECIDED");
    }

    #[test]
    fn env_explicit_recognises_only_the_four_stated_values() {
        assert_eq!(env_explicit(Some("1")), Some(true));
        assert_eq!(env_explicit(Some(" true ")), Some(true));
        assert_eq!(env_explicit(Some("0")), Some(false));
        assert_eq!(env_explicit(Some("FALSE")), Some(false));
        assert_eq!(env_explicit(Some("nonsense")), None, "a typo abstains");
        assert_eq!(env_explicit(None), None);
    }

    #[test]
    fn nothing_said_anywhere_is_off() {
        assert!(!resolve(None, &ProposeConsent::default()));
        assert!(
            !resolve(Some("nonsense"), &ProposeConsent::default()),
            "a typo with no remembered answer must stay off"
        );
    }

    #[test]
    fn an_explicit_env_value_wins_over_the_remembered_answer_in_both_directions() {
        let yes = ProposeConsent {
            enabled: true,
            decided_at: Some("2026-07-27T10:00:00Z".into()),
            source: ProposeConsentSource::Human,
        };
        let no = ProposeConsent {
            enabled: false,
            decided_at: Some("2026-07-27T10:00:00Z".into()),
            source: ProposeConsentSource::Human,
        };
        assert!(
            !resolve(Some("0"), &yes),
            "env off must beat remembered yes"
        );
        assert!(resolve(Some("1"), &no), "env on must beat remembered no");
    }

    #[test]
    fn the_remembered_answer_speaks_only_when_the_env_is_silent() {
        let yes = ProposeConsent {
            enabled: true,
            decided_at: Some("2026-07-27T10:00:00Z".into()),
            source: ProposeConsentSource::Human,
        };
        assert!(resolve(None, &yes));
        assert!(
            resolve(Some("nonsense"), &yes),
            "an unparseable env abstains rather than silencing the human"
        );
    }
}
