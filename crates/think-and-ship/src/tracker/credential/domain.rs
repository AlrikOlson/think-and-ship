//! What a credential IS, independent of where it came from or where it lives.
//!
//! The load-bearing idea is that a secret and *how to present it* travel
//! together. That is not obvious until you meet a second provider: Linear sends
//! a personal API key as a bare `Authorization: <key>` but an OAuth token as
//! `Authorization: Bearer <token>`, and the two are not interchangeable. A
//! credential store that hands back a `String` forces every adapter to guess the
//! scheme from the token's shape — a guess that is wrong the first time a
//! provider changes its key prefix. So [`Credential`] carries both.

use std::fmt;

use serde::{Deserialize, Serialize};

/// How a secret must appear in the `Authorization` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <secret>` — OAuth access tokens, GitHub App
    /// installation tokens, GitHub PATs.
    Bearer,
    /// `Authorization: <secret>` with no prefix — Linear personal API keys.
    Raw,
}

impl AuthScheme {
    /// Render the header value. The one place the prefix rule lives, so no
    /// adapter re-derives it.
    #[must_use]
    pub fn header_value(self, secret: &str) -> String {
        match self {
            Self::Bearer => format!("Bearer {secret}"),
            Self::Raw => secret.to_string(),
        }
    }
}

/// Which flow produced a credential.
///
/// The projector never sees this — it exists so the *store* knows whether a
/// credential can be refreshed, and so a human can be told what they connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// A long-lived key a human pasted. Cannot be refreshed; rotation means the
    /// human issues a new one.
    PersonalKey,
    /// OAuth 2.0 authorization-code (Jira 3LO, Linear OAuth). Refreshable.
    OAuth,
    /// A GitHub App installation token: short-lived, minted from an app key
    /// rather than refreshed with a refresh token.
    AppInstallation,
}

impl GrantKind {
    /// Whether a stored credential of this kind can be renewed without a human.
    #[must_use]
    pub fn is_refreshable(self) -> bool {
        matches!(self, Self::OAuth | Self::AppInstallation)
    }
}

/// A secret that refuses to print itself.
///
/// `Debug` is what leaks tokens in practice — a `tracing::debug!` on a struct,
/// a panic message, an `unwrap` on a `Result<_, SomethingContainingAToken>`.
/// Deriving `Debug` on a credential type is the bug; this makes it impossible.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

/// What `Debug` and `Display` print instead of the secret.
pub const REDACTED: &str = "«redacted»";

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way to read the secret. Named so that every call site is
    /// greppable — if a token ever leaks, this is the list of suspects.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// A credential resolved and ready to use: the secret plus how to present it.
///
/// This is ALL an adapter sees. It carries no refresh token, no client secret
/// and no grant kind, because an adapter has no business renewing anything —
/// that is the resolver's job, behind the port.
///
/// It DOES carry [`Credential::site`], because some providers scope a token to
/// a resource the adapter cannot infer. A Jira 3LO token is not site-scoped:
/// every call goes to `https://api.atlassian.com/ex/jira/{cloudid}/…`, and the
/// cloudid is discovered at consent time from an endpoint the adapter is not
/// allowed to know about. Leaving it out would either strand the adapter with
/// no URL to call or force it back through credential custody for a second
/// lookup — which is the coupling this port exists to prevent.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    secret: Secret,
    scheme: AuthScheme,
    site: Option<String>,
}

impl Credential {
    #[must_use]
    pub fn new(secret: Secret, scheme: AuthScheme) -> Self {
        Self {
            secret,
            scheme,
            site: None,
        }
    }

    /// Attach the provider-side resource this credential is scoped to (a Jira
    /// 3LO cloudid). Not a secret, and not renewal material.
    #[must_use]
    pub fn with_site(mut self, site: impl Into<String>) -> Self {
        self.site = Some(site.into());
        self
    }

    /// The provider-side resource this credential is scoped to, when there is
    /// one. `None` for every provider whose API base is fixed.
    #[must_use]
    pub fn site(&self) -> Option<&str> {
        self.site.as_deref()
    }

    /// The complete `Authorization` header value, prefix included.
    ///
    /// Adapters should prefer this over reading the secret, so the scheme rule
    /// stays in one place.
    #[must_use]
    pub fn header_value(&self) -> String {
        self.scheme.header_value(self.secret.expose())
    }

    #[must_use]
    pub fn scheme(&self) -> AuthScheme {
        self.scheme
    }

    #[must_use]
    pub fn secret(&self) -> &Secret {
        &self.secret
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Scheme and site are not secret and are exactly what you need when
        // debugging a 401 or a call that reached the wrong Jira site.
        f.debug_struct("Credential")
            .field("scheme", &self.scheme)
            .field("site", &self.site)
            .field("secret", &REDACTED)
            .finish()
    }
}

/// Everything the STORE keeps for one provider — including the material an
/// adapter must never see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    /// Lowercase provider key, matching `TrackerPort::provider()`.
    pub provider: String,
    pub grant: GrantKind,
    pub scheme: AuthScheme,
    pub access: Secret,
    /// Present only for refreshable grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Secret>,
    /// RFC-3339 expiry of `access`, when the provider states one. `None` means
    /// "does not expire on a schedule we were told about" — a personal key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The OAuth client id this credential was minted under, when there is one.
    ///
    /// Renewal material, not a secret: a PKCE public client refreshes with its
    /// client id ALONE, and the sign-in process that knew the id has exited by
    /// the time the token expires. Absent for pasted keys — and for credentials
    /// sealed before this field existed, which must keep loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The OAuth client SECRET this credential was minted under, for
    /// confidential clients only.
    ///
    /// Renewal material, and the only field here that exists because a
    /// provider left us no choice. Atlassian requires `client_secret` on the
    /// Jira 3LO refresh as well as the exchange, and does not document PKCE —
    /// so a public-client refresh is not available. The sign-in process that
    /// knew the secret has exited by the time the token expires, so either it
    /// is kept beside the refresh token in the same encrypted store or the
    /// credential silently becomes unrenewable and locks the user out days
    /// later. The rejected alternative was reading it from an environment
    /// variable at refresh time, which makes a headless renewal depend on
    /// something the sign-in never mentioned.
    ///
    /// `None` for public clients (Linear, PKCE) and for pasted keys. Never
    /// reaches an adapter: [`StoredCredential::as_credential`] drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<Secret>,
    /// The provider-side resource this credential is scoped to — a Jira 3LO
    /// cloudid, resolved once at consent from `accessible-resources`.
    ///
    /// It lives HERE, on the credential, rather than in a project's tracker
    /// config, because it is a property of (this token, the site the human
    /// picked on the consent screen). In project config the two could drift,
    /// and every project would keep its own copy of one account-level fact.
    /// It is also not re-resolved per call: that would add an authenticated
    /// round trip before every API call and move the choice away from the one
    /// moment a human is present to make it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    pub updated_at: String,
}

impl StoredCredential {
    /// A long-lived key a human pasted.
    #[must_use]
    pub fn personal_key(provider: &str, secret: &str, scheme: AuthScheme, now: &str) -> Self {
        Self {
            provider: provider.trim().to_ascii_lowercase(),
            grant: GrantKind::PersonalKey,
            scheme,
            access: Secret::new(secret),
            refresh: None,
            expires_at: None,
            client_id: None,
            client_secret: None,
            site: None,
            updated_at: now.to_string(),
        }
    }

    /// Whether `access` is past its stated expiry at `now`.
    ///
    /// Parses both stamps rather than comparing bytes: a provider returns `Z`
    /// form while this engine mints `+00:00`, and those compare wrongly as
    /// strings while denoting the same instant.
    #[must_use]
    pub fn is_expired(&self, now: &str) -> bool {
        let Some(expiry) = &self.expires_at else {
            return false;
        };
        match (
            chrono::DateTime::parse_from_rfc3339(expiry),
            chrono::DateTime::parse_from_rfc3339(now),
        ) {
            (Ok(exp), Ok(now)) => now >= exp,
            // An unparseable expiry is treated as expired: forcing a refresh is
            // recoverable, using a dead token is not.
            _ => true,
        }
    }

    /// The adapter-facing view, with all renewal material dropped and the
    /// resource scope carried through.
    #[must_use]
    pub fn as_credential(&self) -> Credential {
        let credential = Credential::new(self.access.clone(), self.scheme);
        match &self.site {
            Some(site) => credential.with_site(site),
            None => credential,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finding Linear produced: the schemes are not interchangeable, and the
    /// prefix rule lives in exactly one place.
    #[test]
    fn the_scheme_decides_the_header_and_lives_in_one_place() {
        assert_eq!(AuthScheme::Bearer.header_value("tok"), "Bearer tok");
        assert_eq!(AuthScheme::Raw.header_value("lin_api_x"), "lin_api_x");
    }

    /// Debug is how tokens leak in practice — into logs, panics and error
    /// chains. Neither the secret nor anything holding it may print itself.
    #[test]
    fn a_secret_refuses_to_print_itself() {
        let s = Secret::new("super-secret-value");
        assert_eq!(format!("{s:?}"), REDACTED);
        assert_eq!(format!("{s}"), REDACTED);
        assert!(!format!("{s:?}").contains("super-secret"));

        let c = Credential::new(Secret::new("super-secret-value"), AuthScheme::Bearer);
        let rendered = format!("{c:?}");
        assert!(!rendered.contains("super-secret"), "got: {rendered}");
        // …but the scheme IS visible, because it is what you need when
        // debugging a 401 and it is not a secret.
        assert!(rendered.contains("Bearer"));

        // Including when nested inside another struct's derived Debug.
        let stored = StoredCredential::personal_key(
            "linear",
            "super-secret-value",
            AuthScheme::Raw,
            "2026-07-26T00:00:00Z",
        );
        assert!(!format!("{stored:?}").contains("super-secret"));
    }

    /// The adapter-facing view must not carry renewal material. If it did, an
    /// adapter could refresh, and refresh would stop being one thing.
    #[test]
    fn the_adapter_view_drops_the_refresh_token() {
        let stored = StoredCredential {
            refresh: Some(Secret::new("refresh-me")),
            ..StoredCredential::personal_key(
                "jira",
                "access",
                AuthScheme::Bearer,
                "2026-07-26T00:00:00Z",
            )
        };
        let cred = stored.as_credential();
        assert_eq!(cred.header_value(), "Bearer access");
        assert!(!format!("{cred:?}").contains("refresh-me"));
    }

    /// The client secret is renewal material for a confidential client and must
    /// stop at the port — an adapter that could see it could mint its own
    /// tokens, which is the whole thing this view exists to prevent. The SITE
    /// must cross, because a Jira adapter has no URL without it.
    #[test]
    fn the_adapter_view_drops_the_client_secret_and_keeps_the_site() {
        let stored = StoredCredential {
            refresh: Some(Secret::new("refresh-me")),
            client_id: Some("client-42".into()),
            client_secret: Some(Secret::new("SECRET-THAT-MINTS-TOKENS")),
            site: Some("cloud-id-abc".into()),
            ..StoredCredential::personal_key(
                "jira",
                "access",
                AuthScheme::Bearer,
                "2026-07-28T00:00:00Z",
            )
        };

        let cred = stored.as_credential();
        assert_eq!(cred.site(), Some("cloud-id-abc"));

        let rendered = format!("{cred:?}");
        assert!(
            !rendered.contains("SECRET-THAT-MINTS-TOKENS"),
            "got: {rendered}"
        );
        // The site is NOT a secret and is what you need when a call reached the
        // wrong Jira site, so it must be visible.
        assert!(rendered.contains("cloud-id-abc"), "got: {rendered}");
    }

    /// The stored secret refuses to print itself even nested — the field is new,
    /// so its redaction is proven rather than inherited by assumption.
    #[test]
    fn a_stored_client_secret_does_not_print() {
        let stored = StoredCredential {
            client_secret: Some(Secret::new("atlassian-app-secret")),
            ..StoredCredential::personal_key(
                "jira",
                "a",
                AuthScheme::Bearer,
                "2026-07-28T00:00:00Z",
            )
        };
        assert!(!format!("{stored:?}").contains("atlassian-app-secret"));
    }

    #[test]
    fn only_refreshable_grants_claim_to_be_refreshable() {
        assert!(!GrantKind::PersonalKey.is_refreshable());
        assert!(GrantKind::OAuth.is_refreshable());
        assert!(GrantKind::AppInstallation.is_refreshable());
    }

    /// Expiry compares INSTANTS, not bytes. `Z` and `+00:00` denote the same
    /// moment and sort differently as strings.
    #[test]
    fn expiry_compares_instants_not_strings() {
        let mut c =
            StoredCredential::personal_key("linear", "k", AuthScheme::Raw, "2026-07-26T00:00:00Z");
        assert!(!c.is_expired("2026-07-26T00:00:00Z"), "no expiry set");

        c.expires_at = Some("2026-07-26T10:00:00Z".into());
        assert!(!c.is_expired("2026-07-26T09:59:59+00:00"));
        assert!(c.is_expired("2026-07-26T10:00:01+00:00"));
        // Same instant in the other form must compare equal, not by byte order.
        assert!(c.is_expired("2026-07-26T10:00:00+00:00"));
    }

    /// A record sealed before `client_id` existed must keep loading. The field
    /// arrived with app-actor sign-in; every store written before it is full of
    /// records without it, and a deserialization failure here would read as
    /// "credential missing" — a silent sign-out.
    #[test]
    fn a_record_sealed_before_client_id_still_loads() {
        let old = r#"{
            "provider": "linear",
            "grant": "o_auth",
            "scheme": "bearer",
            "access": "tok",
            "refresh": "ref",
            "expires_at": "2026-07-27T00:00:00Z",
            "updated_at": "2026-07-26T00:00:00Z"
        }"#;
        let stored: StoredCredential = serde_json::from_str(old).expect("old record loads");
        assert_eq!(stored.client_id, None);
        assert_eq!(stored.access.expose(), "tok");
        // The same rule for the two newest fields: a store written
        // before them must keep loading, or the reader sees "credential
        // missing" and the user is silently signed out.
        assert!(stored.client_secret.is_none());
        assert!(stored.site.is_none());
    }

    /// An unparseable expiry forces a refresh rather than being ignored:
    /// refreshing unnecessarily is recoverable, using a dead token is not.
    #[test]
    fn an_unreadable_expiry_fails_toward_refresh() {
        let mut c =
            StoredCredential::personal_key("linear", "k", AuthScheme::Raw, "2026-07-26T00:00:00Z");
        c.expires_at = Some("not a timestamp".into());
        assert!(c.is_expired("2026-07-26T00:00:00Z"));
    }
}
