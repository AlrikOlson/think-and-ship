//! Telemetry consent (`telemetry-consent-gate`).
//!
//! DECIDED (2026-06-10, user): telemetry is **opt-in on every tier**. Nothing
//! is collected or sent until a human runs the explicit enable, whose prompt
//! carries the full disclosure — so the enable surface IS the disclosure.
//!
//! Consent is a local file (`<data_dir>/telemetry/consent.json`) written
//! through the crate's one locking seam (`locked_merge_write`); an explicit
//! choice is never overridden by a default, and concurrent writers resolve
//! newest-`decided_at`-wins. The egress predicate [`should_send`] is
//! structurally zero without a configured endpoint: enterprise/self-host
//! deployments configure none, so they cannot emit regardless of any flag.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::persistence::locked_merge_write;

/// How the current consent value came to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentSource {
    /// Never decided — the opt-in-everywhere default (disabled).
    Default,
    /// A human ran `telemetry on`/`telemetry off`.
    Explicit,
}

/// The persisted consent state. The default is DISABLED for every tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentState {
    pub enabled: bool,
    /// ISO-8601 moment of the explicit decision; `None` while defaulted.
    pub decided_at: Option<String>,
    pub source: ConsentSource,
}

impl Default for ConsentState {
    fn default() -> Self {
        Self {
            enabled: false,
            decided_at: None,
            source: ConsentSource::Default,
        }
    }
}

/// Consent persistence error.
#[derive(Debug, thiserror::Error)]
pub enum ConsentError {
    #[error("consent io: {0}")]
    Io(#[from] std::io::Error),
}

fn consent_path(data_dir: &Path) -> PathBuf {
    data_dir.join("telemetry").join("consent.json")
}

/// Load the consent state; a missing or unreadable file is the default
/// (disabled) — never an error, so a broken file can only fail CLOSED.
#[must_use]
pub fn load(data_dir: &Path) -> ConsentState {
    std::fs::read_to_string(consent_path(data_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record an explicit human decision (`now` is ISO-8601). Concurrent writers
/// merge newest-`decided_at`-wins; an explicit choice always beats a default.
pub fn set(data_dir: &Path, enabled: bool, now: &str) -> Result<ConsentState, ConsentError> {
    let state = ConsentState {
        enabled,
        decided_at: Some(now.to_string()),
        source: ConsentSource::Explicit,
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

/// The egress predicate (consumed by 31h-c): send only when a telemetry
/// endpoint is configured AND consent is explicitly enabled. No endpoint
/// (enterprise/self-host) is structurally zero regardless of the flag.
#[must_use]
pub fn should_send(state: &ConsentState, endpoint: Option<&str>) -> bool {
    endpoint.is_some_and(|e| !e.is_empty()) && state.enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_is_disabled_and_undecided() {
        let dir = TempDir::new().expect("tempdir");
        let state = load(dir.path());
        assert!(!state.enabled);
        assert_eq!(state.source, ConsentSource::Default);
        assert!(state.decided_at.is_none());
    }

    #[test]
    fn explicit_enable_persists_and_disable_overrides() {
        let dir = TempDir::new().expect("tempdir");
        let on = set(dir.path(), true, "2026-06-10T09:00:00Z").expect("set on");
        assert!(on.enabled);
        assert_eq!(on.source, ConsentSource::Explicit);

        let off = set(dir.path(), false, "2026-06-10T09:05:00Z").expect("set off");
        assert!(!off.enabled);
        assert_eq!(load(dir.path()), off);
    }

    #[test]
    fn newer_decision_wins_the_merge() {
        let dir = TempDir::new().expect("tempdir");
        set(dir.path(), true, "2026-06-10T09:10:00Z").expect("newer on");
        // A stale writer (earlier decided_at) must not clobber the newer one.
        let merged = set(dir.path(), false, "2026-06-10T09:00:00Z").expect("stale off");
        assert!(merged.enabled, "stale decision must lose the merge");
    }

    #[test]
    fn corrupt_file_fails_closed() {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("telemetry").join("consent.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{not json").expect("write garbage");
        assert!(!load(dir.path()).enabled);
    }

    #[test]
    fn should_send_requires_endpoint_and_consent() {
        let on = ConsentState {
            enabled: true,
            decided_at: Some("2026-06-10T09:00:00Z".into()),
            source: ConsentSource::Explicit,
        };
        let off = ConsentState::default();
        assert!(should_send(&on, Some("https://ingest.example")));
        assert!(!should_send(&on, None), "no endpoint = structurally zero");
        assert!(!should_send(&on, Some("")));
        assert!(!should_send(&off, Some("https://ingest.example")));
    }
}
