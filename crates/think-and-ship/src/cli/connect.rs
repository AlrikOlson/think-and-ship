//! `think-and-ship connect` — zero-paste device-flow login.
//!
//! The IO wire over the pure [`crate::cloud::device_flow`] core. End to end:
//!   1. `GET {url}/v1/connect-config` — the public, zero-config source for the
//!      WorkOS client id + authorize base + the per-tenant cloud URL.
//!   2. RFC 8628 device authorization against WorkOS (`device_authorization` →
//!      poll `token`), driven by the core's [`run_poll_loop`].
//!   3. Exchange the WorkOS access token at `POST {cloud_url}/v1/agent-token`
//!      (the worker gate resolves the tenant from the JWT) for a long-lived
//!      agent CLOUD_TOKEN, named for the client and machine being connected so
//!      the workspace can show it as a connection (see [`connection_name`]).
//!   4. Adopt the token into the credential store, resolve it back out, and
//!      prove it with one authenticated request — "Connected" is a verified
//!      fact, never a hope (see [`finish_connect_in`]).
//!   5. Write a cloud-configured MCP entry (reusing `setup`'s merge path) and
//!      close by naming the ONE resolved client and its reload requirement.
//!
//! So a new machine is connected without ever pasting a token by hand. The live
//! WorkOS round-trip needs real credentials and is deferred to manual
//! verification; the reqwest transport + the two backend calls are covered by
//! wiremock tests below, and the loop itself by the core's mock-transport tests.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::setup;
use crate::cloud::device_flow::{
    DeviceAuthResponse, DeviceFlowError, DeviceTransport, PollStatus, Sleeper, TokenErrorBody,
    TokenPoll, run_poll_loop,
};

/// The production backend used when `--url` is omitted (the connect-config
/// endpoint exists precisely so `connect` is otherwise zero-config).
///
/// This is a name the product controls, and that is the requirement rather than
/// a preference. `connect` prints this host and — because it adopts the
/// `cloud_url` the endpoint reports — WRITES it into every MCP config it
/// authors, so the hostname outlives the session in a file the user keeps.
///
/// There is deliberately NO built-in default. A host written into a file the
/// user keeps is the operator's choice to make, and a self-hosted deployment
/// is named exactly the way a hosted one is: here, by whoever runs it.
const CLOUD_URL_ENV: &str = "TAS_CLOUD_URL";

/// Resolve the cloud backend: `--cloud-url` first, then [`CLOUD_URL_ENV`], then
/// fail with both named.
///
/// Pure in its inputs so the resolution order and every rejection are testable
/// without touching the process environment — the caller reads the variable.
///
/// The properties enforced here used to be asserted about a hardcoded default.
/// They are checks on the resolved value instead, so they hold for whatever a
/// user supplies rather than only for a constant a test could restate. `http`
/// is accepted for loopback alone: a self-hoster needs it to develop against a
/// local worker, and nobody else should be shipping a bearer token in clear.
fn resolve_cloud_url(flag: Option<&str>, env: Option<&str>) -> Result<String> {
    let raw = match flag.or(env) {
        Some(u) => u.trim().trim_end_matches('/').to_string(),
        None => bail!(
            "no cloud backend configured.\n\
             Pass --url <URL>, or set {CLOUD_URL_ENV}.\n\
             \n\
             Example:\n\
             \x20   {CLOUD_URL_ENV}=https://api.example.com think-and-ship connect"
        ),
    };
    if raw.is_empty() {
        bail!("the cloud backend URL is empty; pass --url <URL> or set {CLOUD_URL_ENV}");
    }
    let loopback = raw.starts_with("http://localhost") || raw.starts_with("http://127.0.0.1");
    if !raw.starts_with("https://") && !loopback {
        bail!(
            "the cloud backend URL must be https (got {raw}); \
             http is accepted only for localhost"
        );
    }
    Ok(raw)
}

/// RFC 8628 §3.4 grant type for the token-poll request.
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The public `GET /v1/connect-config` response (see `backend/connect-config.ts`).
#[derive(Debug, Deserialize)]
struct ConnectConfig {
    /// The per-tenant cloud API base the agent will sync to (the request origin).
    cloud_url: String,
    /// The WorkOS client id to authorize with; `null` when sign-in isn't enabled.
    #[serde(default)]
    workos_client_id: Option<String>,
    /// The WorkOS authorize issuer base (`{base}/device_authorization`, `{base}/token`).
    workos_authorize_base: String,
}

/// The WorkOS token endpoint's 2xx body — we only need the access token.
#[derive(Debug, Deserialize)]
struct WorkosTokenResponse {
    access_token: String,
}

/// The backend's `POST /v1/agent-token` 2xx body (`handleMintAgentToken`).
///
/// `jti` is the registration's identity in the workspace registry — the handle
/// `POST /v1/agent-tokens/{jti}/revoke` takes. It is kept alongside the token
/// because a connect that fails between the mint and a working local credential
/// is the ONLY holder of the value that can undo what the mint registered.
#[derive(Debug, Deserialize)]
struct AgentTokenResponse {
    token: String,
    jti: String,
}

/// Where WorkOS starts the device grant, relative to `authorize_base`.
///
/// NOT the RFC 8628 §3.1 spelling. The RFC names no path — it only describes a
/// "device authorization endpoint" — and WorkOS's AuthKit CLI Auth serves it at
/// `/authorize/device`. Guessing `/device_authorization` from the RFC's prose is
/// what made every connect fail with a 404 until a real run met a real WorkOS.
const DEVICE_AUTHORIZE_PATH: &str = "/authorize/device";

/// Where WorkOS exchanges an approved device code for a token.
///
/// WorkOS reuses its own `/authenticate` endpoint for every grant type rather
/// than exposing a separate OAuth `/token`, so the device grant lands here.
const DEVICE_TOKEN_PATH: &str = "/authenticate";

/// A live reqwest [`DeviceTransport`] against WorkOS at `authorize_base`.
struct ReqwestDeviceTransport {
    http: reqwest::Client,
    authorize_base: String,
    client_id: String,
}

impl DeviceTransport for ReqwestDeviceTransport {
    async fn device_authorize(&self) -> Result<DeviceAuthResponse, DeviceFlowError> {
        let resp = self
            .http
            .post(format!("{}{DEVICE_AUTHORIZE_PATH}", self.authorize_base))
            .form(&[("client_id", self.client_id.as_str())])
            .send()
            .await
            .map_err(|_| {
                DeviceFlowError::Unexpected(
                    "could not reach the sign-in service. Check your network connection \
                     and run `think-and-ship connect` again."
                        .into(),
                )
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(DeviceFlowError::Unexpected(format!(
                "device authorization returned {}: {body}",
                status.as_u16()
            )));
        }
        resp.json::<DeviceAuthResponse>().await.map_err(|e| {
            DeviceFlowError::Unexpected(format!("invalid device authorization response: {e}"))
        })
    }

    async fn poll_token(&self, device_code: &str) -> Result<TokenPoll, DeviceFlowError> {
        let resp = self
            .http
            .post(format!("{}{DEVICE_TOKEN_PATH}", self.authorize_base))
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", DEVICE_GRANT_TYPE),
                ("device_code", device_code),
            ])
            .send()
            .await
            .map_err(|_| {
                DeviceFlowError::Unexpected(
                    "lost the connection to the sign-in service while waiting for approval. \
                     Check your network connection and run `think-and-ship connect` again."
                        .into(),
                )
            })?;
        if resp.status().is_success() {
            let body = resp
                .json::<WorkosTokenResponse>()
                .await
                .map_err(|e| DeviceFlowError::Unexpected(format!("invalid token response: {e}")))?;
            Ok(TokenPoll::Granted(body.access_token))
        } else {
            // RFC 8628 §3.5 error body: { "error": "<code>" }.
            let body = resp.json::<TokenErrorBody>().await.map_err(|e| {
                DeviceFlowError::Unexpected(format!("invalid token error body: {e}"))
            })?;
            Ok(TokenPoll::Status(PollStatus::from_error(&body.error)))
        }
    }
}

/// The live sleeper — wraps `tokio::time::sleep` for the real poll cadence.
struct TokioSleeper;

impl Sleeper for TokioSleeper {
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}

/// Fetch the public connect-config (no auth) from `{base_url}/v1/connect-config`.
async fn fetch_connect_config(http: &reqwest::Client, base_url: &str) -> Result<ConnectConfig> {
    let resp = http
        .get(format!(
            "{}/v1/connect-config",
            base_url.trim_end_matches('/')
        ))
        .send()
        .await
        // The reqwest chain names DNS/TLS/socket internals nobody at the
        // terminal can act on; the one usable action is the same either way.
        .map_err(|_| {
            anyhow::anyhow!(
                "could not reach the backend at {base_url}. Check your network \
                 connection and run `think-and-ship connect` again."
            )
        })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "the backend at {base} could not tell us how to sign in \
             (HTTP {status}). Check the URL, or try again shortly.\n  detail: {body}",
            base = base_url,
        );
    }
    resp.json::<ConnectConfig>()
        .await
        .context("parsing the connect-config response")
}

/// What the machine calls itself, cleaned up for a human to read.
///
/// macOS hands back a `.local` mDNS suffix that means nothing to the person
/// reading "Claude Code on …", and a shell can hand back a blank line or the
/// placeholder every machine shares. None of those identify a machine, so each
/// one resolves to "unknown" rather than to a label that looks specific and
/// isn't.
fn machine_label(raw: Option<&str>) -> Option<String> {
    let name = raw?.trim().trim_end_matches('.');
    let name = name.strip_suffix(".local").unwrap_or(name);
    if name.is_empty() || name.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Some(name.to_string())
}

/// Ask the operating system for this machine's name.
///
/// The environment first because it is free and authoritative where it is set
/// (`HOSTNAME` on most Linux shells, `COMPUTERNAME` on Windows), then the
/// `hostname` command, which exists on all three platforms and is what macOS
/// leaves as the only answer.
fn machine_name() -> Option<String> {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(var)
            && let Some(label) = machine_label(Some(&value))
        {
            return Some(label);
        }
    }
    let out = std::process::Command::new("hostname").output().ok()?;
    if !out.status.success() {
        return None;
    }
    machine_label(Some(&String::from_utf8_lossy(out.stdout.as_slice())))
}

/// The name the minted token carries — what the app renders as the connection's
/// client and machine.
///
/// This is the object's identity in the webapp, so it is composed from the two
/// facts that actually distinguish one connection from another: which client is
/// wired up, and which machine it runs on. Either may be unknown, and an unknown
/// half is dropped rather than filled with a guess. Both unknown yields an empty
/// name, which the backend already renders as `Agent <id>` — an honest
/// "we don't know" beats a label that claims to know.
fn connection_name(client: Option<&str>, machine: Option<&str>) -> String {
    match (client, machine) {
        (Some(client), Some(machine)) => format!("{client} on {machine}"),
        (Some(client), None) => client.to_string(),
        (None, Some(machine)) => machine.to_string(),
        (None, None) => String::new(),
    }
}

/// Exchange a WorkOS access token for the long-lived agent CLOUD_TOKEN at
/// `POST {cloud_url}/v1/agent-token` (Bearer auth; the worker gate resolves the
/// tenant from the JWT).
///
/// `name` is what the workspace will call this connection forever after — the
/// registry stores it at mint time and the app reads it back — so it is passed
/// in from the caller's resolution rather than being a constant here.
async fn exchange_agent_token(
    http: &reqwest::Client,
    cloud_url: &str,
    workos_token: &str,
    name: &str,
) -> Result<AgentTokenResponse> {
    let resp = http
        .post(format!(
            "{}/v1/agent-token",
            cloud_url.trim_end_matches('/')
        ))
        .bearer_auth(workos_token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "could not reach the backend at {cloud_url} to finish signing in. \
                 Check your network connection and run `think-and-ship connect` again."
            )
        })?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "the backend refused to issue a workspace token (HTTP {status}). \
             Your sign-in may have expired — run connect again.\n  detail: {body}"
        );
    }
    resp.json::<AgentTokenResponse>()
        .await
        .context("parsing the agent-token response")
}

/// Why the authenticated smoke against the backend did not come back 2xx.
///
/// Three shapes, because they need three different corrective actions — and
/// none of them may name a transport. The person at the terminal can act on
/// "check your network connection"; they cannot act on a hyper error string.
#[derive(Debug, PartialEq, Eq)]
enum SmokeFailure {
    /// The backend answered and refused the token. The credential is bad.
    Rejected { status: u16 },
    /// The backend could not be reached at all.
    Unreachable,
    /// The backend was reached and errored. Not the credential's fault, and
    /// nothing on this machine can fix it.
    Backend { status: u16 },
}

impl SmokeFailure {
    /// What happened and the ONE thing to do about it.
    fn advice(&self, cloud_url: &str) -> String {
        match self {
            Self::Rejected { status } => format!(
                "the backend at {cloud_url} rejected the token that was just stored \
                 (HTTP {status}), so this connection does not work and nothing was \
                 configured. Run `think-and-ship connect` again to sign in fresh."
            ),
            Self::Unreachable => format!(
                "could not reach the backend at {cloud_url} to verify the connection. \
                 Check your network connection and run `think-and-ship connect` again."
            ),
            Self::Backend { status } => format!(
                "the backend at {cloud_url} answered HTTP {status} while verifying the \
                 connection — a problem on the service side, not with your setup. \
                 Try `think-and-ship connect` again shortly."
            ),
        }
    }
}

/// The authenticated smoke: one `GET /v1/records` with the Bearer token, so
/// "Connected" is a verified fact rather than a hope. `since` is set to `now`
/// so the tenant's history is not shipped back just to prove a credential.
///
/// Any 2xx proves the token authenticates; 401/403 means the credential itself
/// was refused; anything else is the backend's problem. A transport error is
/// deliberately collapsed to [`SmokeFailure::Unreachable`] — the reqwest chain
/// names DNS/TLS/socket internals nobody at the terminal can act on.
async fn smoke_check(
    http: &reqwest::Client,
    cloud_url: &str,
    token: &str,
) -> Result<(), SmokeFailure> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let resp = http
        .get(format!("{}/v1/records", cloud_url.trim_end_matches('/')))
        .query(&[("since", now.as_str())])
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| SmokeFailure::Unreachable)?;
    let status = resp.status().as_u16();
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(SmokeFailure::Rejected { status }),
        _ => Err(SmokeFailure::Backend { status }),
    }
}

/// How this particular client picks up a changed MCP config. Each one's own,
/// verified against that host's documentation — never a shared "restart it".
fn reload_step(host: &str) -> &'static str {
    match host {
        "Claude Code" => "run /mcp and reconnect think-and-ship, or start a new session",
        "Cursor" => "toggle think-and-ship in Settings → Tools & MCP, or restart Cursor",
        "Windsurf" => "refresh the plugins in the Cascade panel, or restart Windsurf",
        "VS Code" => {
            "run \"MCP: List Servers\" from the Command Palette and restart think-and-ship"
        }
        _ => "restart it",
    }
}

/// The closing message: EVERY client whose config now carries the entry, each
/// with its own reload step, plus anything a human still has to do.
///
/// It used to name one client, because `connect` configured one client. Naming
/// one was honest then and would be a lie now — a user with Claude Code and
/// Cursor open on the same repository is owed both lines, and the client that
/// silently went unmentioned is exactly the one that would sit there local while
/// the terminal said "Connected".
///
/// Takes the write OUTCOMES, not just the hosts. This function used to be
/// reachable from a write that had declined to happen, so it announced a reload
/// into a config that still held a local-only entry.
/// [`setup::WriteOutcome::Declined`] never arrives here (see
/// [`finish_connect_in`]), and the three that do all imply the entry on disk
/// carries the cloud wiring: `Created` and `Updated` because they just wrote it,
/// `AlreadyCurrent` because it means the entry EQUALS the one we would have
/// written.
fn ready_message(writes: &setup::ConnectWrites, cloud_url: &str) -> String {
    let mut out = format!("Connected — verified with an authenticated request to {cloud_url}.");
    for client in &writes.configured {
        let host = client.host;
        match client.outcome {
            // Nothing changed on disk, so there is nothing for this client to
            // pick up. Naming a reload here would send someone to restart an
            // editor for no reason.
            setup::WriteOutcome::AlreadyCurrent => out.push_str(&format!(
                "\n{host} was already configured for this workspace — nothing changed, \
                 and there is nothing to reload."
            )),
            _ => out.push_str(&format!(
                "\n{host} picks this up next: {}. Your traces then sync automatically.",
                reload_step(host)
            )),
        }
    }
    for step in &writes.manual {
        match step {
            setup::ManualStep::HostCommand { host, at, command } => out.push_str(&format!(
                "\n{host} also holds an entry in {at}, which only its own CLI may edit. \
                 Run this to bring it up to date:\n\n  {command}\n"
            )),
            setup::ManualStep::Unauthorable { host, config_file } => out.push_str(&format!(
                "\n{host} is here too but is never configured automatically — add the \
                 think-and-ship entry to {config_file} yourself if you use it."
            )),
        }
    }
    out
}

/// How the pre-proof chain (adopt → resolve back → smoke) failed, split by
/// what the failure means for the registration the mint just created.
///
/// The distinction decides the compensation. `Unusable` means this machine
/// holds no working credential for that registration — the store refused the
/// write, lost it, or kept something the backend rejects — so leaving it
/// registered is exactly the dogfood orphan: an agent the app lists forever
/// that can never write. `Unproven` means the credential is stored intact and
/// only the verification was interrupted (no network, backend 5xx); the
/// connection may well work the moment the blip passes, and revoking it would
/// destroy a healthy credential over a hiccup.
enum ProofFailure {
    Unusable(String),
    Unproven(String),
}

/// The pre-proof half of the connect tail: adopt into the store → resolve BACK
/// out of the store → authenticated smoke. An `Err` carries the user-facing
/// why, classified by [`ProofFailure`].
async fn adopt_and_prove(
    http: &reqwest::Client,
    store: &dyn crate::tracker::credential::CredentialStore,
    resolver: &crate::tracker::credential::Resolver,
    profile: &str,
    cloud_url: &str,
    minted: &str,
) -> Result<(), ProofFailure> {
    use crate::cloud::credential::{adopt, forget, resolve, staging_profile};

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let staging = staging_profile(profile);
    // Captured BEFORE anything is written, so a promotion that goes wrong has
    // something to put back. `None` here is the first-connect case and it
    // changes what an unproven token is worth — see the smoke arm below.
    let previous = resolve(store, profile);
    // The staged entry is deleted on every path out of this function. A
    // half-finished connect that left one behind would be a second credential
    // for `disconnect` to miss.
    let discard = || {
        let _ = forget(store, &staging);
    };

    if let Err(e) = adopt(resolver, &staging, minted, &now) {
        discard();
        return Err(ProofFailure::Unusable(format!(
            "storing the agent token in the credential store: {e}"
        )));
    }
    let Some(staged) = resolve(store, &staging) else {
        discard();
        return Err(ProofFailure::Unusable(format!(
            "the credential store did not return the token it just stored under \
             profile '{staging}'. Run `think-and-ship connect` again; if this repeats, \
             run `think-and-ship disconnect` first to clear the profile."
        )));
    };
    // The round-trip proof, now local and exact rather than inferred from a 401
    // three lines later. This is the check the macOS 128-byte truncation walked
    // through: the store kept SOMETHING, so the old non-empty test passed, and
    // only the backend could tell that what it kept was not the token.
    if staged != minted {
        discard();
        return Err(ProofFailure::Unusable(format!(
            "the credential store changed the agent token while saving it under \
             profile '{staging}' ({} bytes in, {} bytes back). The stored value cannot \
             authenticate. Your previous connection has not been touched.",
            minted.len(),
            staged.len(),
        )));
    }

    let outcome = smoke_check(http, cloud_url, &staged).await;
    match outcome {
        Ok(()) => {
            let promoted = promote(resolver, store, profile, &staged, &now);
            discard();
            promoted.map_err(ProofFailure::Unusable)
        }
        // The backend answered and REFUSED the token. Whatever the store kept is
        // not a working credential for this registration, so it never reaches
        // the real profile — and the credential that was already there is still
        // exactly what it was.
        Err(f @ SmokeFailure::Rejected { .. }) => {
            discard();
            Err(ProofFailure::Unusable(format!(
                "{}{}",
                f.advice(cloud_url),
                kept_note(previous.is_some()),
            )))
        }
        // The backend could not be reached, so nothing was proven either way.
        //
        // An unproven credential is worth more than NO credential and less than
        // a working one, and that is the whole rule: promote it when there is
        // nothing to protect, refuse to overwrite when there is. Before this,
        // a network blip during connect destroyed a working credential — the
        // reported failure mode, reachable without the server doing anything
        // wrong at all.
        Err(f) => {
            let advice = f.advice(cloud_url);
            if previous.is_none() {
                let promoted = promote(resolver, store, profile, &staged, &now);
                discard();
                if let Err(why) = promoted {
                    return Err(ProofFailure::Unusable(why));
                }
                Err(ProofFailure::Unproven(format!(
                    "{advice}\nThe token was stored anyway — this machine held no \
                     other credential, so an unverified one is better than none. \
                     Run `think-and-ship status` once the backend is reachable."
                )))
            } else {
                discard();
                Err(ProofFailure::Unproven(format!(
                    "{advice}\nYour existing connection has NOT been changed and still \
                     works; the token this run minted was discarded unproven. Run \
                     `think-and-ship connect` again when the backend is reachable."
                )))
            }
        }
    }
}

/// The reassurance a failure message owes a user who had a working connection.
///
/// Only said when it is true. Telling someone their previous credential is
/// intact when they never had one is noise, and worse, it is the kind of noise
/// that makes the true version stop being read.
fn kept_note(had_previous: bool) -> &'static str {
    if had_previous {
        "\nYour existing connection has NOT been changed and still works."
    } else {
        ""
    }
}

/// Move a proven token from the staging profile onto the real one.
///
/// Reads it back and compares, because promotion is a SECOND write to a
/// different key and a store that can mangle one can mangle the other. If that
/// comparison fails the previous credential — captured before any write — goes
/// back, so the worst case is the machine ending exactly where it started
/// rather than holding a token that authenticates as nobody.
fn promote(
    resolver: &crate::tracker::credential::Resolver,
    store: &dyn crate::tracker::credential::CredentialStore,
    profile: &str,
    proven: &str,
    now: &str,
) -> Result<(), String> {
    use crate::cloud::credential::{adopt, resolve};

    let previous = resolve(store, profile);
    let restore = |why: String| -> String {
        match &previous {
            Some(old) if adopt(resolver, profile, old, now).is_ok() => format!(
                "{why}\nThe credential that was already here has been put back, so this \
                 machine is exactly as connected as it was before this run."
            ),
            Some(_) => format!(
                "{why}\nThe credential that was already here could NOT be put back either. \
                 Run `think-and-ship connect` again to restore this machine."
            ),
            None => why,
        }
    };

    if let Err(e) = adopt(resolver, profile, proven, now) {
        return Err(restore(format!(
            "the agent token was verified but could not be saved under profile \
             '{profile}': {e}"
        )));
    }
    if resolve(store, profile).as_deref() != Some(proven) {
        return Err(restore(format!(
            "the credential store did not keep the verified agent token under profile \
             '{profile}'."
        )));
    }
    Ok(())
}

/// Best-effort compensating revoke of a registration this run minted and then
/// proved it cannot use: `POST /v1/agent-tokens/{jti}/revoke`, authenticated
/// with the WorkOS bearer that authorized the mint (independent of the token
/// being destroyed, so it survives a store that mangled the minted string).
///
/// Returns whether the backend confirmed with a 2xx. No retry and no error
/// detail — the caller is already failing for the real reason, and the only
/// question is which honest sentence it may append about the workspace.
async fn revoke_minted(
    http: &reqwest::Client,
    cloud_url: &str,
    workos_token: &str,
    jti: &str,
) -> bool {
    let url = format!(
        "{}/v1/agent-tokens/{jti}/revoke",
        cloud_url.trim_end_matches('/')
    );
    match http.post(url).bearer_auth(workos_token).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Everything `connect` does after the token is minted, in production order:
/// adopt into the store → resolve BACK out of the store → authenticated smoke →
/// config write → the ready message.
///
/// Two deliberate choices live here. The smoke authenticates with the token
/// resolved back from the store, not the minted string still in hand — that
/// proves the exact chain the spawned server runs at startup, so a store that
/// silently mangles what it saves (the macOS 128-byte truncation was found
/// exactly this way) fails the connect instead of the next session. And the
/// smoke runs BEFORE the config write, so a rejected credential leaves the MCP
/// config untouched rather than armed with sync wiring that cannot work.
///
/// Every early exit leaves only the adopted credential behind, and adopting
/// REPLACES, so a retry re-runs the same sequence with nothing to clean up.
/// The mint is the one effect a retry does NOT replace — it registered this
/// connection in the workspace before anything local could fail — so an exit
/// that proves the registration unusable revokes it on the way out, and says
/// exactly what it left when even that fails.
async fn finish_connect_in(tail: ConnectTail<'_>) -> Result<String> {
    let ConnectTail {
        http,
        cwd,
        home_config,
        store,
        resolver,
        profile,
        cloud_url,
        minted,
        minted_jti,
        name,
        record_dir,
        project_id,
        caller,
        workos_token,
        force,
    } = tail;
    match adopt_and_prove(http, store, resolver, profile, cloud_url, minted).await {
        Ok(()) => {}
        Err(ProofFailure::Unproven(why)) => bail!("{why}"),
        Err(ProofFailure::Unusable(why)) => {
            let label = if name.is_empty() {
                format!("token id {minted_jti}")
            } else {
                format!("'{name}' (token id {minted_jti})")
            };
            if revoke_minted(http, cloud_url, workos_token, minted_jti).await {
                bail!(
                    "{why}\nThe registration this run created in the workspace ({label}) \
                     was revoked — nothing unusable is left behind."
                );
            }
            bail!(
                "{why}\nThis run also registered {label} in the workspace and could not \
                 revoke it, so the app will list a connection that never writes. \
                 A fresh connect does not replace it — it needs to be revoked."
            );
        }
    }

    // The connection is a real thing the moment the backend accepts the stored
    // token, so it is recorded HERE — before the MCP config write, and
    // independently of whether that write finds the right client.
    //
    // That order is the fix, not an accident of sequencing. The MCP entry used
    // to be the only place a connection was recorded, which made it the
    // connection database and made writing it to the wrong client destroy the
    // connection outright. Now the entry is routing for one client, and the
    // record below is what `sync push`, `status` and every other CLI verb read —
    // none of which the MCP host ever spawns.
    let connection = crate::cloud::connection::Connection {
        cloud_url: cloud_url.to_string(),
        profile: profile.to_string(),
        connected_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    crate::cloud::connection::save_in(record_dir, project_id, &connection).with_context(|| {
        format!(
            "recording the connection in {}",
            crate::cloud::connection::path_in(record_dir).display()
        )
    })?;

    let writes = setup::write_cloud_mcp_config_in(
        cwd,
        home_config,
        caller,
        cloud_url,
        profile,
        false,
        force,
    )?;
    // The cloud lane never passes OnExisting::Keep, so this is unreachable — and
    // it is spelled out rather than assumed, because the whole defect was a
    // decline that reached the success message. If a future edit reintroduces a
    // declining policy here, this fails the command instead of lying about it.
    if let Some(declined) = writes
        .configured
        .iter()
        .find(|c| c.outcome == setup::WriteOutcome::Declined)
    {
        bail!(
            "the think-and-ship entry in {}'s config was left unchanged, so this machine \
             is authenticated but not wired up. Run `think-and-ship connect --force` to \
             replace that entry.",
            declined.host,
        );
    }
    Ok(ready_message(&writes, cloud_url))
}

/// The named inputs to [`finish_connect_in`] — nine positional arguments is how
/// a cwd ends up handed over as a home config.
struct ConnectTail<'a> {
    http: &'a reqwest::Client,
    cwd: &'a std::path::Path,
    home_config: Option<std::path::PathBuf>,
    store: &'a dyn crate::tracker::credential::CredentialStore,
    resolver: &'a crate::tracker::credential::Resolver,
    profile: &'a str,
    cloud_url: &'a str,
    /// The token the exchange just returned. Only ever spent by adopting it —
    /// the smoke uses what the STORE gives back.
    minted: &'a str,
    /// The mint's registry handle. Spent only by the compensating revoke when
    /// an early exit proves this machine cannot use the registration.
    minted_jti: &'a str,
    /// The connection name the mint registered — what the residue message
    /// shows a human when the revoke itself fails.
    name: &'a str,
    /// Where the connection record is written, and the key it is filed under.
    /// Threaded rather than resolved inside, so a test writes into its own temp
    /// dir instead of the developer's real one.
    record_dir: &'a std::path::Path,
    project_id: &'a str,
    /// Which clients the caller's environment names. Threaded for the same
    /// reason as `record_dir`: read inside, it would be the ONE input a test
    /// cannot supply, and every test would silently inherit whichever client
    /// happened to be running the suite. That is not hypothetical — the first
    /// version of this change read it inside, and the whole ready-state suite
    /// started behaving differently under a Claude Code session than it would
    /// on a build machine.
    caller: &'a setup::CallerEnv,
    /// The WorkOS bearer that authorized the mint; authenticates the revoke.
    workos_token: &'a str,
    force: bool,
}

/// Print the user-facing authorization prompt (the code + where to enter it).
fn print_user_prompt(auth: &DeviceAuthResponse) {
    eprintln!("\nTo authorize this machine:");
    if let Some(complete) = &auth.verification_uri_complete {
        eprintln!("  open: {complete}");
        eprintln!(
            "  (or visit {} and enter code {})",
            auth.verification_uri, auth.user_code
        );
    } else {
        eprintln!("  visit: {}", auth.verification_uri);
        eprintln!("  enter code: {}", auth.user_code);
    }
    eprintln!("\nWaiting for approval…");
}

/// The credential store this machine uses for the cloud token, plus the resolver
/// that owns persistence to it.
fn cloud_resolver() -> (
    std::sync::Arc<dyn crate::tracker::credential::CredentialStore>,
    crate::tracker::credential::Resolver,
) {
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let store = crate::cloud::credential::store_for(&data_dir);
    let resolver = crate::tracker::credential::Resolver::new(store.clone());
    (store, resolver)
}

/// Move a pre-store plaintext token out of the config it is sitting in.
///
/// Runs on EVERY connect, before the device flow, and deliberately does not
/// depend on `--force`. Two reasons. A plaintext bearer token in a file that gets
/// committed, home-synced and pasted into support threads is a live leak, and
/// declining to clean it up because the user did not pass a flag would be
/// choosing the leak. And doing it BEFORE the interactive round-trip means a
/// failed or abandoned sign-in still leaves the secret safe in the store rather
/// than back where it started.
///
/// A config this tool will not rewrite (`~/.claude.json`) cannot be cleaned for
/// the user, so the token is adopted anyway — it is the live credential — and the
/// human is told exactly which file still holds a copy.
/// [`migrate_plaintext_token_in`] against the real user-level Claude config.
fn migrate_plaintext_token(
    cwd: &std::path::Path,
    profile: &str,
    resolver: &crate::tracker::credential::Resolver,
) -> Result<()> {
    migrate_plaintext_token_in(cwd, setup::claude_home_config(), profile, resolver)
}

/// [`migrate_plaintext_token`] with the user-level config supplied rather than
/// read from `HOME`, so a test can drive the migration against a candidate set
/// the process environment has never held. Mutating `HOME` mid-suite is
/// process-global and races every other test.
pub(crate) fn migrate_plaintext_token_in(
    cwd: &std::path::Path,
    home_config: Option<std::path::PathBuf>,
    profile: &str,
    resolver: &crate::tracker::credential::Resolver,
) -> Result<()> {
    let Some(found) = setup::find_plaintext_token_in(cwd, home_config.clone()) else {
        return Ok(());
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    crate::cloud::credential::adopt(resolver, profile, &found.token, &now)
        .context("moving the existing token into the credential store")?;

    if !found.ours_to_write {
        eprintln!(
            "Your existing cloud token is now in the credential store under profile \
             '{profile}'.\n  A plaintext copy is still in {} — remove the \
             {} line from that entry by hand.",
            found.at,
            crate::cloud::credential::TOKEN_ENV,
        );
        return Ok(());
    }

    // Detection just read the key out of that file, so anything other than
    // Removed means a concurrent edit. Nothing to say, and nothing lost.
    if let setup::EnvRemoval::Removed(_) = setup::remove_server_env_in(
        cwd,
        home_config,
        &[crate::cloud::credential::TOKEN_ENV],
        false,
    )? {
        eprintln!(
            "Moved your existing cloud token out of {} and into the credential store \
             under profile '{profile}'.",
            found.at,
        );
    }
    Ok(())
}

/// `think-and-ship connect` — run the device flow, store the token, and write the
/// cloud MCP config.
///
/// `url` defaults to `DEFAULT_CLOUD_URL`. `--dry-run` resolves the connect target
/// and previews the config it would write without the interactive WorkOS
/// round-trip. Builds a current-thread tokio runtime like `sync_push`.
///
/// `--force` is NOT needed to update an existing entry — connect always brings
/// the entry it finds up to date, because the token it just minted belongs to
/// this `cloud_url` and profile and nothing else. The flag only escalates to
/// rewriting an entry that already matches byte for byte.
///
/// The minted token goes to the credential store under a named profile; the
/// config gets the profile's NAME. Nothing written here is a secret. "Connected"
/// is only printed after the stored token has authenticated one real request
/// against the backend — see `finish_connect_in` — and the closing message
/// names the one client that has to reload, not every client we know about.
pub fn connect(url: Option<&str>, dry_run: bool, force: bool, clients: &[String]) -> Result<()> {
    // Resolved BEFORE the browser flow, so an unknown --client name fails in
    // the first second rather than after a human has finished signing in.
    let caller = setup::CallerEnv::from_env().naming(clients)?;
    let base_url = resolve_cloud_url(url, std::env::var(CLOUD_URL_ENV).ok().as_deref())?;
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let profile = crate::cloud::credential::default_profile();
    let http = reqwest::Client::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // 1. Resolve the connect target (public, no auth).
    let config = rt.block_on(fetch_connect_config(&http, &base_url))?;
    let Some(client_id) = config.workos_client_id.clone() else {
        bail!(
            "sign-in isn't enabled on the backend at {base_url} yet. \
             If this is your own deployment, configure its identity provider; \
             otherwise use the hosted backend (omit --url)."
        );
    };
    eprintln!(
        "Connecting to {} (authorizing via {})",
        config.cloud_url, config.workos_authorize_base
    );

    if dry_run {
        eprintln!(
            "\n--dry-run: would run the WorkOS device authorization, store the token in \
             the credential store under profile '{profile}', then write this MCP \
             config:\n"
        );
        setup::write_cloud_mcp_config_in(
            &cwd,
            setup::claude_home_config(),
            &caller,
            &config.cloud_url,
            &profile,
            true,
            force,
        )?;
        return Ok(());
    }

    let (store, resolver) = cloud_resolver();

    // 2. Any pre-store plaintext token leaves the config first, whatever happens
    //    next.
    migrate_plaintext_token(&cwd, &profile, &resolver)?;

    // 3. Device authorization → poll → WorkOS access token.
    let transport = ReqwestDeviceTransport {
        http: http.clone(),
        authorize_base: config.workos_authorize_base.clone(),
        client_id,
    };
    let workos_token = rt
        .block_on(async {
            let auth = transport.device_authorize().await?;
            print_user_prompt(&auth);
            run_poll_loop(&transport, &TokioSleeper, &auth).await
        })
        .context("the WorkOS device authorization failed")?;

    // 4. Exchange for the long-lived agent token, named for the client this
    //    machine is connecting. The name is resolved here, before the mint, so
    //    the registry entry the workspace keeps is the one the app can show as a
    //    connection rather than an anonymous credential.
    let name = connection_name(
        setup::client_label_in(&cwd, setup::claude_home_config(), &caller),
        machine_name().as_deref(),
    );
    let minted = rt.block_on(exchange_agent_token(
        &http,
        &config.cloud_url,
        &workos_token,
        &name,
    ))?;

    // 5. Adopt → resolve back → authenticated smoke → config write. The command
    //    only says "Connected" once the backend has accepted the stored token.
    let message = rt.block_on(finish_connect_in(ConnectTail {
        http: &http,
        cwd: &cwd,
        home_config: setup::claude_home_config(),
        store: store.as_ref(),
        resolver: &resolver,
        profile: &profile,
        cloud_url: &config.cloud_url,
        minted: &minted.token,
        minted_jti: &minted.jti,
        name: &name,
        record_dir: &crate::cloud::connection::data_dir(),
        project_id: &crate::cloud::connection::project_id(),
        caller: &caller,
        workos_token: &workos_token,
        force,
    }))?;
    eprintln!("\n{message}");
    Ok(())
}

/// `think-and-ship disconnect` — forget the stored token and strip the cloud
/// wiring from the MCP config.
///
/// Both halves, always, and in that order. Forgetting the credential while
/// leaving `SYNC_TARGET=cloud` in place would produce a server that tries to
/// sync on every write and fails; stripping the config while leaving the token in
/// the keychain would leave a live long-lived credential on a machine the user
/// believes they disconnected. The store deletion is idempotent, so running this
/// twice is not an error.
/// `think-and-ship token` — print this project's cloud agent token and nothing
/// else, the way `gh auth token` does.
///
/// This exists because a documentation fix could not work. The credential store
/// keeps a `StoredCredential` envelope, and that envelope has three
/// dot-separated segments — so decoding it AS a JWT succeeds and returns the
/// genuine claims of the token embedded inside it. A wrong read produces
/// CONFIRMING evidence for the wrong conclusion: a parse error wearing a
/// revocation's clothes. It cost an hour and a chunk filed on a false premise
/// (see the correction in connect-failure-clobbers-a-working-credential).
///
/// It is its own verb rather than a line in `status` or `doctor` on purpose:
/// those get pasted into support threads, and a secret must only ever leave the
/// machine when someone asked for exactly that. Resolution goes through the same
/// env-first/record-second path as everything else, so what it prints is what
/// the server would actually send.
pub fn print_token() -> Result<()> {
    let stored = crate::cloud::connection::load();
    let env = crate::cloud::config::EnvOverrides::from_env();
    let store = crate::cloud::credential::store_for(&crate::cloud::connection::data_dir());
    let Some((token, _)) =
        crate::cloud::config::resolve_token(store.as_ref(), &env, stored.as_ref())
    else {
        bail!(
            "no cloud token for this project. Run `think-and-ship status` to see whether \
             this machine is connected."
        );
    };
    // stdout, bare, no trailing commentary — this is meant to be piped.
    println!("{token}");
    Ok(())
}

pub fn disconnect(dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let profile = crate::cloud::credential::default_profile();
    let (store, _resolver) = cloud_resolver();
    disconnect_in(
        &cwd,
        setup::claude_home_config(),
        store.as_ref(),
        &profile,
        &crate::cloud::connection::data_dir(),
        &crate::cloud::connection::project_id(),
        dry_run,
    )
}

/// [`disconnect`] with the config location, store and profile supplied, so the
/// transition is provable without a real keychain and without reading `HOME`.
pub(crate) fn disconnect_in(
    cwd: &std::path::Path,
    home_config: Option<std::path::PathBuf>,
    store: &dyn crate::tracker::credential::CredentialStore,
    profile: &str,
    record_dir: &std::path::Path,
    project_id: &str,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        eprintln!(
            "--dry-run: would forget profile '{profile}' from this machine's credential \
             store, drop the connection record in {}, and remove {} from this project's \
             MCP entry.",
            crate::cloud::connection::path_in(record_dir).display(),
            setup::CLOUD_ENV_KEYS.join(", "),
        );
        return Ok(());
    }

    crate::cloud::credential::forget(store, profile)
        .context("forgetting the stored agent token")?;
    eprintln!("Forgot profile '{profile}' from this machine's credential store.");

    // Defence in depth. `adopt_and_prove` deletes the staging profile on every
    // path it can take, but a process killed mid-connect takes no path at all,
    // and a live token that `disconnect` did not clear is exactly the state this
    // command exists to make impossible.
    let staging = crate::cloud::credential::staging_profile(profile);
    if crate::cloud::credential::resolve(store, &staging).is_some() {
        crate::cloud::credential::forget(store, &staging)
            .context("forgetting a half-finished connect's staged token")?;
        eprintln!("Also cleared a staged token left by an interrupted connect.");
    }

    // The THIRD half, and it is not optional: leaving the record behind would
    // leave every CLI verb still believing this project is connected, reporting
    // a workspace it can no longer reach. A disconnected machine has to read as
    // disconnected from every surface, not just from the two that existed first.
    if crate::cloud::connection::forget_in(record_dir, project_id)
        .context("forgetting the recorded connection")?
    {
        eprintln!("Forgot the recorded connection for this project.");
    }

    match setup::remove_server_env_in(cwd, home_config, setup::CLOUD_ENV_KEYS, false)? {
        setup::EnvRemoval::Removed(keys) => {
            eprintln!("Removed {} from this project's MCP entry.", keys.join(", "));
            eprintln!("Restart your MCP client to stop syncing.");
        }
        setup::EnvRemoval::NothingToRemove => {
            eprintln!("This project's MCP entry held no cloud settings — nothing to remove.");
        }
        setup::EnvRemoval::NoServerEntry => {
            eprintln!("No think-and-ship MCP entry for this project — nothing to remove.");
        }
        setup::EnvRemoval::ExternalConfig { at } => {
            eprintln!(
                "This project's entry lives in {at}, which this tool will not rewrite.\n  \
                 Remove {} from it by hand.",
                setup::CLOUD_ENV_KEYS.join(", "),
            );
        }
    }
    Ok(())
}

/// The four credential transitions, proven end to end over the real config
/// writer and the real store seam.
///
/// These are the tests the chunk turns on, so it is worth being explicit about
/// what they do and do not cover. They drive exactly the calls `connect` makes
/// after the device flow — `migrate_plaintext_token_in`, `credential::adopt`,
/// `write_cloud_mcp_config_in`, `disconnect_in` — in the same order, against a
/// temp directory and a temp store. What they skip is the WorkOS round-trip,
/// which needs real credentials and is covered by the wiremock tests below.
///
/// Every absence assertion is paired with a presence assertion, deliberately. A
/// test that only checks "the secret is not in the file" passes against a config
/// that was never written, an empty string, and a typo'd path — which is exactly
/// how a leak survives a green suite.
#[cfg(test)]
mod credential_transitions {
    use super::*;
    use crate::cloud::credential::{PROFILE_ENV, TOKEN_ENV, adopt, provider_key, resolve};
    use crate::tracker::credential::{CredentialStore, FileCredentialStore, Resolver};
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    const PROFILE: &str = "acme-workspace";
    /// The project key the connection record is filed under in these tests.
    const TEST_PROJECT: &str = "acme-project";

    /// Every config file that could hold a secret for this project, read raw.
    ///
    /// Raw TEXT, not parsed JSON: the question is whether the token appears
    /// anywhere in the bytes, under any key, at any nesting depth. Walking a
    /// parsed document key by key would only ever find the keys the test author
    /// thought of.
    fn all_config_text(cwd: &Path) -> String {
        let mut out = String::new();
        for name in [
            ".mcp.json",
            ".cursor/mcp.json",
            ".vscode/mcp.json",
            ".windsurf/mcp.json",
        ] {
            if let Ok(body) = std::fs::read_to_string(cwd.join(name)) {
                out.push_str(&body);
            }
        }
        out
    }

    /// The project entry's env block, so presence can be asserted alongside
    /// absence.
    fn env_of(cwd: &Path) -> Value {
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(cwd.join(".mcp.json")).unwrap()).unwrap();
        doc["mcpServers"]["think-and-ship"]["env"].clone()
    }

    /// Assert the config is properly connected AND holds no secret. Both halves,
    /// every time, so neither can drift out of the suite.
    fn assert_connected_without_a_secret(cwd: &Path, secret: &str, label: &str) {
        let env = env_of(cwd);
        assert_eq!(
            env["THINK_AND_SHIP_SYNC_TARGET"], "cloud",
            "{label}: write-through sync must still be armed — a config with no \
             SYNC_TARGET would pass every absence check below while syncing nothing",
        );
        assert_eq!(
            env[PROFILE_ENV], PROFILE,
            "{label}: the entry must name the profile that resolves the token",
        );
        let text = all_config_text(cwd);
        assert!(
            !text.contains(secret),
            "{label}: the token appears in a config file:\n{text}",
        );
        assert!(
            !text.contains(TOKEN_ENV),
            "{label}: the plaintext token key appears in a config file:\n{text}",
        );
    }

    /// A project with a think-and-ship entry already in place, which is what
    /// `connect` updates.
    fn seeded_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "think-and-ship": { "command": "think-and-ship", "args": ["serve"] },
                    "someone-elses-server": { "command": "other" }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        tmp
    }

    /// ACCEPTANCE 1 + 3: connect, reconnect and regenerate each leave the token
    /// in the store and no plaintext anywhere; disconnect leaves neither.
    #[test]
    fn every_transition_leaves_the_secret_in_the_store_and_never_in_a_config() {
        let project = seeded_project();
        let cwd = project.path();
        let data = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(data.path()));
        let resolver = Resolver::new(store.clone());

        // ── connect ──────────────────────────────────────────────────────────
        let first = "cloud_tok_first_00000000";
        adopt(&resolver, PROFILE, first, "2026-07-30T00:00:00Z").unwrap();
        setup::write_cloud_mcp_config_in(
            cwd,
            None,
            &setup::CallerEnv::unknown(),
            "https://api.example",
            PROFILE,
            false,
            true,
        )
        .unwrap();
        assert_connected_without_a_secret(cwd, first, "connect");
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(first),
            "connect: the server must be able to resolve the token it just stored",
        );

        // ── reconnect: same profile, a newly minted token ────────────────────
        let second = "cloud_tok_second_11111111";
        adopt(&resolver, PROFILE, second, "2026-07-30T01:00:00Z").unwrap();
        setup::write_cloud_mcp_config_in(
            cwd,
            None,
            &setup::CallerEnv::unknown(),
            "https://api.example",
            PROFILE,
            false,
            true,
        )
        .unwrap();
        assert_connected_without_a_secret(cwd, second, "reconnect");
        assert_connected_without_a_secret(cwd, first, "reconnect (previous token)");
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(second),
            "reconnect: the new token REPLACED the old one rather than adding a second",
        );

        // ── regenerate: a third token over the top of the second ─────────────
        let third = "cloud_tok_third_22222222";
        adopt(&resolver, PROFILE, third, "2026-07-30T02:00:00Z").unwrap();
        assert_connected_without_a_secret(cwd, third, "regenerate");
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(third),
            "regenerate: the store holds exactly the newest token",
        );
        // Only ONE entry exists for this profile — a regenerate that appended
        // would leave the old credential live on the machine.
        assert_eq!(
            store
                .providers()
                .iter()
                .filter(|p| p.starts_with("cloud-"))
                .count(),
            1,
            "regenerate must not leave a second stored credential behind",
        );

        // ── disconnect: nothing in the store, the config, or the record ──────
        //
        // The third artifact is the one this chunk added, and it is asserted
        // here rather than in its own test on purpose: a machine that reads as
        // disconnected from two surfaces and connected from the third is the
        // precise state that used to be reachable, so the three have to be
        // proven together or not at all.
        crate::cloud::connection::save_in(
            cwd,
            TEST_PROJECT,
            &crate::cloud::connection::Connection {
                cloud_url: "https://acme.example".to_string(),
                profile: PROFILE.to_string(),
                connected_at: "2026-07-31T23:00:00Z".to_string(),
            },
        )
        .unwrap();
        assert!(crate::cloud::connection::load_in(cwd, TEST_PROJECT).is_some());

        disconnect_in(cwd, None, store.as_ref(), PROFILE, cwd, TEST_PROJECT, false).unwrap();
        assert_eq!(
            resolve(store.as_ref(), PROFILE),
            None,
            "disconnect: the token is gone from the store",
        );
        assert_eq!(
            crate::cloud::connection::load_in(cwd, TEST_PROJECT),
            None,
            "disconnect: the recorded connection is gone, so no CLI verb still \
             believes this project is connected",
        );
        let after = env_of(cwd);
        for key in setup::CLOUD_ENV_KEYS {
            assert!(
                after.get(*key).is_none(),
                "disconnect: {key} survived in the config: {after}",
            );
        }
        assert_eq!(
            after["THINK_AND_SHIP_PERSIST"], "true",
            "disconnect removes the CLOUD wiring only — persistence is not a cloud setting",
        );
        let text = all_config_text(cwd);
        for secret in [first, second, third] {
            assert!(
                !text.contains(secret),
                "disconnect: a token is still in a config file:\n{text}",
            );
        }
        // Nothing unrelated was collateral.
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(cwd.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            doc["mcpServers"]["someone-elses-server"]["command"],
            "other"
        );

        // And again, because disconnecting twice must not be an error.
        disconnect_in(cwd, None, store.as_ref(), PROFILE, cwd, TEST_PROJECT, false)
            .expect("disconnect is idempotent");
    }

    /// ACCEPTANCE 4: a plaintext token already in the config is MOVED, not left.
    ///
    /// This is the upgrade path for every machine connected before the store
    /// existed, and it runs on connect without `--force` — because declining to
    /// clean up a live leak until the user passes a flag would be choosing the
    /// leak.
    #[test]
    fn an_existing_plaintext_token_is_moved_into_the_store_on_the_next_connect() {
        let project = TempDir::new().unwrap();
        let cwd = project.path();
        let legacy = "cloud_tok_legacy_plaintext";
        // The pre-store shape, exactly as the old cloud_server_config wrote it.
        std::fs::write(
            cwd.join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "think-and-ship": {
                        "command": "think-and-ship",
                        "args": ["serve"],
                        "env": {
                            "THINK_AND_SHIP_PERSIST": "true",
                            "THINK_AND_SHIP_SYNC_TARGET": "cloud",
                            "THINK_AND_SHIP_CLOUD_URL": "https://api.example",
                            TOKEN_ENV: legacy,
                            "SOMETHING_THE_USER_SET": "keep-me",
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // Precondition — otherwise this test could pass against a file that
        // never held the token in the first place.
        assert!(
            all_config_text(cwd).contains(legacy),
            "precondition: the plaintext token is in the config",
        );

        let data = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(data.path()));
        let resolver = Resolver::new(store.clone());
        assert_eq!(
            resolve(store.as_ref(), PROFILE),
            None,
            "precondition: the store is empty",
        );

        migrate_plaintext_token_in(cwd, None, PROFILE, &resolver).unwrap();

        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(legacy),
            "the existing token was MOVED into the store — not discarded, or the user \
             would be disconnected by an upgrade",
        );
        let text = all_config_text(cwd);
        assert!(
            !text.contains(legacy),
            "the plaintext copy must be gone from the config:\n{text}",
        );
        assert!(
            !text.contains(TOKEN_ENV),
            "the plaintext token key must be gone too:\n{text}",
        );
        let env = env_of(cwd);
        assert_eq!(
            env["THINK_AND_SHIP_SYNC_TARGET"], "cloud",
            "migration must not disarm the sync it found armed",
        );
        assert_eq!(
            env["SOMETHING_THE_USER_SET"], "keep-me",
            "migration touches the token key and nothing else",
        );

        // Idempotent: a second connect finds nothing to migrate and changes
        // nothing.
        let before = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();
        migrate_plaintext_token_in(cwd, None, PROFILE, &resolver).unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join(".mcp.json")).unwrap(),
            before,
            "a second connect must not rewrite an already-migrated config",
        );
    }

    /// A token in a config this tool will not rewrite (`~/.claude.json`) is still
    /// adopted, because it is the live credential — but the file is left alone and
    /// the human has to be told. Reporting success while silently leaving the
    /// secret would be the worst of the three options.
    #[test]
    fn a_token_in_an_unwritable_config_is_adopted_and_the_file_left_for_the_human() {
        let project = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let home_config = home.path().join(".claude.json");
        let legacy = "cloud_tok_in_claude_json";
        let key = project.path().canonicalize().unwrap();
        std::fs::write(
            &home_config,
            serde_json::to_string_pretty(&json!({
                "projects": {
                    key.to_string_lossy(): {
                        "mcpServers": {
                            "think-and-ship": {
                                "command": "think-and-ship",
                                "env": { TOKEN_ENV: legacy }
                            }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let found = setup::find_plaintext_token_in(project.path(), Some(home_config.clone()))
            .expect("the token in ~/.claude.json is found");
        assert_eq!(found.token, legacy);
        assert!(
            !found.ours_to_write,
            "~/.claude.json is not ours to rewrite — see ServerEntry::is_ours_to_write",
        );

        let data = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(data.path()));
        let resolver = Resolver::new(store.clone());
        migrate_plaintext_token_in(
            project.path(),
            Some(home_config.clone()),
            PROFILE,
            &resolver,
        )
        .unwrap();

        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(legacy),
            "the live credential is adopted even when its file cannot be cleaned",
        );
        assert!(
            std::fs::read_to_string(&home_config)
                .unwrap()
                .contains(legacy),
            "the file we refuse to rewrite must be left byte-for-byte alone",
        );
    }

    /// The store key the writer uses is the key the reader reads. If these drift,
    /// connect stores a token the server cannot find and the failure is silent.
    #[test]
    fn the_key_connect_writes_is_the_key_the_server_resolves() {
        let data = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(data.path()));
        let resolver = Resolver::new(store.clone());
        adopt(&resolver, PROFILE, "tok", "2026-07-30T00:00:00Z").unwrap();

        assert!(
            store.load(&provider_key(PROFILE)).unwrap().is_some(),
            "adopt must store under provider_key(profile)",
        );
        assert_eq!(store.providers(), vec![provider_key(PROFILE)]);
    }
}

/// The device-grant transport, mocked at the URLs WorkOS really serves.
///
/// Every `path(...)` below is a LITERAL, deliberately not `DEVICE_AUTHORIZE_PATH`
/// / `DEVICE_TOKEN_PATH`. Mounting the constants would make these tests follow
/// the code wherever it went, which is precisely how the shipped 404 survived:
/// the paths were guessed from RFC 8628's prose (which names none), production
/// answered `Cannot POST /user_management/device_authorization` on every single
/// connect, and the suite stayed green because it mocked whatever was asked for.
/// Spelling the real paths out is what makes a wrong URL fail here instead.
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn transport(authorize_base: &str) -> ReqwestDeviceTransport {
        ReqwestDeviceTransport {
            http: reqwest::Client::new(),
            authorize_base: authorize_base.trim_end_matches('/').to_string(),
            client_id: "client_123".into(),
        }
    }

    #[tokio::test]
    async fn device_authorize_posts_and_parses_the_rfc8628_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authorize/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc_1",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://auth.example/device",
                "expires_in": 1800,
                "interval": 5
            })))
            .mount(&server)
            .await;

        let auth = transport(&server.uri()).device_authorize().await.unwrap();
        assert_eq!(auth.user_code, "WDJB-MJHT");
        assert_eq!(auth.poll_interval(), Duration::from_secs(5));
    }

    #[tokio::test]
    async fn device_authorize_surfaces_a_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authorize/device"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"error":"invalid_client"}"#),
            )
            .mount(&server)
            .await;

        let err = transport(&server.uri())
            .device_authorize()
            .await
            .unwrap_err();
        assert!(matches!(err, DeviceFlowError::Unexpected(msg) if msg.contains("400")));
    }

    #[tokio::test]
    async fn poll_token_maps_2xx_to_granted_with_the_access_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authenticate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "wos_at", "token_type": "bearer"
            })))
            .mount(&server)
            .await;

        let got = transport(&server.uri()).poll_token("dc_1").await.unwrap();
        assert_eq!(got, TokenPoll::Granted("wos_at".into()));
    }

    #[tokio::test]
    async fn poll_token_maps_a_non_2xx_error_body_to_a_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authenticate"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({ "error": "authorization_pending" })),
            )
            .mount(&server)
            .await;

        let got = transport(&server.uri()).poll_token("dc_1").await.unwrap();
        assert_eq!(got, TokenPoll::Status(PollStatus::Pending));
    }

    /// The 400 body verbatim from `api.workos.com`, `error_description` and all.
    /// Every poll before the human clicks approve gets this, so reading it as a
    /// failure rather than as "keep waiting" would abort every connect.
    #[tokio::test]
    async fn the_real_pending_body_from_workos_is_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/authenticate"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "authorization_pending",
                "error_description": "The authorization request is still pending user approval."
            })))
            .mount(&server)
            .await;

        let got = transport(&server.uri()).poll_token("dc_1").await.unwrap();
        assert_eq!(got, TokenPoll::Status(PollStatus::Pending));
    }

    #[tokio::test]
    async fn fetch_connect_config_parses_the_public_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/connect-config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cloud_url": "https://api.example",
                "workos_client_id": "client_123",
                "workos_authorize_base": "https://auth.example/user_management"
            })))
            .mount(&server)
            .await;

        let cfg = fetch_connect_config(&reqwest::Client::new(), &server.uri())
            .await
            .unwrap();
        assert_eq!(cfg.workos_client_id.as_deref(), Some("client_123"));
        assert_eq!(cfg.cloud_url, "https://api.example");
        assert_eq!(
            cfg.workos_authorize_base,
            "https://auth.example/user_management"
        );
    }

    #[tokio::test]
    async fn exchange_agent_token_posts_bearer_and_returns_the_cloud_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/agent-token"))
            .and(header("authorization", "Bearer wos_at"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token": "cloud_tok", "expires_at": "2026-07-01T00:00:00Z", "jti": "j1"
            })))
            .mount(&server)
            .await;

        let minted = exchange_agent_token(
            &reqwest::Client::new(),
            &server.uri(),
            "wos_at",
            "Claude Code on studio",
        )
        .await
        .unwrap();
        assert_eq!(minted.token, "cloud_tok");
        // The jti comes back too: it is the only handle that can revoke the
        // registration this call just created, should nothing local work out.
        assert_eq!(minted.jti, "j1");
    }

    #[tokio::test]
    async fn exchange_agent_token_surfaces_a_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/agent-token"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthenticated"}"#),
            )
            .mount(&server)
            .await;

        let err = exchange_agent_token(&reqwest::Client::new(), &server.uri(), "bad", "Cursor")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("401"));
    }
}

/// The connection's identity: what the workspace will call this machine.
///
/// The registry keeps the mint-time name forever, and the app renders it as the
/// connection's client and machine — so a wrong or empty name here is a wrong
/// object there, and the wire body is asserted rather than assumed.
#[cfg(test)]
mod connection_identity {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_name_states_only_what_is_known() {
        assert_eq!(
            connection_name(Some("Claude Code"), Some("studio")),
            "Claude Code on studio"
        );
        // An unknown half is dropped, never guessed or padded.
        assert_eq!(connection_name(Some("Cursor"), None), "Cursor");
        assert_eq!(connection_name(None, Some("studio")), "studio");
        // Both unknown: the backend renders `Agent <id>` from the jti, which is
        // honest. Inventing a name here would outlive the run that invented it.
        assert_eq!(connection_name(None, None), "");
    }

    #[test]
    fn machine_labels_that_identify_nothing_are_refused() {
        // The macOS shape, newline and mDNS suffix included.
        assert_eq!(
            machine_label(Some("Studio-MBP.local\n")),
            Some("Studio-MBP".to_string())
        );
        assert_eq!(machine_label(Some(" studio ")), Some("studio".to_string()));
        // Shared by every machine, so it distinguishes none of them.
        assert_eq!(machine_label(Some("localhost")), None);
        assert_eq!(machine_label(Some("LOCALHOST\n")), None);
        assert_eq!(machine_label(Some("   ")), None);
        assert_eq!(machine_label(None), None);
    }

    #[tokio::test]
    async fn the_mint_request_carries_the_client_and_the_machine() {
        let server = MockServer::start().await;
        // The matcher IS the assertion: a mint that posts any other name — the
        // old fixed "think-and-ship connect" included — gets no match and the
        // call fails.
        Mock::given(method("POST"))
            .and(path("/v1/agent-token"))
            .and(body_partial_json(
                json!({ "name": "Claude Code on studio" }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "token": "cloud_tok", "jti": "j2" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let name = connection_name(Some("Claude Code"), Some("studio"));
        let minted = exchange_agent_token(&reqwest::Client::new(), &server.uri(), "wos_at", &name)
            .await
            .unwrap();
        assert_eq!(minted.token, "cloud_tok");
    }
}

/// The chunk's acceptance tests: connect ends in a FACT.
///
/// These drive [`finish_connect_in`] — the exact tail `connect` runs after the
/// mint — never a re-enactment of it, so "the smoke gates the config write" and
/// "no failure blocks a retry" are properties of the shipped sequence. Every
/// absence assertion is paired with a presence assertion, per the
/// [`credential_transitions`] discipline.
#[cfg(test)]
mod ready_state {
    use super::*;
    use crate::cloud::credential::resolve;
    use crate::tracker::credential::{FileCredentialStore, Resolver};
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PROFILE: &str = "acme-workspace";
    /// The project key the connection record is filed under in these tests.
    const TEST_PROJECT: &str = "acme-project";
    const MINTED: &str = "cloud_tok_minted_ready_state";
    const MINTED_JTI: &str = "jti_ready_state";
    const NAME: &str = "Claude Code on test-machine";
    const WORKOS: &str = "wos_at_ready_state";
    const HOSTS: [&str; 4] = ["Claude Code", "Cursor", "Windsurf", "VS Code"];

    /// A failure message the person at the terminal can act on names no
    /// transport machinery. The needles are lowercase fragments of what
    /// reqwest/hyper actually produce.
    fn assert_no_transport_internals(msg: &str) {
        let lower = msg.to_lowercase();
        for needle in [
            "reqwest",
            "hyper",
            "error sending request",
            "dns error",
            "tcp connect",
            "os error",
            "connection refused",
        ] {
            assert!(
                !lower.contains(needle),
                "transport internals leaked into a user-facing message: {msg}",
            );
        }
    }

    fn seeded_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "think-and-ship": { "command": "think-and-ship", "args": ["serve"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        tmp
    }

    fn store_and_resolver() -> (TempDir, Arc<FileCredentialStore>, Resolver) {
        let data = TempDir::new().unwrap();
        let store = Arc::new(FileCredentialStore::new(data.path()));
        let resolver = Resolver::new(store.clone());
        (data, store, resolver)
    }

    /// A backend whose `/v1/records` answers with `status`.
    async fn backend_answering(status: u16) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/records"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({ "records": [] })))
            .mount(&server)
            .await;
        server
    }

    /// A URL nothing listens on. NOT a dropped MockServer port — the OS hands
    /// a freed ephemeral port straight to the next `bind(0)`, so under a
    /// parallel test run another test's mock answered on it (observed as a 404
    /// where "unreachable" was expected). Port 1 is privileged; no test can
    /// bind it, so the connection is refused deterministically.
    fn unreachable_url() -> String {
        "http://127.0.0.1:1".to_string()
    }

    async fn run_finish(
        cwd: &Path,
        home_config: Option<std::path::PathBuf>,
        store: &FileCredentialStore,
        resolver: &Resolver,
        cloud_url: &str,
        minted: &str,
    ) -> Result<String> {
        finish_connect_in(ConnectTail {
            http: &reqwest::Client::new(),
            cwd,
            home_config,
            store,
            resolver,
            profile: PROFILE,
            cloud_url,
            minted,
            minted_jti: MINTED_JTI,
            name: NAME,
            // The record lands in the test's own temp cwd, not the developer's
            // real data dir — the whole point of threading these rather than
            // resolving them inside.
            record_dir: cwd,
            project_id: TEST_PROJECT,
            // An environment naming NOBODY, so these tests describe a project
            // rather than whichever client happens to be running the suite.
            caller: &setup::CallerEnv::unknown(),
            workos_token: WORKOS,
            // force:FALSE, deliberately. This helper used to pin it to true,
            // which is the one value that skips the already-configured gate —
            // so the whole ready-state suite drove the branch a real user never
            // takes, and the gate's defect was invisible to it. A helper that
            // pins an argument to the convenient value deletes a branch from
            // the suite.
            force: false,
        })
        .await
    }

    /// There is no built-in backend. The tool refuses to invent one, and says
    /// how to supply it — this host is written into every MCP config connect
    /// authors, so a silent default would put a name of ours in a file the
    /// user keeps.
    #[test]
    fn no_backend_configured_names_both_ways_to_supply_one() {
        let err = resolve_cloud_url(None, None).unwrap_err().to_string();
        assert!(err.contains("--url"), "must name the flag: {err}");
        assert!(err.contains(CLOUD_URL_ENV), "must name the variable: {err}");
    }

    /// The flag beats the environment, so one invocation can point somewhere
    /// else without unsetting a variable the shell keeps.
    #[test]
    fn the_flag_wins_over_the_environment() {
        let got =
            resolve_cloud_url(Some("https://flag.example"), Some("https://env.example")).unwrap();
        assert_eq!(got, "https://flag.example");
        let got = resolve_cloud_url(None, Some("https://env.example")).unwrap();
        assert_eq!(got, "https://env.example");
    }

    /// A trailing slash doubles into every path this is joined with, and the
    /// value arrives from a shell variable often enough to be worth absorbing
    /// rather than rejecting.
    #[test]
    fn a_trailing_slash_and_surrounding_space_are_absorbed() {
        assert_eq!(
            resolve_cloud_url(Some("  https://api.example.com/  "), None).unwrap(),
            "https://api.example.com",
        );
    }

    /// Asserted as properties of the RESOLVED value, not of a constant: a test
    /// that restates a constant passes whatever the constant says. A bearer
    /// token is exchanged over this origin, so cleartext is refused everywhere
    /// a second party could be listening.
    #[test]
    fn plaintext_is_refused_except_on_loopback() {
        for bad in ["http://api.example.com", "http://evil.test"] {
            let err = resolve_cloud_url(Some(bad), None).unwrap_err().to_string();
            assert!(
                err.contains("must be https"),
                "{bad} should be refused: {err}"
            );
        }
        for ok in ["http://localhost:8787", "http://127.0.0.1:8787"] {
            assert_eq!(resolve_cloud_url(Some(ok), None).unwrap(), ok);
        }
    }

    /// An exported-but-empty variable is a misconfiguration, not a request to
    /// connect to the empty string.
    #[test]
    fn an_empty_value_is_a_misconfiguration() {
        let err = resolve_cloud_url(None, Some("   "))
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "{err}");
    }

    /// A message for the clients that were actually configured: each one named,
    /// each with its own reload step, and no client that was not configured.
    fn configured(hosts: &[&'static str]) -> setup::ConnectWrites {
        setup::ConnectWrites {
            configured: hosts
                .iter()
                .map(|host| setup::ClientWrite {
                    host,
                    outcome: setup::WriteOutcome::Updated,
                })
                .collect(),
            ..setup::ConnectWrites::default()
        }
    }

    /// ACCEPTANCE: the closing message names EVERY client whose config now
    /// carries the entry, each with its own reload requirement — and never a
    /// client that was not configured.
    ///
    /// It used to assert exactly one client, which was honest while `connect`
    /// wrote exactly one. A user running two agents against one repository is
    /// owed both lines; the client that goes unmentioned is precisely the one
    /// that would sit there local while the terminal says "Connected".
    #[test]
    fn the_ready_message_names_every_client_it_configured() {
        for host in HOSTS {
            let msg = ready_message(&configured(&[host]), "https://api.example");
            assert!(msg.contains(host), "{host} must be named: {msg}");
            for other in HOSTS.iter().filter(|o| **o != host) {
                assert!(
                    !msg.contains(other),
                    "{host}'s ready line must not recite {other}: {msg}",
                );
            }
            assert!(
                !msg.contains(".json"),
                "no config-path recital in the closing message: {msg}",
            );
        }

        // Two clients configured: both named, both with their own step.
        let both = ready_message(
            &configured(&["Claude Code", "Cursor"]),
            "https://api.example",
        );
        assert!(
            both.contains("Claude Code") && both.contains("Cursor"),
            "{both}"
        );
        assert!(
            both.contains("/mcp") && both.contains("Tools & MCP"),
            "each client's own reload step, not one shared line: {both}",
        );
        assert!(
            !both.contains("Windsurf") && !both.contains("VS Code"),
            "a client that was not configured must not be named: {both}",
        );

        // A present client we never author for is named as the manual step it is.
        let with_manual = setup::ConnectWrites {
            manual: vec![setup::ManualStep::Unauthorable {
                host: "VS Code",
                config_file: ".vscode/mcp.json",
            }],
            ..configured(&["Claude Code"])
        };
        let msg = ready_message(&with_manual, "https://api.example");
        assert!(
            msg.contains("VS Code") && msg.contains(".vscode/mcp.json"),
            "a present, unconfigured client must be named with what to do: {msg}",
        );

        // Each client's reload requirement is its own, verified against the
        // hosts' current documentation — not a shared "restart your client".
        let url = "https://api.example";
        assert!(ready_message(&configured(&["Claude Code"]), url).contains("/mcp"));
        assert!(ready_message(&configured(&["Cursor"]), url).contains("Tools & MCP"));
        assert!(ready_message(&configured(&["Windsurf"]), url).contains("Cascade"));
        assert!(ready_message(&configured(&["VS Code"]), url).contains("MCP: List Servers"));
    }

    /// ACCEPTANCE (connection-is-a-first-class-object): a proven connect leaves
    /// a connection RECORD behind, and a failed one leaves none.
    ///
    /// This is the pair the reported bug turned on. `sync push` in a plain shell
    /// failed after a successful `connect` because the only thing connect wrote
    /// that named the workspace was an `env` block inside an MCP config file —
    /// readable by a process the MCP host spawned, and by nothing else. The
    /// record asserted here is what every CLI verb reads instead.
    ///
    /// The negative half is not decoration. A record written before the smoke
    /// would leave a machine claiming a connection the backend had just refused,
    /// which is the same class of lie as the MCP entry that used to be armed
    /// with sync wiring that could not work.
    #[tokio::test]
    async fn a_proven_connect_records_the_connection_and_a_refused_one_does_not() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();

        assert_eq!(
            crate::cloud::connection::load_in(cwd, TEST_PROJECT),
            None,
            "nothing is recorded before connecting",
        );

        let rejecting = backend_answering(401).await;
        run_finish(cwd, None, &store, &resolver, &rejecting.uri(), MINTED)
            .await
            .expect_err("a refused token must not connect");
        assert_eq!(
            crate::cloud::connection::load_in(cwd, TEST_PROJECT),
            None,
            "a connect the backend refused must not leave a connection behind",
        );

        let accepting = backend_answering(200).await;
        run_finish(cwd, None, &store, &resolver, &accepting.uri(), MINTED)
            .await
            .expect("an accepted token connects");

        let recorded = crate::cloud::connection::load_in(cwd, TEST_PROJECT)
            .expect("a proven connect records the connection");
        assert_eq!(recorded.cloud_url, accepting.uri());
        assert_eq!(recorded.profile, PROFILE, "a profile NAME, never a token");
        assert!(
            !recorded.connected_at.is_empty(),
            "the record says when it was proven",
        );

        // The record is the thing a shell reads, so prove the resolver reaches
        // it with NO environment supplied — the gate whose absence let the MCP
        // lane stand in for the shell lane.
        assert_eq!(
            crate::cloud::connection::pick(None, Some(&recorded.cloud_url)),
            Some((accepting.uri(), crate::cloud::connection::Source::Stored)),
        );

        let raw = std::fs::read_to_string(crate::cloud::connection::path_in(cwd)).unwrap();
        assert!(
            !raw.contains(MINTED),
            "the connection record must never hold the token:\n{raw}",
        );
    }

    /// ACCEPTANCE 1 (positive half): success is declared only after the token
    /// that was STORED authenticates one real request — the mock demands the
    /// Bearer header and exactly one call.
    #[tokio::test]
    async fn success_is_declared_only_after_the_stored_token_authenticates() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();

        let server = MockServer::start().await;
        let bearer = format!("Bearer {MINTED}");
        Mock::given(method("GET"))
            .and(path("/v1/records"))
            .and(header("authorization", bearer.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": [] })))
            .expect(1)
            .mount(&server)
            .await;

        let message = run_finish(cwd, None, &store, &resolver, &server.uri(), MINTED)
            .await
            .expect("an accepted token connects");

        assert!(message.contains("Connected"));
        assert!(message.contains("Claude Code"), "{message}");
        assert!(
            message.contains("/mcp"),
            "the reload requirement: {message}"
        );

        // The config is armed and secret-free — presence with absence.
        let text = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        let env = &doc["mcpServers"]["think-and-ship"]["env"];
        assert_eq!(env["THINK_AND_SHIP_SYNC_TARGET"], "cloud");
        assert_eq!(env[crate::cloud::credential::PROFILE_ENV], PROFILE);
        assert!(!text.contains(MINTED), "no secret in the config: {text}");

        server.verify().await;
    }

    /// THE INVARIANT THE WHOLE FIX EXISTS FOR, asserted end to end over the
    /// production tail: if the closing message promises that traces will sync,
    /// the config on disk must actually be able to sync them.
    ///
    /// Driven from an `init`-authored LOCAL entry with force:false, which is the
    /// exact pair of commands the docs tell a new user to run and the exact path
    /// that used to print "Connected … Your traces then sync automatically" over
    /// a config with no cloud wiring at all. The assertion is deliberately the
    /// implication rather than the symptom: it reads the promise out of the
    /// message and then demands the file back it up, so it stays true if the
    /// wording changes and fails for any future path that reintroduces a
    /// success message over an unwritten config.
    #[tokio::test]
    async fn a_promise_to_sync_is_only_made_over_a_config_that_can_sync() {
        let project = TempDir::new().unwrap();
        let cwd = project.path();
        // Precisely what `think-and-ship init` leaves behind: an entry, no cloud.
        std::fs::write(
            cwd.join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": {
                    "think-and-ship": {
                        "command": "think-and-ship",
                        "args": ["serve"],
                        "env": { "THINK_AND_SHIP_PERSIST": "true" }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let (_data, store, resolver) = store_and_resolver();
        let server = backend_answering(200).await;

        let message = run_finish(cwd, None, &store, &resolver, &server.uri(), MINTED)
            .await
            .expect("a valid credential over an init-authored entry connects");

        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(cwd.join(".mcp.json")).unwrap()).unwrap();
        let env = &doc["mcpServers"]["think-and-ship"]["env"];

        if message.contains("sync automatically") {
            assert_eq!(
                env["THINK_AND_SHIP_SYNC_TARGET"], "cloud",
                "the message promised syncing over a config that cannot sync:\n{message}\n{doc}",
            );
            assert_eq!(
                env[crate::cloud::credential::PROFILE_ENV],
                PROFILE,
                "the message promised syncing over a config naming no profile:\n{message}\n{doc}",
            );
            assert_eq!(
                env["THINK_AND_SHIP_CLOUD_URL"],
                server.uri(),
                "the message promised syncing over a config naming no backend:\n{message}\n{doc}",
            );
        } else {
            panic!("a successful connect over an existing entry must upgrade it:\n{message}");
        }
    }

    /// The other half of the same honesty: when the entry already matched,
    /// nothing changed, so the closing line must not send someone off to restart
    /// their editor for a change that did not happen.
    #[tokio::test]
    async fn a_reconnect_that_changed_nothing_does_not_announce_a_reload() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();
        let server = backend_answering(200).await;

        let first = run_finish(cwd, None, &store, &resolver, &server.uri(), MINTED)
            .await
            .unwrap();
        assert!(first.contains("sync automatically"), "{first}");

        let second = run_finish(cwd, None, &store, &resolver, &server.uri(), MINTED)
            .await
            .unwrap();
        assert!(second.contains("Connected"), "{second}");
        assert!(
            second.contains("nothing to reload"),
            "an unchanged reconnect must say nothing changed: {second}",
        );
        assert!(
            !second.contains("/mcp"),
            "no reload instruction for a write that did not happen: {second}",
        );
    }

    /// ACCEPTANCE 1 + 4: a rejected credential fails the command — no
    /// "Connected", no config write — and the retry runs to success with
    /// nothing to clean up first.
    #[tokio::test]
    async fn a_working_credential_survives_every_way_a_connect_can_fail() {
        // The reported defect, in the state that makes it hurt: this machine is
        // ALREADY connected and working, and a second connect goes wrong.
        const WORKING: &str = "cloud_tok_already_working";

        for (label, status) in [("refused", 401), ("backend broken", 503)] {
            let project = seeded_project();
            let cwd = project.path();
            let (_data, store, resolver) = store_and_resolver();
            crate::cloud::credential::adopt(&resolver, PROFILE, WORKING, "2026-07-30T00:00:00Z")
                .unwrap();

            let failing = backend_answering(status).await;
            let err = run_finish(cwd, None, &store, &resolver, &failing.uri(), MINTED)
                .await
                .unwrap_err();
            let msg = format!("{err:#}");

            assert_eq!(
                resolve(store.as_ref(), PROFILE).as_deref(),
                Some(WORKING),
                "{label}: the credential that was working must still be the one stored",
            );
            assert_eq!(
                resolve(
                    store.as_ref(),
                    &crate::cloud::credential::staging_profile(PROFILE)
                ),
                None,
                "{label}: the unproven token must not be left staged",
            );
            assert!(
                msg.contains("NOT been changed"),
                "{label}: a user whose connection survived must be told so: {msg}",
            );
        }
    }

    /// The first-connect case, which is the same rule pointing the other way:
    /// an unverified credential is worth more than no credential at all, so an
    /// unreachable backend stores it rather than leaving the machine empty.
    /// Refusing to overwrite is about protecting something that works — there is
    /// nothing here to protect.
    #[tokio::test]
    async fn an_unreachable_backend_still_stores_the_first_credential() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();
        assert_eq!(
            resolve(store.as_ref(), PROFILE),
            None,
            "precondition: empty"
        );

        let broken = backend_answering(503).await;
        let err = run_finish(cwd, None, &store, &resolver, &broken.uri(), MINTED)
            .await
            .unwrap_err();

        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(MINTED),
            "with nothing to protect, an unproven token beats none: {err:#}",
        );
        assert_eq!(
            resolve(
                store.as_ref(),
                &crate::cloud::credential::staging_profile(PROFILE)
            ),
            None,
            "the staged entry is cleaned up on this path too",
        );
    }

    #[tokio::test]
    async fn a_rejected_credential_fails_the_command_and_leaves_a_clean_retry() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();
        let before = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();

        let rejecting = backend_answering(401).await;
        let err = run_finish(cwd, None, &store, &resolver, &rejecting.uri(), MINTED)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("rejected"), "{msg}");
        assert!(
            msg.contains("Run `think-and-ship connect` again"),
            "one named corrective action: {msg}",
        );
        assert!(
            !msg.contains("Connected"),
            "a rejected credential must not read as success: {msg}",
        );
        assert_no_transport_internals(&msg);

        // The smoke gates the write: the config is byte-for-byte untouched...
        assert_eq!(
            std::fs::read_to_string(cwd.join(".mcp.json")).unwrap(),
            before,
            "a rejected credential must leave the MCP config alone",
        );
        // ...and NOTHING was stored. This assertion used to read
        // `Some(MINTED)` — the rejected token was adopted before it was proven,
        // and the test recorded that as acceptable because a retry replaced it.
        // It is not acceptable when the profile already held a working
        // credential, which is the case this staging exists for.
        assert_eq!(
            resolve(store.as_ref(), PROFILE),
            None,
            "a token the backend refused must never reach the real profile",
        );
        assert_eq!(
            resolve(
                store.as_ref(),
                &crate::cloud::credential::staging_profile(PROFILE)
            ),
            None,
            "and the staged entry must not survive the failure either",
        );

        let accepting = backend_answering(200).await;
        let message = run_finish(
            cwd,
            None,
            &store,
            &resolver,
            &accepting.uri(),
            "cloud_tok_reminted",
        )
        .await
        .expect("nothing from the failed connect may block the retry");
        assert!(message.contains("Claude Code"), "{message}");
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some("cloud_tok_reminted"),
            "the retry's token replaced the rejected one",
        );
        let text = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["mcpServers"]["think-and-ship"]["env"]["THINK_AND_SHIP_SYNC_TARGET"],
            "cloud",
        );
        assert!(!text.contains("cloud_tok_reminted"));
    }

    /// FAILURE MODE: no network. The message names the one action and no
    /// transport internals, and the retry is clean.
    #[tokio::test]
    async fn an_unreachable_backend_names_the_network_and_blocks_no_retry() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();
        let before = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();

        let dead = unreachable_url();
        let err = run_finish(cwd, None, &store, &resolver, &dead, MINTED)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Check your network connection"),
            "one named corrective action: {msg}",
        );
        assert_no_transport_internals(&msg);
        assert_eq!(
            std::fs::read_to_string(cwd.join(".mcp.json")).unwrap(),
            before
        );
        assert_eq!(resolve(store.as_ref(), PROFILE).as_deref(), Some(MINTED));

        let accepting = backend_answering(200).await;
        run_finish(cwd, None, &store, &resolver, &accepting.uri(), MINTED)
            .await
            .expect("a network blip must not leave state that blocks the retry");
    }

    /// FAILURE MODE: the sign-in resolution itself cannot reach the backend.
    #[tokio::test]
    async fn an_unreachable_backend_at_sign_in_names_the_network_action() {
        let dead = unreachable_url();
        let err = fetch_connect_config(&reqwest::Client::new(), &dead)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Check your network connection"), "{msg}");
        assert_no_transport_internals(&msg);
    }

    /// A backend that is reached but erroring is the service's problem, and
    /// the message says so instead of blaming the setup.
    #[tokio::test]
    async fn a_backend_5xx_is_not_blamed_on_the_user() {
        let broken = backend_answering(500).await;
        let failure = smoke_check(&reqwest::Client::new(), &broken.uri(), MINTED)
            .await
            .unwrap_err();
        assert_eq!(failure, SmokeFailure::Backend { status: 500 });
        let msg = failure.advice("https://api.example");
        assert!(msg.contains("again shortly"), "{msg}");
        assert_no_transport_internals(&msg);
    }

    /// FAILURE MODE: the entry lives in a config this tool will not rewrite.
    /// The corrective action is the exact host command, it carries no secret,
    /// and the stored token survives for the moment the human runs it.
    #[tokio::test]
    async fn an_unwritable_config_names_the_exact_host_command_and_keeps_the_token() {
        let project = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let home_config = home.path().join(".claude.json");
        let key = project.path().canonicalize().unwrap();
        std::fs::write(
            &home_config,
            serde_json::to_string_pretty(&json!({
                "projects": {
                    key.to_string_lossy(): {
                        "mcpServers": {
                            "think-and-ship": { "command": "think-and-ship" }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let before = std::fs::read_to_string(&home_config).unwrap();
        let (_data, store, resolver) = store_and_resolver();

        let accepting = backend_answering(200).await;
        let err = run_finish(
            project.path(),
            Some(home_config.clone()),
            &store,
            &resolver,
            &accepting.uri(),
            MINTED,
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("claude mcp add-json think-and-ship"),
            "the corrective action is the exact command: {msg}",
        );
        assert!(
            !msg.contains(MINTED),
            "the command must carry no secret: {msg}"
        );
        assert_no_transport_internals(&msg);

        assert_eq!(
            std::fs::read_to_string(&home_config).unwrap(),
            before,
            "the file we refuse to rewrite is left byte-for-byte alone",
        );
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(MINTED),
            "the verified token stays stored — running the printed command is all that is left",
        );
    }

    /// Two configs holding the entry used to STOP the connect: writing to one
    /// would have left the other stale, and which one the agent reads is the
    /// host's decision, so the user was asked to delete one and run it again.
    ///
    /// Writing to both answers that on its own terms — neither is stale — and it
    /// removes the one demand `connect` had no business making. The end state is
    /// what matters and it is asserted on the files, not the message: both
    /// entries carry the same wiring, and neither holds a secret.
    #[tokio::test]
    async fn two_live_entries_are_both_connected_rather_than_one_being_deleted() {
        let project = seeded_project();
        let cwd = project.path();
        std::fs::create_dir_all(cwd.join(".cursor")).unwrap();
        std::fs::write(
            cwd.join(".cursor/mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "think-and-ship": { "command": "think-and-ship" } }
            }))
            .unwrap(),
        )
        .unwrap();
        let (_data, store, resolver) = store_and_resolver();

        let accepting = backend_answering(200).await;
        let message = run_finish(cwd, None, &store, &resolver, &accepting.uri(), MINTED)
            .await
            .expect("two clients is not a reason to refuse to connect either of them");
        assert!(
            message.contains("Claude Code") && message.contains("Cursor"),
            "both clients named with their own reload step: {message}",
        );
        assert_eq!(resolve(store.as_ref(), PROFILE).as_deref(), Some(MINTED));

        for rel in [".mcp.json", ".cursor/mcp.json"] {
            let after: Value =
                serde_json::from_str(&std::fs::read_to_string(cwd.join(rel)).unwrap()).unwrap();
            let env = &after["mcpServers"]["think-and-ship"]["env"];
            assert_eq!(
                env["THINK_AND_SHIP_CLOUD_URL"],
                accepting.uri(),
                "{rel} must name the backend rather than be left stale: {after}",
            );
            assert!(
                env.get(crate::cloud::credential::TOKEN_ENV).is_none(),
                "{rel} must still hold no secret: {after}",
            );
        }
    }

    /// `token` prints a secret, so exactly ONE place in this crate may print a
    /// resolved token, and it is the verb the user typed to get one.
    ///
    /// Structural, because the hazard is a line somebody adds later: `status`
    /// and `doctor` output gets pasted into support threads, and a token that
    /// leaks into either is a live credential in a public message. Behaviour
    /// cannot catch a leak that has not been written yet.
    #[test]
    fn only_the_token_verb_ever_prints_a_token() {
        let sources = [
            ("cli/connect.rs", include_str!("connect.rs")),
            ("cli/setup.rs", include_str!("setup.rs")),
            ("cli/mod.rs", include_str!("mod.rs")),
        ];
        for (name, src) in sources {
            // Only the production half — this test's own assertion strings name
            // the pattern they forbid, and counting them would make it lie.
            //
            // The production half ends at the first TOP-LEVEL test module, and
            // the marker is anchored at column zero for a reason: `setup.rs`
            // carries an INDENTED `#[cfg(test)]` helper a couple of hundred
            // lines in, so a bare substring match ends "production" there and
            // silently stops reading most of the file. The size floor below is
            // the alarm for the next time a marker quietly stops matching,
            // because a scan that reads almost nothing passes almost anything.
            let lines: Vec<&str> = src.lines().collect();
            let end = lines
                .iter()
                .position(|l| *l == "#[cfg(test)]")
                .unwrap_or(lines.len());
            let production = lines[..end].join("\n");
            assert!(
                production.len() > src.len() / 3,
                "{name}: the production half is only {} of {} bytes — the marker \
                 probably stopped matching",
                production.len(),
                src.len(),
            );
            for (i, line) in production.lines().enumerate() {
                let prints_token = (line.contains("println!") || line.contains("eprintln!"))
                    && (line.contains("{token}") || line.contains("{stored_token}"));
                if prints_token {
                    let preceding: Vec<&str> = production.lines().take(i).collect();
                    let inside_the_verb = name == "cli/connect.rs"
                        && preceding
                            .iter()
                            .rev()
                            .take(40)
                            .any(|l| l.contains("pub fn print_token"));
                    assert!(
                        inside_the_verb,
                        "{name}:{} prints a token outside the `token` verb: {line}",
                        i + 1,
                    );
                }
            }
        }
    }

    /// THE WIRING, not just the rule. A gate that proves `identify_caller`
    /// works while never checking that the connect tail CONSULTS the caller
    /// would stay green through the bug it guards — the previous chunk shipped
    /// exactly that mistake once already.
    ///
    /// So this drives the whole tail with a caller that names Cursor, in a
    /// project with no `.cursor/` directory anywhere, and asserts Cursor's own
    /// config came out of it.
    #[tokio::test]
    async fn the_caller_reaches_the_write_through_the_whole_connect_tail() {
        let project = seeded_project();
        let cwd = project.path();
        assert!(
            !cwd.join(".cursor").exists(),
            "precondition: nothing on disk says Cursor",
        );
        let (_data, store, resolver) = store_and_resolver();
        let accepting = backend_answering(200).await;

        let message = finish_connect_in(ConnectTail {
            http: &reqwest::Client::new(),
            cwd,
            home_config: None,
            store: store.as_ref(),
            resolver: &resolver,
            profile: PROFILE,
            cloud_url: &accepting.uri(),
            minted: MINTED,
            minted_jti: MINTED_JTI,
            name: NAME,
            record_dir: cwd,
            project_id: TEST_PROJECT,
            caller: &setup::CallerEnv::from_env_for_test(&["CURSOR_TRACE_ID"]),
            workos_token: WORKOS,
            force: false,
        })
        .await
        .expect("a Cursor caller connects");

        let cursor = cwd.join(".cursor/mcp.json");
        assert!(
            cursor.exists(),
            "the client that ran the command must be configured: {message}",
        );
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"]["think-and-ship"]["env"]["THINK_AND_SHIP_CLOUD_URL"],
            accepting.uri(),
        );
        assert!(message.contains("Cursor"), "{message}");
    }

    /// The emitted host command, executed for REAL: `claude mcp add-json …`
    /// against a throwaway HOME, exactly as printed to the user. This is the
    /// one hop the wiremock tests cannot cover — whether the real `claude`
    /// CLI accepts what we tell people to paste.
    ///
    /// Run with: cargo test -p think-and-ship --lib the_real_claude_cli -- --ignored --nocapture
    #[test]
    #[ignore = "runs the real `claude` CLI against a throwaway HOME"]
    fn the_real_claude_cli_accepts_the_emitted_add_json_command() {
        let have = std::process::Command::new("sh")
            .args(["-c", "command -v claude"])
            .output()
            .unwrap();
        assert!(
            have.status.success(),
            "this live test needs the `claude` CLI on PATH",
        );

        let project = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let home_config = home.path().join(".claude.json");
        let key = project.path().canonicalize().unwrap();
        std::fs::write(
            &home_config,
            serde_json::to_string_pretty(&json!({
                "projects": {
                    key.to_string_lossy(): {
                        "mcpServers": {
                            "think-and-ship": { "command": "think-and-ship" }
                        }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        // The command the user is told to run, taken from the actual message.
        let err = setup::write_cloud_mcp_config_in(
            project.path(),
            Some(home_config.clone()),
            &setup::CallerEnv::unknown(),
            "https://api.example",
            PROFILE,
            false,
            true,
        )
        .unwrap_err();
        let text = format!("{err:#}");
        let command = text
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("claude mcp remove"))
            .expect("the corrective action embeds the exact remove-then-add command");

        let out = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(project.path())
            .env("HOME", home.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the emitted command must run as printed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        let written = std::fs::read_to_string(&home_config).unwrap();
        assert!(
            written.contains(crate::cloud::credential::PROFILE_ENV) && written.contains(PROFILE),
            "the profile wiring reached ~/.claude.json: {written}",
        );
        assert!(
            written.contains("THINK_AND_SHIP_SYNC_TARGET"),
            "the sync arming reached ~/.claude.json: {written}",
        );
    }

    /// A store whose write always fails — the dogfood keychain, distilled.
    struct FailingStore;

    impl crate::tracker::credential::CredentialStore for FailingStore {
        fn load(
            &self,
            _provider: &str,
        ) -> Result<
            Option<crate::tracker::credential::StoredCredential>,
            crate::tracker::credential::CredentialError,
        > {
            Ok(None)
        }
        fn save(
            &self,
            _credential: &crate::tracker::credential::StoredCredential,
        ) -> Result<(), crate::tracker::credential::CredentialError> {
            Err(crate::tracker::credential::CredentialError::Invalid(
                "the keychain refused the write".into(),
            ))
        }
        fn delete(
            &self,
            _provider: &str,
        ) -> Result<(), crate::tracker::credential::CredentialError> {
            Ok(())
        }
        fn providers(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// Drive [`finish_connect_in`] over a store that refuses the write —
    /// the exact shape of the dogfood failure — against `cloud_url`.
    async fn run_finish_with_failing_store(cwd: &Path, cloud_url: &str) -> Result<String> {
        let failing = Arc::new(FailingStore);
        let resolver = Resolver::new(failing.clone());
        finish_connect_in(ConnectTail {
            http: &reqwest::Client::new(),
            cwd,
            home_config: None,
            store: failing.as_ref(),
            resolver: &resolver,
            profile: PROFILE,
            cloud_url,
            minted: MINTED,
            minted_jti: MINTED_JTI,
            name: NAME,
            record_dir: cwd,
            project_id: TEST_PROJECT,
            // An environment naming NOBODY, so these tests describe a project
            // rather than whichever client happens to be running the suite.
            caller: &setup::CallerEnv::unknown(),
            workos_token: WORKOS,
            force: false,
        })
        .await
    }

    /// ACCEPTANCE (connect-mints-before-it-can-store): the dogfood defect,
    /// distilled. The mint succeeded — the workspace is already listing this
    /// connection — and then the credential store refuses the write. The
    /// command must not walk away: it revokes the registration it just proved
    /// it cannot use, at the mint's jti, with the WorkOS bearer that
    /// authorized the mint.
    ///
    /// The mock IS the registry assertion: `.expect(1)` on the exact revoke
    /// route with the exact bearer, verified at the end. A run that exits
    /// without revoking fails this — which is precisely how the orphan
    /// 'Claude Code on Alriks-MacBook-Pro' was born.
    #[tokio::test]
    async fn a_storage_failure_after_the_mint_revokes_the_fresh_registration() {
        let project = seeded_project();
        let cwd = project.path();
        let before = std::fs::read_to_string(cwd.join(".mcp.json")).unwrap();

        let server = MockServer::start().await;
        let bearer = format!("Bearer {WORKOS}");
        Mock::given(method("POST"))
            .and(path(format!("/v1/agent-tokens/{MINTED_JTI}/revoke")))
            .and(header("authorization", bearer.as_str()))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "revoked": true, "jti": MINTED_JTI })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let err = run_finish_with_failing_store(cwd, &server.uri())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("credential store"),
            "the real failure leads the message: {msg}",
        );
        assert!(
            msg.contains("was revoked"),
            "the cleanup is stated, not silent: {msg}",
        );
        assert!(!msg.contains("Connected"), "{msg}");
        assert_no_transport_internals(&msg);
        // The failure came before the proof, so the config write never ran.
        assert_eq!(
            std::fs::read_to_string(cwd.join(".mcp.json")).unwrap(),
            before,
            "a pre-proof failure must leave the MCP config alone",
        );
        server.verify().await;
    }

    /// The double failure: the store refused the write AND the revoke did not
    /// go through. The message must then name the residue precisely —
    /// connection name and token id — because the app has no revoke surface
    /// yet, so what this line prints is the only handle a human gets.
    #[tokio::test]
    async fn a_failed_revoke_names_exactly_what_was_left_behind() {
        let project = seeded_project();
        let cwd = project.path();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/agent-tokens/{MINTED_JTI}/revoke")))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = run_finish_with_failing_store(cwd, &server.uri())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(NAME),
            "the residue names the connection: {msg}"
        );
        assert!(
            msg.contains(MINTED_JTI),
            "the residue names the token id: {msg}",
        );
        assert!(
            msg.contains("could not") && msg.contains("revoke"),
            "the failure to clean up is stated as a failure: {msg}",
        );
        assert!(
            !msg.contains("was revoked"),
            "no cleanup may be claimed that did not happen: {msg}",
        );
        assert_no_transport_internals(&msg);
    }

    /// The boundary of the compensation: a smoke interrupted by the backend
    /// (5xx here) proves nothing about the credential — it is stored intact
    /// and may work the moment the blip passes. Revoking there would destroy
    /// a healthy connection over a hiccup, so the registration is left alone
    /// on BOTH sides: no revoke request reaches the backend, and the stored
    /// token stays for the retry.
    #[tokio::test]
    async fn an_interrupted_smoke_does_not_revoke_the_stored_credential() {
        let project = seeded_project();
        let cwd = project.path();
        let (_data, store, resolver) = store_and_resolver();
        let server = backend_answering(500).await;

        let err = run_finish(cwd, None, &store, &resolver, &server.uri(), MINTED)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("again shortly"), "{msg}");

        let revokes: Vec<_> = server
            .received_requests()
            .await
            .expect("the mock must record requests")
            .into_iter()
            .filter(|r| r.url.path().contains("/revoke"))
            .collect();
        assert!(
            revokes.is_empty(),
            "an unproven credential must not be revoked: {revokes:?}",
        );
        assert_eq!(
            resolve(store.as_ref(), PROFILE).as_deref(),
            Some(MINTED),
            "the stored token survives the blip for the retry",
        );
    }
}
