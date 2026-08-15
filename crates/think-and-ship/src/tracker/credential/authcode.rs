//! Obtaining an OAuth token: authorization code, PKCE, and a loopback receiver.
//!
//! The Resolver handles refresh, rotation and revoke for a token you already
//! had. This is how you get one.
//!
//! # Loopback, not device flow — decided from what providers support
//!
//! The original design guessed device flow would fit a CLI better
//! (no listener, no port collision, works over SSH) and named
//! `cloud/device_flow.rs` as the template. Checking rather than trusting that
//! guess: **Linear documents authorization-code with registered redirect URLs
//! and explicitly supports PKCE, with no device-authorization endpoint**, and
//! **Atlassian 3LO is likewise authorization-code**. So the device-flow template
//! is wrong for both of the providers this system actually needs, and a loopback
//! receiver is right. The guess was wrong; the instruction to check is what
//! caught it. GitHub does support device flow, and if a GitHub App path is added
//! later, `cloud/device_flow.rs` is the template for that one alone.
//!
//! # PKCE is not optional here
//!
//! A CLI is a *public client*: there is nowhere on a user's machine to keep a
//! client secret, so [`OAuthConfig::client_secret`] is meaningful only for a
//! confidential client. Without PKCE the authorization code is
//! bearer-equivalent — anything that can observe the redirect (another local
//! process, a shell history, a proxy log) can spend it. S256 only; `plain` is
//! legal in RFC 7636 and is not acceptable for something written now.
//!
//! # Three side effects, separated so they can be tested
//!
//! This flow opens a browser, binds a socket and makes a network call. So the
//! pieces come apart: [`Pkce`] and [`authorize_url`] are pure; [`LoopbackReceiver`]
//! binds a real ephemeral port and is tested by making a real request to it;
//! the exchange goes to a mock. Opening the browser is the only untested part,
//! and it is deliberately not in the path — the URL is returned for the caller
//! to print.
//!
//! [`OAuthConfig::client_secret`]: super::oauth::OAuthConfig

use base64::Engine as _;
use sha2::{Digest, Sha256};

use super::domain::{AuthScheme, GrantKind, Secret, StoredCredential};
use super::oauth::OAuthConfig;
use super::store::CredentialError;

/// URL-safe base64 without padding, which is what PKCE and OAuth state want.
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 32 bytes from the OS CSPRNG. Both the verifier and the CSRF state need
/// unguessable values; a weak source here defeats both at once.
fn random_32() -> [u8; 32] {
    use chacha20poly1305::aead::rand_core::RngCore;
    let mut bytes = [0u8; 32];
    chacha20poly1305::aead::OsRng.fill_bytes(&mut bytes);
    bytes
}

/// A PKCE verifier/challenge pair (RFC 7636, S256).
#[derive(Clone)]
pub struct Pkce {
    verifier: Secret,
    challenge: String,
}

impl Pkce {
    /// Generate a fresh pair. The verifier is 43 characters — the RFC's minimum
    /// — from 32 random bytes.
    #[must_use]
    pub fn generate() -> Self {
        let verifier = b64url(&random_32());
        let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier: Secret::new(verifier),
            challenge,
        }
    }

    /// The challenge, which travels in the authorize URL and is public.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// The verifier, which travels ONLY in the token exchange. It is the secret
    /// half — treat it like one.
    #[must_use]
    pub fn verifier(&self) -> &Secret {
        &self.verifier
    }

    #[must_use]
    pub fn method(&self) -> &'static str {
        "S256"
    }
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The challenge is public; the verifier is not.
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .field("verifier", &super::domain::REDACTED)
            .finish()
    }
}

/// An unguessable CSRF state value.
#[must_use]
pub fn new_state() -> String {
    b64url(&random_32())
}

/// Percent-encode a query-parameter value.
///
/// Hand-rolled rather than pulling a dependency for six characters: scopes
/// contain spaces and colons, and redirect URIs contain slashes and colons, all
/// of which must survive intact.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the URL the human opens to grant consent.
///
/// Pure, so the exact parameter set is testable without a browser or a socket.
#[must_use]
pub fn authorize_url(
    config: &OAuthConfig,
    pkce: &Pkce,
    state: &str,
    redirect_uri: &str,
    scopes: &[&str],
) -> String {
    let mut url = format!(
        "{base}?response_type=code&client_id={client}&redirect_uri={redirect}\
         &scope={scope}&state={state}&code_challenge={challenge}&code_challenge_method={method}",
        base = config.authorize_url,
        client = encode(&config.client_id),
        redirect = encode(redirect_uri),
        scope = encode(&scopes.join(" ")),
        state = encode(state),
        challenge = encode(pkce.challenge()),
        method = pkce.method(),
    );
    // Provider-vocabulary knobs pinned by the profile — Linear's `actor=app`
    // travels here, not as a parameter of the generic flow.
    for (key, value) in &config.authorize_params {
        url.push('&');
        url.push_str(&encode(key));
        url.push('=');
        url.push_str(&encode(value));
    }
    url
}

/// What came back on the redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirected {
    pub code: String,
    pub state: String,
}

/// A one-shot localhost listener that catches the redirect.
///
/// Binds an EPHEMERAL port rather than a fixed one: a fixed port collides with
/// whatever else is running, and providers that require exact redirect-URI
/// registration are handled by registering the loopback URI with a wildcard
/// port where the provider allows it, or by the caller pinning a port it has
/// registered.
pub struct LoopbackReceiver {
    listener: std::net::TcpListener,
}

impl LoopbackReceiver {
    /// Bind on 127.0.0.1. `port: 0` asks the OS for a free one.
    pub fn bind(port: u16) -> Result<Self, CredentialError> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
        Ok(Self { listener })
    }

    /// The redirect URI to send to the provider and register with it.
    pub fn redirect_uri(&self) -> Result<String, CredentialError> {
        Ok(format!("http://127.0.0.1:{}/callback", self.port()?))
    }

    pub fn port(&self) -> Result<u16, CredentialError> {
        Ok(self.listener.local_addr()?.port())
    }

    /// Wait for the browser to hit the redirect, then answer it and return what
    /// it carried.
    ///
    /// # Why this loops instead of accepting once
    ///
    /// The first version accepted exactly ONE connection, which is what a
    /// well-behaved test client sends and what a real browser does not. Chrome
    /// and Safari routinely open a speculative connection alongside the
    /// navigation, and fetch `/favicon.ico` after rendering whatever comes back.
    /// Any of those can win the race for a single accept — and then the code
    /// parses a request with no `code` in it, gives up, and the ACTUAL redirect
    /// arrives at a socket nobody is listening on. The user sees a failure for a
    /// flow the provider considers successful.
    ///
    /// So: keep answering requests until one carries a `code` or an explicit
    /// `error`, and give everything else a polite 204 and a closed connection.
    ///
    /// The deadline is not decoration either. A loop with no deadline is a worse
    /// hang than the bug it replaces: a user who closes the tab without
    /// approving deserves a message, not a process that never returns.
    pub fn wait(self, timeout: std::time::Duration) -> Result<Redirected, CredentialError> {
        let deadline = std::time::Instant::now() + timeout;
        // A read timeout on each accepted socket, so one stalled connection
        // cannot hold the whole flow open either.
        self.listener.set_nonblocking(false)?;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(CredentialError::Invalid(format!(
                    "gave up waiting for approval after {}s — if you approved it in the \
                     browser, the redirect did not reach this machine; if you did not, run \
                     the command again",
                    timeout.as_secs()
                )));
            }

            let (mut stream, _) = self.accept_before(deadline)?;
            let _ =
                stream.set_read_timeout(Some(remaining.min(std::time::Duration::from_secs(10))));

            let Some(query) = Self::read_query(&stream) else {
                // Unreadable or empty: not our redirect. Close and keep waiting.
                Self::respond(&mut stream, 204, "");
                continue;
            };

            let mut code = None;
            let mut state = None;
            let mut error = None;
            for pair in query.split('&') {
                let Some((k, v)) = pair.split_once('=') else {
                    continue;
                };
                let v = percent_decode(v);
                match k {
                    "code" => code = Some(v),
                    "state" => state = Some(v),
                    "error" => error = Some(v),
                    _ => {}
                }
            }

            // Anything without a code or an error is browser noise — a favicon,
            // a speculative connection, a probe. Answer it and keep listening.
            if code.is_none() && error.is_none() {
                Self::respond(&mut stream, 204, "");
                continue;
            }

            let outcome = match (error, code, state) {
                (Some(e), _, _) => Err(CredentialError::Invalid(format!(
                    "the provider refused the authorization: {e}"
                ))),
                (None, Some(code), Some(state)) => Ok(Redirected { code, state }),
                _ => Err(CredentialError::Invalid(
                    "the redirect carried an authorization code with no state value".into(),
                )),
            };

            let body = match &outcome {
                Ok(_) => {
                    "<h1>Connected</h1><p>You can close this tab and return to your terminal.</p>"
                }
                Err(_) => "<h1>Not connected</h1><p>Check your terminal for what went wrong.</p>",
            };
            Self::respond(&mut stream, 200, body);
            return outcome;
        }
    }

    /// Accept with the deadline enforced, so a browser that never connects at
    /// all still ends the wait.
    fn accept_before(
        &self,
        deadline: std::time::Instant,
    ) -> Result<(std::net::TcpStream, std::net::SocketAddr), CredentialError> {
        self.listener.set_nonblocking(true)?;
        let result = loop {
            match self.listener.accept() {
                Ok(pair) => break Ok(pair),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        break Err(CredentialError::Invalid(
                            "gave up waiting for the browser to reach this machine".into(),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => break Err(e.into()),
            }
        };
        // Back to blocking for the read; nonblocking is inherited by the socket.
        self.listener.set_nonblocking(false)?;
        if let Ok((stream, _)) = &result {
            let _ = stream.set_nonblocking(false);
        }
        result
    }

    /// Pull the query string out of a request line, if there is one.
    fn read_query(stream: &std::net::TcpStream) -> Option<String> {
        use std::io::{BufRead, BufReader};
        let mut request_line = String::new();
        BufReader::new(stream).read_line(&mut request_line).ok()?;
        // "GET /callback?code=…&state=… HTTP/1.1"
        let target = request_line.split_whitespace().nth(1)?;
        target.split_once('?').map(|(_, q)| q.to_string())
    }

    fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        use std::io::Write;
        let reason = if status == 200 { "OK" } else { "No Content" };
        let _ = write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();
    }
}

/// Decode `%XX` and `+` in a query value.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&value[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Exchange an authorization code for tokens.
///
/// Returns a [`StoredCredential`] rather than saving one: persistence belongs to
/// the Resolver, which already handles refresh, rotation and revoke. A save here
/// would be a second storage path, which is exactly what this module promises
/// not to build.
pub async fn exchange_code(
    http: &reqwest::Client,
    config: &OAuthConfig,
    provider: &str,
    code: &str,
    pkce: &Pkce,
    redirect_uri: &str,
    now: &str,
) -> Result<StoredCredential, CredentialError> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", config.client_id.clone()),
        // The other half of PKCE. Without it a stolen code is spendable.
        ("code_verifier", pkce.verifier().expose().to_string()),
    ];
    // Only sent when there IS one — a public client has none, and sending an
    // empty secret makes some servers reject the request outright.
    if !config.client_secret.is_empty() {
        form.push(("client_secret", config.client_secret.expose().to_string()));
    }

    let resp = http
        .post(&config.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| CredentialError::Invalid(format!("token exchange transport failed: {e}")))?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        return Err(CredentialError::Invalid(format!(
            "token exchange for '{provider}' rejected with {status}: {body}"
        )));
    }

    let token: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CredentialError::Invalid(format!("token response unreadable: {e}")))?;

    let access = token
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CredentialError::Invalid("token response carried no access_token".into()))?;

    let expires_at = token
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .and_then(|secs| {
            chrono::DateTime::parse_from_rfc3339(now)
                .ok()
                .map(|t| (t + chrono::Duration::seconds(secs)).to_rfc3339())
        });

    Ok(StoredCredential {
        provider: provider.trim().to_ascii_lowercase(),
        grant: GrantKind::OAuth,
        // An OAuth access token is always a Bearer, whatever the provider does
        // with its personal keys.
        scheme: AuthScheme::Bearer,
        access: Secret::new(access),
        refresh: token
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .map(Secret::new),
        expires_at,
        // Persisted with the token because refresh needs it AFTER this process
        // exits: a PKCE public client refreshes with its client id alone.
        client_id: Some(config.client_id.clone()),
        // And a CONFIDENTIAL client needs its secret too. Kept only when there
        // is one, so a public client's record does not grow an empty field
        // that looks like a secret we lost.
        client_secret: (!config.client_secret.is_empty()).then(|| config.client_secret.clone()),
        // Resolved after this call, by whoever knows how to ask the provider —
        // the exchange has no opinion about resource scoping.
        site: None,
        updated_at: now.to_string(),
    })
}

/// How long to wait for a human to approve in the browser before giving up.
/// Long enough for a password manager and an SSO hop; short enough that an
/// abandoned flow does not wedge a terminal forever.
pub const REDIRECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Drive the whole flow after the browser has been opened: wait for the
/// redirect, verify the state, exchange the code.
///
/// Everything except opening the browser, so a test can drive it against a real
/// socket and a mock token endpoint. Returns a credential for the caller to hand
/// to the Resolver — this function deliberately does not save, because a second
/// persistence path is the one thing this module promises not to build.
pub async fn complete_authorization(
    http: &reqwest::Client,
    config: &OAuthConfig,
    provider: &str,
    receiver: LoopbackReceiver,
    pkce: &Pkce,
    expected_state: &str,
    now: &str,
) -> Result<StoredCredential, CredentialError> {
    let redirect_uri = receiver.redirect_uri()?;
    // Blocking accept on a dedicated thread so the async caller is not stalled
    // and a test can make the redirect request from the same runtime.
    let redirected = tokio::task::spawn_blocking(move || receiver.wait(REDIRECT_TIMEOUT))
        .await
        .map_err(|e| CredentialError::Invalid(format!("redirect listener failed: {e}")))??;

    // BEFORE the exchange: a code that arrived with the wrong state is not ours,
    // and spending it would be the bug this check exists to prevent.
    verify_state(expected_state, &redirected.state)?;

    exchange_code(
        http,
        config,
        provider,
        &redirected.code,
        pkce,
        &redirect_uri,
        now,
    )
    .await
}

/// Verify the CSRF state that came back.
///
/// Separate and public so it is impossible to forget: generating a state,
/// putting it in the URL and never checking it on the way back looks identical
/// to the correct implementation in any happy-path test.
pub fn verify_state(expected: &str, received: &str) -> Result<(), CredentialError> {
    if expected == received {
        return Ok(());
    }
    Err(CredentialError::Invalid(
        "the authorization response carried the wrong state value — refusing it. \
         This means the response did not come from the request this CLI started."
            .into(),
    ))
}

/// Linear's OAuth profile. Authorization-code with PKCE; no device flow.
#[must_use]
pub fn linear_oauth(client_id: &str) -> OAuthConfig {
    OAuthConfig {
        authorize_url: "https://linear.app/oauth/authorize".into(),
        token_url: "https://api.linear.app/oauth/token".into(),
        revoke_url: Some("https://api.linear.app/oauth/revoke".into()),
        client_id: client_id.into(),
        // Public client: a CLI has nowhere to keep a secret, so PKCE carries the
        // proof instead.
        client_secret: Secret::new(""),
        authorize_params: Vec::new(),
    }
}

/// Linear's app-actor OAuth profile: the same flow, with writes attributed to
/// the APPLICATION instead of the human who approved it.
///
/// `actor=app` is a property of the AUTHORIZATION, not of any mutation — Linear
/// ties it "to the authorization and its access token", so every write under
/// the resulting token is app-authored and there is no per-write choice to
/// model. (`actor=application` was the older spelling; Linear deprecated it in
/// favor of `actor=app`, verified live 2026-07-26.)
#[must_use]
pub fn linear_oauth_app(client_id: &str) -> OAuthConfig {
    let mut config = linear_oauth(client_id);
    config.authorize_params.push(("actor".into(), "app".into()));
    config
}

/// Rebuild the refresh configuration for a stored credential, when its
/// provider profile is known and the sign-in persisted a client id.
///
/// This is what makes refresh survive a process restart: the sign-in command
/// that knew the client id has exited by the time the token expires, so the
/// resolver re-derives the profile from what the store kept. `None` means the
/// credential cannot be refreshed from stored material alone — a pasted key,
/// or a provider with no registered profile.
///
/// # Jira needs a second thing, and that is the whole point
///
/// Linear refreshes with a client id alone. Atlassian requires the client
/// SECRET on the refresh as well, so a Jira credential whose record carries no
/// secret is not refreshable and this returns `None` for it — deliberately,
/// rather than handing back a profile with an empty secret that would fail at
/// the token endpoint with an opaque 400. `None` here surfaces as "no OAuth
/// configuration is registered to refresh it", which at least names the state.
#[must_use]
pub fn stored_refresh_profile(stored: &StoredCredential) -> Option<OAuthConfig> {
    let client_id = stored.client_id.as_deref()?;
    match stored.provider.as_str() {
        "linear" => Some(linear_oauth(client_id)),
        "jira" => {
            let secret = stored.client_secret.as_ref()?;
            Some(super::oauth::jira_3lo(client_id, secret.expose()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuthConfig {
        OAuthConfig {
            authorize_url: "https://example.test/authorize".into(),
            token_url: "https://example.test/token".into(),
            revoke_url: None,
            client_id: "client id/with chars".into(),
            client_secret: Secret::new(""),
            authorize_params: Vec::new(),
        }
    }

    /// S256, and the challenge must actually be the hash of the verifier — not
    /// the verifier itself, which is what `plain` would be and which we refuse.
    #[test]
    fn pkce_is_s256_and_the_challenge_hashes_the_verifier() {
        let p = Pkce::generate();
        assert_eq!(p.method(), "S256");
        assert_ne!(p.challenge(), p.verifier().expose());

        let expected = b64url(&Sha256::digest(p.verifier().expose().as_bytes()));
        assert_eq!(p.challenge(), expected);

        // RFC 7636 requires 43..=128 characters.
        assert!((43..=128).contains(&p.verifier().expose().len()));
        // URL-safe, unpadded.
        assert!(!p.challenge().contains('+'));
        assert!(!p.challenge().contains('/'));
        assert!(!p.challenge().contains('='));
    }

    #[test]
    fn each_pkce_pair_and_state_is_fresh() {
        assert_ne!(
            Pkce::generate().verifier().expose(),
            Pkce::generate().verifier().expose()
        );
        assert_ne!(new_state(), new_state());
    }

    /// The verifier is the secret half and must not print.
    #[test]
    fn the_verifier_does_not_print() {
        let p = Pkce::generate();
        let rendered = format!("{p:?}");
        assert!(!rendered.contains(p.verifier().expose()));
        // The challenge is public and useful when debugging a rejected exchange.
        assert!(rendered.contains(p.challenge()));
    }

    #[test]
    fn the_authorize_url_carries_every_required_parameter_encoded() {
        let p = Pkce::generate();
        let url = authorize_url(
            &config(),
            &p,
            "st-ate",
            "http://127.0.0.1:1234/callback",
            &["read", "write"],
        );

        assert!(url.starts_with("https://example.test/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", encode(p.challenge()))));
        assert!(url.contains("state=st-ate"));
        // Reserved characters survive: the space between scopes, and the
        // colons and slashes in the redirect URI.
        assert!(url.contains("scope=read%20write"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2Fcallback"));
        assert!(url.contains("client_id=client%20id%2Fwith%20chars"));
        // The verifier must NEVER appear in a URL the browser sees.
        assert!(!url.contains(p.verifier().expose()));
    }

    /// The assertion that catches a state check that was never written: a
    /// mismatch must be refused, and the message must say what it means.
    #[test]
    fn a_mismatched_state_is_refused() {
        assert!(verify_state("abc", "abc").is_ok());
        let err = verify_state("abc", "xyz").expect_err("must refuse");
        assert!(err.to_string().contains("did not come from the request"));
    }

    #[test]
    fn percent_and_plus_decode() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        // A malformed escape is preserved rather than panicking.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn linears_profile_is_a_public_client_with_pkce() {
        let cfg = linear_oauth("client-123");
        assert!(cfg.authorize_url.contains("linear.app/oauth/authorize"));
        assert!(
            cfg.client_secret.is_empty(),
            "a CLI is a public client — PKCE carries the proof, not a secret"
        );
        assert!(cfg.revoke_url.is_some());
    }

    /// The whole point of the app-actor profile: `actor=app` reaches the URL
    /// the human opens — and does NOT appear when the human signs in as
    /// themselves, because a stray actor param would silently flip attribution
    /// of every subsequent write.
    #[test]
    fn the_app_actor_profile_pins_actor_app_and_the_user_profile_does_not() {
        let p = Pkce::generate();
        let redirect = "http://127.0.0.1:1234/callback";

        let app_url = authorize_url(&linear_oauth_app("c-1"), &p, "st", redirect, &["read"]);
        assert!(app_url.contains("&actor=app"), "got: {app_url}");

        let user_url = authorize_url(&linear_oauth("c-1"), &p, "st", redirect, &["read"]);
        assert!(!user_url.contains("actor="), "got: {user_url}");
    }

    /// Pinned params are encoded like every other value — a profile with a
    /// reserved character must not corrupt the query string.
    #[test]
    fn pinned_authorize_params_are_percent_encoded() {
        let mut cfg = config();
        cfg.authorize_params
            .push(("aud ience".into(), "a&b=c".into()));
        let url = authorize_url(&cfg, &Pkce::generate(), "st", "http://x/cb", &[]);
        assert!(url.contains("&aud%20ience=a%26b%3Dc"), "got: {url}");
    }

    /// Refresh must survive the sign-in process exiting: the profile is
    /// re-derivable from what the store kept, and ONLY from that.
    #[test]
    fn a_stored_linear_credential_rebuilds_its_refresh_profile_from_its_client_id() {
        let mut stored = StoredCredential::personal_key(
            "linear",
            "tok",
            AuthScheme::Bearer,
            "2026-07-26T00:00:00Z",
        );
        assert!(
            stored_refresh_profile(&stored).is_none(),
            "no client id → no profile"
        );

        stored.client_id = Some("c-42".into());
        let cfg = stored_refresh_profile(&stored).expect("profile");
        assert_eq!(cfg.client_id, "c-42");
        assert!(cfg.token_url.contains("api.linear.app/oauth/token"));

        stored.provider = "unknowable".into();
        assert!(
            stored_refresh_profile(&stored).is_none(),
            "unknown provider → no profile"
        );
    }

    /// THE GAP THIS CHUNK EXISTS TO CLOSE, at the level of one function. A Jira
    /// credential is refreshable from the store ONLY if the store kept the
    /// client secret — Atlassian requires it on the refresh. A profile rebuilt
    /// with an empty secret would look correct here and fail at the token
    /// endpoint days later, so the absence is reported rather than papered over.
    #[test]
    fn a_stored_jira_credential_needs_its_secret_to_rebuild_a_refresh_profile() {
        let mut stored = StoredCredential::personal_key(
            "jira",
            "tok",
            AuthScheme::Bearer,
            "2026-07-28T00:00:00Z",
        );
        stored.client_id = Some("c-42".into());

        assert!(
            stored_refresh_profile(&stored).is_none(),
            "a confidential client with no stored secret is NOT refreshable, and saying so \
             beats returning a profile that 400s at the token endpoint"
        );

        stored.client_secret = Some(Secret::new("app-secret"));
        let cfg = stored_refresh_profile(&stored).expect("profile");
        assert_eq!(cfg.client_id, "c-42");
        assert_eq!(cfg.client_secret.expose(), "app-secret");
        assert!(cfg.token_url.contains("auth.atlassian.com"));
    }
}
