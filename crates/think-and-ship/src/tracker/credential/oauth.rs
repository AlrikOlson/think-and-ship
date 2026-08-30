//! Renewal and revocation — the half of custody that involves the network.
//!
//! # Refresh-token rotation is assumed, not optional
//!
//! Jira 3LO rotates the refresh token on every refresh: the response carries a
//! NEW refresh token and the old one stops working. An implementation that keeps
//! the original refresh token works exactly once and then locks the user out
//! until they re-consent — a failure that shows up days later and looks like
//! nothing. So [`Resolver`] persists whatever refresh token comes back, and if
//! none comes back it keeps the previous one (Linear's behaviour). Both branches
//! are tested.
//!
//! # Revoke must revoke
//!
//! This codebase has already shipped rotate-without-revoke once — the
//! `alias-revoke` chunk exists because old secrets stayed live. So revocation
//! here does two things and is tested on the observable consequence rather than
//! on its return value: it calls the provider's revocation endpoint, AND it
//! forgets the credential locally. The test asserts that the NEXT call fails.
//! "revoke returned Ok" is not evidence of anything.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use super::domain::{AuthScheme, Credential, Secret, StoredCredential};
use super::store::{CredentialError, CredentialStore};

/// What an adapter asks for. The whole point: no grant type in the signature.
#[async_trait]
pub trait CredentialPort: Send + Sync {
    /// A usable credential for `provider`, renewed first if it had expired.
    async fn credential(&self, provider: &str) -> Result<Credential, CredentialError>;
}

/// One provider's OAuth endpoints and client identity.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Where the human is sent to grant consent (authorization-code flow).
    pub authorize_url: String,
    pub token_url: String,
    /// Absent for providers with no revocation endpoint — then revoke is local
    /// only, and [`Resolver::revoke`] says so rather than pretending.
    pub revoke_url: Option<String>,
    pub client_id: String,
    pub client_secret: Secret,
    /// Extra parameters pinned to the authorize URL, in provider vocabulary.
    ///
    /// This is where a provider's non-OAuth knobs live without leaking into the
    /// generic flow: Linear's `actor=app` (writes attributed to the application),
    /// and later Atlassian's `audience=`/`prompt=` for 3LO. Keys and values are
    /// percent-encoded at render time.
    pub authorize_params: Vec<(String, String)>,
}

/// The OAuth wire calls, separated from policy so the policy is testable.
pub struct OAuthClient {
    http: reqwest::Client,
}

/// The subset of an OAuth token response this system uses.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    /// Present when the provider rotates (Jira 3LO); absent when it does not
    /// (Linear). Both are legal and both are handled.
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

impl Default for OAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OAuthClient {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// Exchange a refresh token for a fresh access token.
    ///
    /// `now` is injected rather than read so expiry arithmetic is testable
    /// without sleeping.
    pub async fn refresh(
        &self,
        config: &OAuthConfig,
        stored: &StoredCredential,
        now: &str,
    ) -> Result<StoredCredential, CredentialError> {
        let refresh = stored.refresh.as_ref().ok_or_else(|| {
            CredentialError::Invalid(format!(
                "credential for '{}' has expired and carries no refresh token — reconnect it",
                stored.provider
            ))
        })?;

        // The form carries the refresh token and the client secret: never
        // onto a cleartext transport.
        super::transport::require_tls(&config.token_url)?;
        let resp = self
            .http
            .post(&config.token_url)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.expose()),
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.expose()),
            ])
            .send()
            .await
            .map_err(|e| CredentialError::Invalid(format!("refresh transport failed: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            // A 400 from a token endpoint is the canonical "refresh token is
            // dead" signal. Naming it beats surfacing a bare status.
            return Err(CredentialError::Invalid(format!(
                "refresh for '{}' rejected with {status}: {body} — the refresh token may have been \
                 rotated away or revoked; reconnect the provider",
                stored.provider
            )));
        }

        let token: TokenResponse = resp
            .json()
            .await
            .map_err(|e| CredentialError::Invalid(format!("refresh response unreadable: {e}")))?;

        let expires_at = token.expires_in.and_then(|secs| {
            chrono::DateTime::parse_from_rfc3339(now)
                .ok()
                .map(|t| (t + chrono::Duration::seconds(secs)).to_rfc3339())
        });

        Ok(StoredCredential {
            provider: stored.provider.clone(),
            grant: stored.grant,
            scheme: stored.scheme,
            access: Secret::new(token.access_token),
            // ROTATION: take the new refresh token when the provider sends one
            // (Jira 3LO always does), otherwise keep the existing one (Linear).
            // Dropping it in the first case locks the user out one refresh later.
            refresh: token
                .refresh_token
                .map(Secret::new)
                .or_else(|| stored.refresh.clone()),
            expires_at,
            // Carried forward, not re-derived: dropping it here would make the
            // SECOND refresh unconfigurable after a process restart.
            client_id: stored.client_id.clone(),
            // Same reason, and it bites harder: a confidential client cannot
            // refresh AT ALL without its secret, so losing it here turns the
            // next expiry into a forced re-consent.
            client_secret: stored.client_secret.clone(),
            // A refresh renews a token; it does not re-run consent, so the site
            // the human picked is still the site this token is for.
            site: stored.site.clone(),
            updated_at: now.to_string(),
        })
    }

    /// Ask the provider to invalidate a token. `Ok(false)` means the provider
    /// has no revocation endpoint, which the caller must report honestly rather
    /// than treat as success.
    pub async fn revoke(
        &self,
        config: &OAuthConfig,
        stored: &StoredCredential,
    ) -> Result<bool, CredentialError> {
        let Some(url) = &config.revoke_url else {
            return Ok(false);
        };
        let resp = self
            .http
            .post(url)
            .form(&[
                ("token", stored.access.expose()),
                ("client_id", config.client_id.as_str()),
                ("client_secret", config.client_secret.expose()),
            ])
            .send()
            .await
            .map_err(|e| CredentialError::Invalid(format!("revoke transport failed: {e}")))?;

        let status = resp.status().as_u16();
        // RFC 7009: a revocation endpoint returns 200 even for an
        // already-invalid token, so a non-2xx is a real problem.
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(CredentialError::Invalid(format!(
                "revoke for '{}' failed with {status}: {body}",
                stored.provider
            )));
        }
        Ok(true)
    }
}

/// The [`CredentialPort`] implementation: store + renewal policy.
///
/// Renewal happens HERE and only here. An adapter that got a refresh token
/// could renew independently, two adapters could race, and the store would end
/// up with whichever won — so the adapter-facing type deliberately cannot.
pub struct Resolver {
    store: Arc<dyn CredentialStore>,
    oauth: OAuthClient,
    /// Per-provider OAuth config. A provider absent from this map can still be
    /// used with a pasted key — it just cannot be refreshed.
    configs: std::collections::HashMap<String, OAuthConfig>,
    /// Injected clock: `None` means read the real one. Tests set it so expiry
    /// and rotation are provable without sleeping.
    now: Option<String>,
}

impl Resolver {
    #[must_use]
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            oauth: OAuthClient::new(),
            configs: std::collections::HashMap::new(),
            now: None,
        }
    }

    #[must_use]
    pub fn with_oauth(mut self, provider: &str, config: OAuthConfig) -> Self {
        self.configs
            .insert(provider.trim().to_ascii_lowercase(), config);
        self
    }

    #[must_use]
    pub fn at(mut self, now: &str) -> Self {
        self.now = Some(now.to_string());
        self
    }

    fn now(&self) -> String {
        self.now.clone().unwrap_or_else(|| {
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
    }

    /// Store a pasted key. The non-OAuth path, and the one a solo user takes.
    pub fn connect_personal_key(
        &self,
        provider: &str,
        secret: &str,
        scheme: AuthScheme,
    ) -> Result<(), CredentialError> {
        if secret.trim().is_empty() {
            return Err(CredentialError::Invalid(
                "a personal key cannot be empty".into(),
            ));
        }
        self.store.save(&StoredCredential::personal_key(
            provider,
            secret.trim(),
            scheme,
            &self.now(),
        ))
    }

    /// Adopt a credential obtained elsewhere — currently the authorization-code
    /// flow.
    ///
    /// THE single persistence path. `authcode` deliberately returns a credential
    /// rather than saving one, so everything that lands ends up under the same
    /// refresh, rotation and revoke machinery instead of a parallel store.
    pub fn adopt(&self, credential: &StoredCredential) -> Result<(), CredentialError> {
        self.store.save(credential)
    }

    /// Revoke a credential: tell the provider, then forget it locally.
    ///
    /// Returns whether the provider was actually asked. Local deletion happens
    /// EVEN IF the remote call fails — a token we can no longer manage must not
    /// stay on disk, and leaving it there is how `alias-revoke` happened. The
    /// error is surfaced after deletion so the human knows to revoke it in the
    /// provider's UI.
    pub async fn revoke(&self, provider: &str) -> Result<bool, CredentialError> {
        let provider = provider.trim().to_ascii_lowercase();
        let stored = self.store.load(&provider)?;

        let mut remote = Ok(false);
        if let (Some(stored), Some(config)) = (&stored, self.configs.get(&provider)) {
            remote = self.oauth.revoke(config, stored).await;
        }

        // Always forget locally, whatever the provider said.
        self.store.delete(&provider)?;
        remote
    }
}

#[async_trait]
impl CredentialPort for Resolver {
    async fn credential(&self, provider: &str) -> Result<Credential, CredentialError> {
        let provider = provider.trim().to_ascii_lowercase();
        let stored = self
            .store
            .load(&provider)?
            .ok_or_else(|| CredentialError::Missing(provider.clone()))?;

        let now = self.now();
        if !stored.is_expired(&now) {
            return Ok(stored.as_credential());
        }

        // Expired. A pasted key cannot be renewed — say what to do about it
        // rather than returning a bare failure.
        if !stored.grant.is_refreshable() {
            return Err(CredentialError::Invalid(format!(
                "the stored key for '{provider}' has expired and cannot be refreshed \
                 (it is a {:?} grant) — issue a new one and reconnect",
                stored.grant
            )));
        }
        let config = self.configs.get(&provider).ok_or_else(|| {
            CredentialError::Invalid(format!(
                "credential for '{provider}' has expired and no OAuth configuration is \
                 registered to refresh it"
            ))
        })?;

        let refreshed = self.oauth.refresh(config, &stored, &now).await?;
        // Persist BEFORE returning: with a rotating provider the old refresh
        // token is already dead, so failing to save here would strand the user.
        self.store.save(&refreshed)?;
        Ok(refreshed.as_credential())
    }
}

/// The Jira 3LO profile, with the Forge rejection recorded where it is relevant.
///
/// Atlassian Connect is deprecated (apps required on Forge by Q4 2026) and
/// Atlassian steers integrations to "Forge or 3LO/OAuth 2.0". Forge is rejected
/// for a hosting-model reason rather than a preference: it runs your code INSIDE
/// Atlassian's runtime, and this system is an external Rust service plus a
/// Cloudflare Worker calling the Cloud REST API from its own infrastructure —
/// the documented 3LO case. Accepted with it: per-site consent, an
/// accessible-resources cloudid lookup before any API call, and refresh-token
/// rotation on EVERY refresh, which is why [`OAuthClient::refresh`] persists the
/// returned refresh token instead of keeping the original.
///
/// # The two parameters that are not optional
///
/// `audience=api.atlassian.com` and `prompt=consent` are REQUIRED by Atlassian
/// on the authorize URL, and this profile shipped without either of them —
/// green, because the only test asserted the token host and the absent revoke
/// endpoint. Without `audience` the authorization is not for the API at all;
/// without `prompt=consent` a returning user is not re-prompted and no refresh
/// token is issued. They are pinned in the profile rather than passed by the
/// caller for the same reason Linear's `actor=app` is: a provider's vocabulary
/// belongs to its profile, not to the generic flow.
///
/// # Confidential client
///
/// Atlassian requires `client_secret` on both the code exchange and the
/// refresh, and documents no PKCE for 3LO — so unlike Linear this is not a
/// public client, and the secret has to survive the sign-in process. See
/// [`StoredCredential::client_secret`].
///
/// [`StoredCredential::client_secret`]: super::domain::StoredCredential::client_secret
#[must_use]
pub fn jira_3lo(client_id: &str, client_secret: &str) -> OAuthConfig {
    OAuthConfig {
        authorize_url: "https://auth.atlassian.com/authorize".into(),
        token_url: "https://auth.atlassian.com/oauth/token".into(),
        // Atlassian documents no token-revocation endpoint for 3LO; revocation
        // is a user action in site settings. Modelled as None so `revoke`
        // reports "forgotten locally, revoke it in Atlassian too" rather than
        // implying the token is dead.
        revoke_url: None,
        client_id: client_id.into(),
        client_secret: Secret::new(client_secret),
        authorize_params: vec![
            ("audience".into(), "api.atlassian.com".into()),
            ("prompt".into(), "consent".into()),
        ],
    }
}

/// The scope every Jira sign-in must ask for on top of whatever it wants to do.
///
/// Atlassian issues a refresh token ONLY when `offline_access` is among the
/// requested scopes. Omit it and the flow succeeds, the credential works for an
/// hour, and then the user is signed out with nothing to renew from — the exact
/// shape of the 24-hour credential `linear-app-actor-identity` found. So it is
/// added by the sign-in path rather than left to whoever types the `--scopes`
/// flag.
pub const JIRA_OFFLINE_SCOPE: &str = "offline_access";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::credential::store::FileCredentialStore;
    use tempfile::TempDir;

    const NOW: &str = "2026-07-26T12:00:00+00:00";

    fn resolver(dir: &TempDir) -> Resolver {
        Resolver::new(Arc::new(FileCredentialStore::new(dir.path()))).at(NOW)
    }

    #[tokio::test]
    async fn a_pasted_key_resolves_with_its_provider_scheme() {
        let dir = TempDir::new().expect("tempdir");
        let r = resolver(&dir);
        r.connect_personal_key("linear", "lin_api_x", AuthScheme::Raw)
            .expect("connect");

        let cred = r.credential("linear").await.expect("resolve");
        // Raw, not Bearer — the Linear finding, enforced end to end.
        assert_eq!(cred.header_value(), "lin_api_x");
    }

    #[tokio::test]
    async fn an_unconnected_provider_is_missing_not_empty() {
        let dir = TempDir::new().expect("tempdir");
        let err = resolver(&dir)
            .credential("github")
            .await
            .expect_err("must fail");
        assert!(matches!(err, CredentialError::Missing(_)));
        assert!(err.to_string().contains("connect it first"));
    }

    /// An expired pasted key cannot be refreshed, and the error must say what to
    /// do instead of just failing.
    #[tokio::test]
    async fn an_expired_pasted_key_says_to_reconnect() {
        let dir = TempDir::new().expect("tempdir");
        let store = FileCredentialStore::new(dir.path());
        let mut cred = StoredCredential::personal_key("github", "ghp_x", AuthScheme::Bearer, NOW);
        cred.expires_at = Some("2026-07-26T11:00:00+00:00".into());
        store.save(&cred).expect("save");

        let err = Resolver::new(Arc::new(store))
            .at(NOW)
            .credential("github")
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("reconnect"), "got: {err}");
    }

    #[test]
    fn the_jira_profile_records_the_forge_rejection_and_has_no_revoke_url() {
        let cfg = jira_3lo("client", "secret");
        assert!(cfg.token_url.contains("auth.atlassian.com"));
        assert!(
            cfg.revoke_url.is_none(),
            "Atlassian documents no 3LO revocation endpoint; modelling one would lie"
        );
    }

    /// THE TEST THIS PROFILE SHIPPED WITHOUT. `audience` and `prompt` are
    /// required by Atlassian on the authorize URL: without the first the
    /// authorization is not for the API, and without the second a returning
    /// user is never re-prompted and no refresh token is minted. A profile
    /// missing them passes every assertion above while being unable to complete
    /// a single consent.
    #[test]
    fn the_jira_profile_pins_the_two_authorize_parameters_atlassian_requires() {
        let cfg = jira_3lo("client", "secret");
        let params: std::collections::HashMap<&str, &str> = cfg
            .authorize_params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        assert_eq!(
            params.get("audience"),
            Some(&"api.atlassian.com"),
            "without audience the authorization is not for the API"
        );
        assert_eq!(
            params.get("prompt"),
            Some(&"consent"),
            "without prompt=consent no refresh token is issued to a returning user"
        );
        // And it is a CONFIDENTIAL client, unlike Linear — the secret is
        // carried, not empty.
        assert!(!cfg.client_secret.is_empty());
    }

    #[test]
    fn an_empty_personal_key_is_refused() {
        let dir = TempDir::new().expect("tempdir");
        assert!(
            resolver(&dir)
                .connect_personal_key("linear", "   ", AuthScheme::Raw)
                .is_err()
        );
    }
}
