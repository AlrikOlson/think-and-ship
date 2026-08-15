//! Credential custody that actually holds.
//!
//! Two of these criteria are the kind that pass trivially if you assert the
//! wrong thing, so they are asserted on consequences instead:
//!
//! - **Revoke** is not "revoke returned Ok". It is "the next call FAILS", plus
//!   the provider's revocation endpoint having actually been hit. This codebase
//!   has already shipped rotate-without-revoke once, so the weaker assertion is
//!   the one that would have missed it.
//! - **Rotation** is not "refresh returned a token". Jira 3LO invalidates the
//!   old refresh token on every refresh, so the test asserts the NEW refresh
//!   token was persisted — an implementation that keeps the original passes a
//!   naive test and locks the user out one refresh later.

use std::sync::Arc;

use serde_json::json;
use think_and_ship::tracker::credential::{
    AuthScheme, CredentialPort, CredentialStore, FileCredentialStore, GrantKind, OAuthConfig,
    Resolver, Secret, StoredCredential,
};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NOW: &str = "2026-07-26T12:00:00+00:00";
const LATER: &str = "2026-07-26T13:00:00+00:00";

fn store(dir: &tempfile::TempDir) -> Arc<FileCredentialStore> {
    Arc::new(FileCredentialStore::new(dir.path()))
}

fn oauth_config(server: &MockServer, revocable: bool) -> OAuthConfig {
    OAuthConfig {
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/oauth/token", server.uri()),
        revoke_url: revocable.then(|| format!("{}/oauth/revoke", server.uri())),
        client_id: "client-id".into(),
        client_secret: Secret::new("client-secret"),
        authorize_params: Vec::new(),
    }
}

/// An OAuth credential that expired an hour ago.
fn expired_oauth(provider: &str) -> StoredCredential {
    StoredCredential {
        provider: provider.into(),
        grant: GrantKind::OAuth,
        scheme: AuthScheme::Bearer,
        access: Secret::new("stale-access"),
        refresh: Some(Secret::new("original-refresh")),
        expires_at: Some("2026-07-26T11:00:00+00:00".into()),
        client_id: Some("client-id".into()),
        client_secret: None,
        site: None,
        updated_at: NOW.into(),
    }
}

/// An expired token is refreshed transparently: the caller asks for a
/// credential and gets a working one, never learning a refresh happened.
#[tokio::test]
async fn an_expired_credential_is_refreshed_mid_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "refresh_token": "rotated-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    let cred = Resolver::new(s.clone())
        .with_oauth("jira", oauth_config(&server, false))
        .at(NOW)
        .credential("jira")
        .await
        .expect("resolve");

    assert_eq!(cred.header_value(), "Bearer fresh-access");

    // The refresh actually used the stored refresh token.
    let sent = server.received_requests().await.expect("recorded");
    assert_eq!(sent.len(), 1);
    let body = String::from_utf8_lossy(&sent[0].body);
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("original-refresh"));
}

/// ROTATION. Jira 3LO returns a NEW refresh token and kills the old one. An
/// implementation that keeps the original passes any test that only checks the
/// access token — and then fails on the SECOND refresh, days later.
#[tokio::test]
async fn a_rotated_refresh_token_replaces_the_one_that_is_now_dead() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "refresh_token": "rotated-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    Resolver::new(s.clone())
        .with_oauth("jira", oauth_config(&server, false))
        .at(NOW)
        .credential("jira")
        .await
        .expect("resolve");

    let persisted = s.load("jira").expect("load").expect("present");
    assert_eq!(
        persisted.refresh.as_ref().map(Secret::expose),
        Some("rotated-refresh"),
        "the rotated refresh token must be persisted — the original is already dead"
    );
    // The client id survives the rotation: dropping it here would make the
    // SECOND refresh unconfigurable after a process restart.
    assert_eq!(persisted.client_id.as_deref(), Some("client-id"));
    // …and the new expiry was computed from the injected clock, not guessed.
    assert_eq!(
        persisted.expires_at.as_deref(),
        Some("2026-07-26T13:00:00+00:00")
    );
    assert_eq!(persisted.expires_at.as_deref(), Some(LATER));
}

/// A provider that does NOT rotate (Linear) must keep its existing refresh
/// token rather than losing it to a `None`.
#[tokio::test]
async fn a_non_rotating_provider_keeps_its_existing_refresh_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("linear")).expect("seed");

    Resolver::new(s.clone())
        .with_oauth("linear", oauth_config(&server, false))
        .at(NOW)
        .credential("linear")
        .await
        .expect("resolve");

    let persisted = s.load("linear").expect("load").expect("present");
    assert_eq!(
        persisted.refresh.as_ref().map(Secret::expose),
        Some("original-refresh"),
        "no rotation means keep what we had, not drop it"
    );
}

/// A dead refresh token must produce an actionable error, not a bare status.
#[tokio::test]
async fn a_rejected_refresh_says_what_to_do_about_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("invalid_grant"))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    let err = Resolver::new(s)
        .with_oauth("jira", oauth_config(&server, false))
        .at(NOW)
        .credential("jira")
        .await
        .expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("reconnect the provider"), "got: {msg}");
}

/// REVOKE, asserted on the consequence. Not "revoke returned Ok" — the next
/// call must FAIL, and the provider must actually have been told.
#[tokio::test]
async fn revoke_calls_the_provider_and_the_next_call_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .and(body_string_contains("stale-access"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    let resolver = Resolver::new(s.clone())
        .with_oauth("jira", oauth_config(&server, true))
        .at(NOW);

    assert!(
        resolver.revoke("jira").await.expect("revoke"),
        "the provider has a revocation endpoint, so it must be reported as called"
    );

    // The provider was told, with the token.
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);

    // And the credential is genuinely gone — this is the assertion that would
    // have caught alias-revoke.
    assert!(s.load("jira").expect("load").is_none());
    assert!(resolver.credential("jira").await.is_err());
}

/// Even when the provider call FAILS, the local credential must be forgotten —
/// a token we can no longer manage must not stay on disk. The error is still
/// surfaced so the human knows to finish the job in the provider's UI.
#[tokio::test]
async fn a_failed_remote_revoke_still_forgets_the_local_credential() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    let resolver = Resolver::new(s.clone())
        .with_oauth("jira", oauth_config(&server, true))
        .at(NOW);

    let result = resolver.revoke("jira").await;
    assert!(result.is_err(), "the remote failure must be surfaced");
    assert!(
        s.load("jira").expect("load").is_none(),
        "the credential must be forgotten regardless — leaving it is how alias-revoke happened"
    );
}

/// A provider with no revocation endpoint (Jira 3LO) reports `false` rather
/// than implying the token is dead. Local forgetting still happens.
#[tokio::test]
async fn a_provider_without_a_revocation_endpoint_says_so_rather_than_pretending() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed");

    let called = Resolver::new(s.clone())
        .with_oauth(
            "jira",
            think_and_ship::tracker::credential::oauth::jira_3lo("id", "secret"),
        )
        .at(NOW)
        .revoke("jira")
        .await
        .expect("revoke");

    assert!(
        !called,
        "Atlassian documents no 3LO revocation endpoint — claiming we called one would be a lie"
    );
    assert!(s.load("jira").expect("load").is_none());
}

/// THE containment guarantee: a secret must not reach the roadmap store, the
/// git-mirrored partition, or an export. It holds structurally — the roadmap
/// types have nowhere to put one — and this asserts it on the real serialized
/// forms rather than trusting the design.
#[test]
fn no_secret_reaches_the_roadmap_store_or_its_export() {
    use think_and_ship::roadmap::domain::ChunkStatus;
    use think_and_ship::roadmap::engine::RoadmapEngine;

    const SECRET: &str = "lin_api_THIS_MUST_NEVER_APPEAR";

    let dir = tempfile::TempDir::new().expect("tempdir");
    let resolver = Resolver::new(store(&dir)).at(NOW);
    resolver
        .connect_personal_key("linear", SECRET, AuthScheme::Raw)
        .expect("connect");

    // A fully-populated roadmap: a chunk, bound to a tracker, with relations.
    let mut e = RoadmapEngine::new("proj".into());
    e.add_chunk(
        "c1".into(),
        "Chunk c1".into(),
        ChunkStatus::Pending,
        10,
        "why".into(),
        vec!["it works".into()],
        vec![],
        false,
    )
    .expect("add");
    e.set_tracker_opt_in("c1", "linear", true).expect("opt in");
    e.record_tracker_link("c1", "linear", "ENG-1", "hash", Some("v1"))
        .expect("link");
    e.record_tracker_relations("c1", "linear", "rel-hash")
        .expect("relations");

    // Every serialized form the roadmap can produce.
    let roadmap_json = serde_json::to_string(e.roadmap()).expect("serialize roadmap");
    let markdown = e.export("markdown");
    let json_export = e.export("json");

    for (name, rendered) in [
        ("roadmap store", &roadmap_json),
        ("markdown export", &markdown),
        ("json export", &json_export),
    ] {
        assert!(
            !rendered.contains(SECRET),
            "the secret leaked into the {name}"
        );
        assert!(
            !rendered.contains("lin_api"),
            "even a token PREFIX leaked into the {name}"
        );
    }
}

/// The credential file lives outside the roadmap store entirely, so a
/// git-mirrored roadmap partition cannot pick it up.
#[test]
fn credentials_live_outside_the_roadmap_store() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let resolver = Resolver::new(store(&dir)).at(NOW);
    resolver
        .connect_personal_key("github", "ghp_secret", AuthScheme::Bearer)
        .expect("connect");

    let cred_dir = dir.path().join("tracker").join("credentials");
    assert!(cred_dir.is_dir(), "credentials have their own directory");

    // Nothing under the roadmap partition, which is what repo-git mirrors.
    let roadmap_dir = dir.path().join("roadmap");
    assert!(
        !roadmap_dir.exists(),
        "storing a credential must not create anything under the roadmap store"
    );
}

/// The port is grant-blind: the same call site serves a pasted key and an OAuth
/// token, and the caller cannot tell which it got.
#[tokio::test]
async fn one_call_site_serves_both_grant_kinds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    s.save(&expired_oauth("jira")).expect("seed oauth");

    let resolver = Resolver::new(s)
        .with_oauth("jira", oauth_config(&server, false))
        .at(NOW);
    resolver
        .connect_personal_key("linear", "lin_api_x", AuthScheme::Raw)
        .expect("connect key");

    // Identical call shape; the caller learns nothing about the grant.
    let port: &dyn CredentialPort = &resolver;
    assert_eq!(
        port.credential("linear")
            .await
            .expect("linear")
            .header_value(),
        "lin_api_x"
    );
    assert_eq!(
        port.credential("jira").await.expect("jira").header_value(),
        "Bearer fresh-access"
    );
}

// ---------------------------------------------------------------------------
// Authorization-code flow (tracker-oauth-connect-flow)
// ---------------------------------------------------------------------------
//
// These drive a REAL loopback socket rather than mocking it: the test itself
// plays the browser and makes the redirect request. That exercises the actual
// bind, the actual HTTP parse and the actual response write, none of which a
// mock would cover — and those are exactly where a hand-rolled listener breaks.

use think_and_ship::tracker::credential::authcode::{
    LoopbackReceiver, Pkce, authorize_url, complete_authorization, linear_oauth, linear_oauth_app,
    new_state,
};

fn authcode_config(server: &MockServer) -> OAuthConfig {
    OAuthConfig {
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/oauth/token", server.uri()),
        revoke_url: None,
        client_id: "cli-client".into(),
        // Public client: PKCE carries the proof instead of a secret.
        client_secret: Secret::new(""),
        authorize_params: Vec::new(),
    }
}

/// Play the browser: hit the redirect URI the way a provider would.
fn redirect_to_raw(port: u16, query: &str) {
    let query = query.to_string();
    std::thread::spawn(move || {
        use std::io::Write;
        // Retry briefly: the listener binds before the test thread starts.
        for _ in 0..50 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let _ = write!(
                    s,
                    "GET /callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n"
                );
                let _ = s.flush();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
}

/// The whole point of the flow: a human obtains a token without ever
/// copy-pasting a code, and it lands in the SAME store the Resolver refreshes.
#[tokio::test]
async fn a_token_is_obtained_end_to_end_and_stored_by_the_resolver() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "obtained-access",
            "refresh_token": "obtained-refresh",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let port = receiver.port().expect("port");
    let pkce = Pkce::generate();
    let state = new_state();

    // The provider redirects the browser back.
    redirect_to_raw(port, &format!("code=the-code&state={state}"));

    let obtained = complete_authorization(
        &reqwest::Client::new(),
        &authcode_config(&server),
        "linear",
        receiver,
        &pkce,
        &state,
        NOW,
    )
    .await
    .expect("the flow completes");

    assert_eq!(obtained.access.expose(), "obtained-access");
    assert_eq!(obtained.grant, GrantKind::OAuth);
    // An OAuth access token is a Bearer even for Linear, whose personal keys
    // are raw.
    assert_eq!(obtained.scheme, AuthScheme::Bearer);
    assert_eq!(obtained.expires_at.as_deref(), Some(LATER));
    // The client id lands in the record: refresh needs it after this process
    // exits, and the sign-in command is the only thing that ever knew it.
    assert_eq!(obtained.client_id.as_deref(), Some("cli-client"));

    // The exchange carried the PKCE verifier — without it a stolen code is
    // spendable by anyone who saw the redirect.
    let sent = server.received_requests().await.expect("recorded");
    let body = String::from_utf8_lossy(&sent[0].body);
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains("code_verifier="));
    assert!(body.contains(pkce.verifier().expose()));
    // A public client sends no secret; an empty one makes some servers refuse.
    assert!(!body.contains("client_secret"));

    // THE premise being proved: one storage path. Adopt, then resolve through
    // the ordinary port with no special casing.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let s = store(&dir);
    let resolver = Resolver::new(s.clone()).at(NOW);
    resolver.adopt(&obtained).expect("adopt");
    assert_eq!(
        resolver
            .credential("linear")
            .await
            .expect("resolve")
            .header_value(),
        "Bearer obtained-access"
    );
    // …and it is the same record the refresh machinery would renew.
    assert_eq!(
        s.load("linear")
            .expect("load")
            .expect("present")
            .refresh
            .as_ref()
            .map(Secret::expose),
        Some("obtained-refresh")
    );
}

/// The check most likely to be written and never wired: a code that arrives
/// with the wrong state is not ours, and spending it would be the bug.
#[tokio::test]
async fn a_code_arriving_with_the_wrong_state_is_refused_before_the_exchange() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "must-never-be-requested"
        })))
        .mount(&server)
        .await;

    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let port = receiver.port().expect("port");
    redirect_to_raw(port, "code=the-code&state=a-state-we-did-not-issue");

    let err = complete_authorization(
        &reqwest::Client::new(),
        &authcode_config(&server),
        "linear",
        receiver,
        &Pkce::generate(),
        "the-state-we-issued",
        NOW,
    )
    .await
    .expect_err("must refuse");

    assert!(err.to_string().contains("did not come from the request"));
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "the code must NOT be exchanged — refusal has to happen first"
    );
}

/// A provider that refuses consent redirects with `error=`, and the human must
/// be told what happened rather than seeing a parse failure.
#[tokio::test]
async fn a_denied_consent_is_reported_as_a_refusal() {
    let server = MockServer::start().await;
    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let port = receiver.port().expect("port");
    redirect_to_raw(port, "error=access_denied");

    let err = complete_authorization(
        &reqwest::Client::new(),
        &authcode_config(&server),
        "linear",
        receiver,
        &Pkce::generate(),
        "state",
        NOW,
    )
    .await
    .expect_err("must fail");
    assert!(err.to_string().contains("access_denied"));
}

/// The redirect URI the provider is given must be the one actually listening.
#[test]
fn the_redirect_uri_names_the_port_that_is_bound() {
    let r = LoopbackReceiver::bind(0).expect("bind");
    let port = r.port().expect("port");
    assert_ne!(port, 0, "an ephemeral bind must resolve to a real port");
    assert_eq!(
        r.redirect_uri().expect("uri"),
        format!("http://127.0.0.1:{port}/callback")
    );
}

/// Linear's real profile, as the CLI will use it.
#[test]
fn the_linear_authorize_url_is_well_formed() {
    let pkce = Pkce::generate();
    let url = authorize_url(
        &linear_oauth("client-abc"),
        &pkce,
        "st",
        "http://127.0.0.1:9999/callback",
        &["read", "write", "issues:create"],
    );
    assert!(url.starts_with("https://linear.app/oauth/authorize?"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("scope=read%20write%20issues%3Acreate"));
}

/// The app-actor profile, as the CLI will use it: the SAME flow with one more
/// pinned parameter, and attribution is the only difference. Linear ties
/// `actor=app` to the authorization and its token — every write under the
/// resulting credential is app-authored.
#[test]
fn the_linear_app_actor_url_differs_only_by_the_actor_parameter() {
    let pkce = Pkce::generate();
    let build = |config| {
        authorize_url(
            &config,
            &pkce,
            "st",
            "http://127.0.0.1:9999/callback",
            &["read", "write", "issues:create"],
        )
    };
    let user_url = build(linear_oauth("client-abc"));
    let app_url = build(linear_oauth_app("client-abc"));
    assert_eq!(app_url, format!("{user_url}&actor=app"));
}

/// THE defect this test exists for. The first version accepted exactly one
/// connection, which is what a well-behaved test client sends and what a real
/// browser does not. Chrome and Safari open speculative connections and fetch
/// /favicon.ico; whichever wins the single accept consumed it, and the real
/// redirect then arrived at a socket nobody was listening on.
///
/// This drives the noise a real browser produces BEFORE the redirect and
/// asserts the flow still completes. A mock client that sends exactly one
/// correct request — which is what the original test did — cannot catch this.
#[tokio::test]
async fn the_receiver_survives_the_noise_a_real_browser_makes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "survived-the-noise"
        })))
        .mount(&server)
        .await;

    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let port = receiver.port().expect("port");
    let state = new_state();

    let noisy_state = state.clone();
    std::thread::spawn(move || {
        use std::io::Write;
        let connect = || {
            for _ in 0..50 {
                if let Ok(s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                    return Some(s);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            None
        };

        // 1. A favicon fetch — no query at all.
        if let Some(mut s) = connect() {
            let _ = write!(s, "GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let _ = s.flush();
        }
        // 2. A speculative connection that opens and says nothing useful.
        if let Some(mut s) = connect() {
            let _ = write!(s, "GET /?utm=noise HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let _ = s.flush();
        }
        // 3. Only NOW the real redirect.
        if let Some(mut s) = connect() {
            let _ = write!(
                s,
                "GET /callback?code=real-code&state={noisy_state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            );
            let _ = s.flush();
        }
    });

    let obtained = complete_authorization(
        &reqwest::Client::new(),
        &authcode_config(&server),
        "linear",
        receiver,
        &Pkce::generate(),
        &state,
        NOW,
    )
    .await
    .expect("the flow must survive browser noise");

    assert_eq!(obtained.access.expose(), "survived-the-noise");
}

/// A loop with no deadline is a worse hang than the bug it replaces. A user who
/// closes the tab without approving must get a message, not a wedged terminal.
#[test]
fn the_receiver_gives_up_rather_than_waiting_forever() {
    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let started = std::time::Instant::now();

    let err = receiver
        .wait(std::time::Duration::from_millis(300))
        .expect_err("must give up");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "it returned, but not promptly: {:?}",
        started.elapsed()
    );
    let msg = err.to_string();
    assert!(
        msg.contains("browser") || msg.contains("approval"),
        "the message must tell a human what to do, got: {msg}"
    );
}

/// A code with no state is refused rather than silently accepted — the state is
/// what proves the response belongs to the request this CLI started.
#[tokio::test]
async fn a_code_with_no_state_at_all_is_refused() {
    let server = MockServer::start().await;
    let receiver = LoopbackReceiver::bind(0).expect("bind");
    let port = receiver.port().expect("port");
    redirect_to_raw(port, "code=orphan-code");

    let err = complete_authorization(
        &reqwest::Client::new(),
        &authcode_config(&server),
        "linear",
        receiver,
        &Pkce::generate(),
        "expected",
        NOW,
    )
    .await
    .expect_err("must refuse");
    assert!(err.to_string().contains("no state"));
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

/// # The rotation proof still owed after the in-process case
///
/// Rotation is already proven above for a resolver someone configured
/// IN-PROCESS. That is not the case that breaks. The case that breaks was
/// found the hard way once already: the sign-in command has exited, the
/// token expires hours later, and the ONLY material available is what the store
/// kept. For Jira that is strictly harder than for Linear, because Atlassian
/// requires the client SECRET on the refresh — so a store that kept only the
/// client id yields a credential that cannot be renewed at all.
///
/// This drives the whole loop from the store alone: rebuild the profile with
/// `stored_refresh_profile`, refresh with it, and assert three things that each
/// fail differently — the secret reached the wire, the rotated refresh token was
/// persisted, and the material needed for the SECOND refresh survived the first.
mod jira_store_only_refresh {
    use super::*;
    use think_and_ship::tracker::credential::authcode::stored_refresh_profile;

    fn expired_jira() -> StoredCredential {
        StoredCredential {
            provider: "jira".into(),
            grant: GrantKind::OAuth,
            scheme: AuthScheme::Bearer,
            access: Secret::new("stale-access"),
            refresh: Some(Secret::new("original-refresh")),
            expires_at: Some("2026-07-26T11:00:00+00:00".into()),
            client_id: Some("atlassian-client".into()),
            client_secret: Some(Secret::new("atlassian-secret")),
            site: Some("cloud-abc".into()),
            updated_at: NOW.into(),
        }
    }

    #[tokio::test]
    async fn a_jira_credential_refreshes_from_the_store_alone_and_survives_for_the_next_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh-access",
                "refresh_token": "rotated-refresh",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let s = store(&dir);
        s.save(&expired_jira()).expect("seed");

        // THE POINT: the config is rebuilt from the STORED record, exactly as a
        // fresh process would have to. Only the host is redirected at the mock;
        // every credential value below comes from disk.
        let reloaded = s.load("jira").expect("load").expect("present");
        let mut config = stored_refresh_profile(&reloaded)
            .expect("a stored Jira credential must be refreshable from what the store kept");
        assert!(
            config.token_url.contains("auth.atlassian.com"),
            "the rebuilt profile must be the real Atlassian one before we redirect it"
        );
        config.token_url = format!("{}/oauth/token", server.uri());

        let cred = Resolver::new(s.clone())
            .with_oauth("jira", config)
            .at(NOW)
            .credential("jira")
            .await
            .expect("resolve");

        assert_eq!(cred.header_value(), "Bearer fresh-access");
        // The cloudid crosses the port: without it a Jira adapter has no URL.
        assert_eq!(cred.site(), Some("cloud-abc"));

        // The confidential-client secret reached the wire from the STORE, not
        // from a config someone happened to still hold in memory.
        let sent = server.received_requests().await.expect("recorded");
        assert_eq!(sent.len(), 1);
        let body = String::from_utf8_lossy(&sent[0].body);
        assert!(body.contains("grant_type=refresh_token"), "got: {body}");
        assert!(
            body.contains("client_secret=atlassian-secret"),
            "got: {body}"
        );
        assert!(body.contains("original-refresh"), "got: {body}");

        let persisted = s.load("jira").expect("load").expect("present");
        assert_eq!(
            persisted.refresh.as_ref().map(Secret::expose),
            Some("rotated-refresh"),
            "Atlassian killed the original the moment it was spent"
        );
        // Everything the SECOND refresh will need, days from now, in another
        // process. Dropping any of these passes this test's first half and
        // locks the user out on the next expiry.
        assert_eq!(persisted.client_id.as_deref(), Some("atlassian-client"));
        assert_eq!(
            persisted.client_secret.as_ref().map(Secret::expose),
            Some("atlassian-secret")
        );
        assert_eq!(persisted.site.as_deref(), Some("cloud-abc"));
        assert!(
            stored_refresh_profile(&persisted).is_some(),
            "the credential must still be refreshable after being refreshed once"
        );
    }

    /// The negative half, and the one that would have caught the 24-hour
    /// credential: a Jira record with no stored secret is NOT refreshable, and
    /// the failure says so at resolve time rather than as an opaque 400.
    #[tokio::test]
    async fn a_jira_credential_without_its_secret_cannot_be_refreshed_and_says_so() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let s = store(&dir);
        let secretless = StoredCredential {
            client_secret: None,
            ..expired_jira()
        };
        s.save(&secretless).expect("seed");

        let reloaded = s.load("jira").expect("load").expect("present");
        assert!(
            stored_refresh_profile(&reloaded).is_none(),
            "no secret means no profile — a profile with an empty secret would 400 remotely"
        );

        // And with no profile registered, the resolver refuses in words.
        let err = Resolver::new(s)
            .at(NOW)
            .credential("jira")
            .await
            .expect_err("must fail");
        assert!(
            err.to_string()
                .contains("no OAuth configuration is registered"),
            "got: {err}"
        );
    }
}

/// The accessible-resources call over a real HTTP client: the Bearer header is
/// sent, the array is parsed, and a refusal is surfaced with its status rather
/// than as an empty list — an empty list would read as "you have no sites",
/// which is a different and much more confusing problem.
mod accessible_resources_wire {
    use super::*;
    use think_and_ship::tracker::credential::atlassian::{accessible_resources_at, select_site};
    use wiremock::matchers::header;

    #[tokio::test]
    async fn the_lookup_sends_the_bearer_token_and_parses_the_sites() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oauth/token/accessible-resources"))
            .and(header("authorization", "Bearer at-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": "cloud-1",
                    "name": "acme",
                    "url": "https://acme.atlassian.net",
                    "scopes": ["write:jira-work"],
                    "avatarUrl": "https://example.test/a.png"
                }
            ])))
            .mount(&server)
            .await;

        let sites = accessible_resources_at(&reqwest::Client::new(), &server.uri(), "at-1")
            .await
            .expect("lookup");

        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].id, "cloud-1");
        // An unknown field (avatarUrl) must not break parsing — Atlassian adds
        // fields without asking.
        assert_eq!(sites[0].url, "https://acme.atlassian.net");
        assert_eq!(select_site(&sites, None).expect("one site").id, "cloud-1");
    }

    #[tokio::test]
    async fn a_refused_lookup_names_the_status_and_the_likely_cause() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oauth/token/accessible-resources"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"message":"nope"}"#))
            .mount(&server)
            .await;

        let err = accessible_resources_at(&reqwest::Client::new(), &server.uri(), "at-1")
            .await
            .expect_err("must fail");
        let msg = err.to_string();
        assert!(msg.contains("401"), "got: {msg}");
        assert!(msg.contains("scopes"), "got: {msg}");
    }
}
