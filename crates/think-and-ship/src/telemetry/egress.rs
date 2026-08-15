//! Consent-gated shape egress (`telemetry-egress-ingest`).
//!
//! The wire half over the shape extractor and the consent gate. Telemetry
//! traffic is a SEPARATE path from sync: shapes go to a vendor ingest
//! endpoint, never to the user's tenant — and only when
//! [`consent::should_send`](crate::telemetry::consent::should_send) allows.
//! The per-install salt lives next to the consent file; the install pseudonym
//! derived from it lets repeated pushes from one install aggregate without
//! identifying it (the salt itself never leaves the machine).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::telemetry::shape::{StructuralShape, pseudonym};

/// Env var naming the ingest endpoint. There is NO built-in default: with the
/// variable unset, there is nowhere to send and telemetry is structurally zero
/// no matter what consent says. An operator who wants shapes collected names
/// the endpoint that receives them, and so runs the thing they are pointing at.
pub const TELEMETRY_URL_VAR: &str = "THINK_AND_SHIP_TELEMETRY_URL";

fn salt_path(data_dir: &Path) -> PathBuf {
    data_dir.join("telemetry").join("salt")
}

/// Load the per-install salt, generating it on first use (64 hex chars from
/// two v4 UUIDs — 244 random bits). The salt never leaves the machine; only
/// pseudonyms derived from it do.
pub fn load_or_create_salt(data_dir: &Path) -> std::io::Result<Vec<u8>> {
    let path = salt_path(data_dir);
    if let Ok(existing) = std::fs::read(&path)
        && !existing.is_empty()
    {
        return Ok(existing);
    }
    let fresh = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, fresh.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(fresh.into_bytes())
}

/// The wire payload: an install pseudonym + the shape. Nothing else.
/// (Serialize-only: the consumer is the TypeScript ingest, not Rust.)
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryReport {
    /// Salted-hash pseudonym of this install (stable per install, unlinkable
    /// across installs; derived from the local salt, which never leaves).
    pub install: String,
    pub shape: StructuralShape,
}

/// Build the report for this install.
#[must_use]
pub fn build_report(salt: &[u8], shape: StructuralShape) -> TelemetryReport {
    TelemetryReport {
        install: pseudonym(salt, "install"),
        shape,
    }
}

/// Egress failure.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("telemetry transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("telemetry ingest rejected: HTTP {0}")]
    Rejected(u16),
}

/// POST the report to `{endpoint}/v1/telemetry/shapes`. The caller is
/// responsible for the consent gate ([`should_send`]) — this function only
/// moves bytes.
///
/// [`should_send`]: crate::telemetry::consent::should_send
pub async fn send_report(
    http: &reqwest::Client,
    endpoint: &str,
    report: &TelemetryReport,
) -> Result<(), EgressError> {
    let url = format!("{}/v1/telemetry/shapes", endpoint.trim_end_matches('/'));
    let res = http.post(url).json(report).send().await?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(EgressError::Rejected(res.status().as_u16()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::shape::HASH_LEN;
    use tempfile::TempDir;

    #[test]
    fn salt_is_created_once_and_stable() {
        let dir = TempDir::new().expect("tempdir");
        let first = load_or_create_salt(dir.path()).expect("create");
        assert_eq!(first.len(), 64);
        let second = load_or_create_salt(dir.path()).expect("reload");
        assert_eq!(first, second, "salt must be stable across loads");
    }

    #[test]
    fn report_carries_a_pseudonym_not_the_salt() {
        let dir = TempDir::new().expect("tempdir");
        let salt = load_or_create_salt(dir.path()).expect("salt");
        let report = build_report(&salt, StructuralShape::default());
        assert_eq!(report.install.len(), HASH_LEN);
        let serialized = serde_json::to_string(&report).expect("serialize");
        let salt_hex = String::from_utf8(salt).expect("utf8");
        assert!(
            !serialized.contains(&salt_hex),
            "the salt must never appear on the wire"
        );
    }

    #[tokio::test]
    async fn send_report_posts_to_the_ingest_route() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/telemetry/shapes"))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        let report = build_report(b"salt", StructuralShape::default());
        send_report(&reqwest::Client::new(), &server.uri(), &report)
            .await
            .expect("ingest accepts");
    }

    #[tokio::test]
    async fn rejection_surfaces_the_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/telemetry/shapes"))
            .respond_with(ResponseTemplate::new(422))
            .mount(&server)
            .await;

        let report = build_report(b"salt", StructuralShape::default());
        let err = send_report(&reqwest::Client::new(), &server.uri(), &report)
            .await
            .expect_err("must surface rejection");
        assert!(matches!(err, EgressError::Rejected(422)));
    }
}
