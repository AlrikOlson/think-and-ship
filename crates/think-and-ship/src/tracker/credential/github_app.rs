//! Registering a GitHub App from a manifest: every permission decided in code,
//! and the whole credential set delivered machine-to-machine.
//!
//! # What this replaces
//!
//! The status quo for a GitHub App is a human walking a settings page, ticking
//! permission checkboxes, then copying a private key, a webhook secret and a
//! client secret out of a browser and pasting them into a terminal. Two things
//! are wrong with it and neither is aesthetic: the permission set is chosen by
//! whoever is clicking, and the safety of three secrets depends on how careful
//! someone was with a clipboard.
//!
//! The manifest flow fixes both. We POST a JSON manifest that pre-sets the
//! permissions and events; GitHub hands back a one-hour `code`; we exchange it
//! for `id`, `slug`, `client_id`, `client_secret`, `pem` and `webhook_secret` in
//! a single call. The webhook secret is never shown to a human at all.
//!
//! # The two irreducible human steps
//!
//! Stated here rather than hidden, because a reader deserves to know where the
//! automation stops:
//!
//! 1. **CREATE.** The human clicks *Create GitHub App* on GitHub's own page.
//!    The manifest has already made every decision; the click is consent to a
//!    registration, and GitHub does not allow it to be scripted.
//! 2. **INSTALL.** A second, separate consent installs the app on an account or
//!    organisation. It is a distinct grant from creating the registration and it
//!    is likewise not scriptable.
//!
//! Everything before and after those two clicks is code.
//!
//! # The redirect is the loopback receiver we already own
//!
//! [`LoopbackReceiver`] catches this redirect unchanged — it is the same shape
//! as an OAuth callback, and it was already hardened for the browsers that send
//! favicon fetches and speculative connections alongside the navigation. There
//! is deliberately no second receiver here.
//!
//! **The wildcard port is settled for GitHub, and only for GitHub.**
//! GitHub's OAuth documentation carves loopback callbacks out of its port-matching
//! rule: for a loopback URL the `redirect_uri` need not match the port registered
//! on the app. So [`LoopbackReceiver::bind`]`(0)` — an OS-assigned port — is
//! correct here. That is GitHub's policy written against RFC 8252's
//! recommendation; it is *not* evidence about what the Atlassian developer
//! console accepts, and the Jira half of the same question is still open.
//!
//! # Why the permission set is a `const` and not a parameter
//!
//! A permission must never be acquired by accident. Written as a constant with a
//! test asserting the exact set, adding one is a reviewable diff with a name on
//! it. Passed in as an argument, it becomes whatever a call site felt like — and
//! the failure mode of an over-broad GitHub App is silent until it is not.
//!
//! The key names are also a trap worth naming. GitHub's *permissions required*
//! documentation calls the Projects permission `projects`, because that is its
//! display name. The App Permissions **schema** — the thing a manifest is
//! validated against — calls it `organization_projects`. A manifest built from
//! the display name is rejected at registration time with a message about a
//! default permission record that is "not included in the list".
//!
//! # `organization_projects` is necessary and is NOT sufficient
//!
//! Settled after this module shipped, and recorded here because the
//! tempting reaction is to edit the permission set and the permission set is not
//! what is wrong.
//!
//! The key names ORGS, and it means it: an installation token cannot reach a
//! **user-owned** Projects v2 board at all. There is no user-projects permission
//! to request instead — the App permission vocabulary does not contain one, and
//! neither does the fine-grained-PAT vocabulary that shares it. A user-owned
//! board needs a USER token carrying the `project` / `read:project` scope, so
//! the App lane and the board lane authenticate differently.
//!
//! `organization_projects: write` therefore STAYS — it is exactly right for an
//! org-owned board, which is the case this app exists to serve. What must not
//! happen is a caller concluding from its presence that the installation token
//! can serve every board. The decision lives, total and testable, on
//! [`OwnerKind::required_credential`], and that is the thing to consult.
//!
//! [`LoopbackReceiver`]: super::authcode::LoopbackReceiver
//! [`LoopbackReceiver::bind`]: super::authcode::LoopbackReceiver::bind
//! [`OwnerKind::required_credential`]: crate::tracker::projects_v2::OwnerKind::required_credential

use std::collections::BTreeMap;

use serde::Deserialize;

use super::domain::{REDACTED, Secret};
use super::store::CredentialError;

/// Where a manifest registration is POSTed and where the conversion is called.
const GITHUB_WEB: &str = "https://github.com";
const GITHUB_API: &str = "https://api.github.com";

/// One permission the app asks for, and the level it asks for it at.
///
/// A tuple would have done; a named pair means the `why` below sits next to the
/// thing it justifies rather than in a comment that drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permission {
    /// The key as the App Permissions **schema** spells it — not as the
    /// permissions-required docs page displays it.
    pub key: &'static str,
    /// `read`, `write`, or `admin`.
    pub level: &'static str,
    /// Why this app needs it. Present so that adding a permission requires
    /// writing down a reason in the same diff.
    pub why: &'static str,
}

/// THE permission set. Every entry is justified; every plausible neighbour that
/// is absent is justified in [`the reasoned exclusions`](EXCLUDED_PERMISSIONS).
///
/// Sorted by key, so a diff that adds one lands in a predictable place.
pub const DEFAULT_PERMISSIONS: &[Permission] = &[
    Permission {
        key: "issues",
        level: "write",
        why: "the projector creates and updates issues from roadmap chunks — the adapter's whole job",
    },
    Permission {
        key: "metadata",
        level: "read",
        why: "GitHub requires it of every app; without it the app cannot resolve the repository it was installed on",
    },
    Permission {
        key: "organization_projects",
        level: "write",
        why: "the board lane writes a chunk's lifecycle onto a Projects v2 board. NOTE the key: the docs display this as 'projects'; the schema spells it 'organization_projects', and the display name is rejected",
    },
];

/// The events the app subscribes to.
///
/// One entry, because one is all anything reads: `backend/src/github.ts` handles
/// the `issues` event and ignores every action but `opened`. Subscribing to more
/// would be signing up for traffic no code path consumes.
pub const DEFAULT_EVENTS: &[&str] = &["issues"];

/// Permissions deliberately NOT requested, with the reason.
///
/// This exists so that "we did not ask for X" is a recorded decision rather than
/// an omission nobody noticed. It is documentation with a compiler behind it: a
/// key cannot appear in both lists (see the test).
pub const EXCLUDED_PERMISSIONS: &[(&str, &str)] = &[
    (
        "repository_projects",
        "classic Projects (columns and cards). Projects v2 is reached through organization_projects; this one buys nothing and grants board admin",
    ),
    (
        "contents",
        "nothing in this repo reads or writes repository files through the App",
    ),
    (
        "pull_requests",
        "the tracker projects chunks onto issues, never onto pull requests",
    ),
    (
        "members",
        "assignees are resolved from the issue payload, never by enumerating an organisation",
    ),
    (
        "administration",
        "there is no repository setting this app has any business changing",
    ),
];

/// The manifest GitHub validates the registration against.
///
/// Only `url` and `hook_attributes.url` are required by GitHub. Everything else
/// here is set anyway, because a field left unset is a field GitHub or a human
/// decides for us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppManifest {
    /// The app's display name. GitHub rejects a name already taken globally.
    pub name: String,
    /// The app's homepage. REQUIRED by GitHub.
    pub url: String,
    /// Where webhook deliveries go. REQUIRED by GitHub.
    pub hook_url: String,
    /// Where GitHub sends the temporary code after the CREATE click. This is
    /// the loopback receiver's URI.
    pub redirect_url: String,
    pub description: String,
    /// `false` keeps the registration installable only by its owner, which is
    /// the right default for something a single account is onboarding.
    pub public: bool,
}

impl AppManifest {
    /// Build the manifest for a redirect the caller has already bound.
    ///
    /// `redirect_url` comes from [`LoopbackReceiver::redirect_uri`], so the port
    /// in it is the port actually listening.
    ///
    /// [`LoopbackReceiver::redirect_uri`]: super::authcode::LoopbackReceiver::redirect_uri
    #[must_use]
    pub fn new(name: &str, homepage: &str, hook_url: &str, redirect_url: &str) -> Self {
        Self {
            name: name.trim().to_string(),
            url: homepage.trim().to_string(),
            hook_url: hook_url.trim().to_string(),
            redirect_url: redirect_url.trim().to_string(),
            description: "Mirrors a think-and-ship roadmap onto GitHub Issues and a Projects v2 \
                          board."
                .into(),
            public: false,
        }
    }

    /// The manifest as GitHub reads it.
    ///
    /// `default_permissions` and `default_events` are rendered from the
    /// constants above and nowhere else, so there is exactly one place a
    /// permission can be added.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let permissions: serde_json::Map<String, serde_json::Value> = DEFAULT_PERMISSIONS
            .iter()
            .map(|p| (p.key.to_string(), serde_json::Value::from(p.level)))
            .collect();

        serde_json::json!({
            "name": self.name,
            "url": self.url,
            "description": self.description,
            "hook_attributes": { "url": self.hook_url, "active": true },
            "redirect_url": self.redirect_url,
            "public": self.public,
            "default_permissions": permissions,
            "default_events": DEFAULT_EVENTS,
        })
    }
}

/// Whose account the app is registered under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The signed-in user's own account.
    Personal,
    /// An organisation, by login.
    Organization(String),
}

impl Owner {
    /// Where the browser POSTs the manifest, `state` included.
    ///
    /// The two paths are genuinely different endpoints rather than one endpoint
    /// with a parameter, which is why this is a function and not a format
    /// string at a call site.
    #[must_use]
    pub fn registration_url(&self, state: &str) -> String {
        match self {
            Self::Personal => format!("{GITHUB_WEB}/settings/apps/new?state={}", encode(state)),
            Self::Organization(org) => format!(
                "{GITHUB_WEB}/organizations/{}/settings/apps/new?state={}",
                encode(org.trim()),
                encode(state)
            ),
        }
    }
}

/// Percent-encode a value for a query string or a path segment.
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

/// Escape a value for an HTML attribute.
///
/// The manifest is JSON in an attribute, so it is full of `"` and it is the one
/// input here that would break the page if it were interpolated raw.
fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The page that carries the manifest to GitHub.
///
/// A manifest must be **POSTed**, so there is no URL to open — the browser has
/// to submit a form. This renders a self-submitting one for the caller to write
/// somewhere and open. It is pure: no file is written and no browser is opened
/// here, which is what makes the exact form contents testable.
///
/// The noscript body is not decoration. If scripting is off, the human still
/// gets a button rather than a blank page.
#[must_use]
pub fn manifest_form_html(manifest: &AppManifest, owner: &Owner, state: &str) -> String {
    let action = owner.registration_url(state);
    let payload = escape_attr(&manifest.to_json().to_string());
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Create the GitHub App</title></head>\n<body>\n\
         <form id=\"manifest\" method=\"post\" action=\"{action}\">\n\
         <input type=\"hidden\" name=\"manifest\" value=\"{payload}\">\n\
         <noscript><p>Creating the app on GitHub. Review the permissions on the \
         next page, then click <strong>Create GitHub App</strong>.</p>\
         <button type=\"submit\">Continue to GitHub</button></noscript>\n\
         </form>\n\
         <script>document.getElementById('manifest').submit()</script>\n\
         </body></html>\n"
    )
}

/// What GitHub hands back once the temporary code is converted.
///
/// Three of these fields are secrets and one of them — `webhook_secret` — is a
/// secret no human ever needs to see, which is strictly better than the flow
/// this replaces.
#[derive(Clone, Deserialize)]
pub struct RegisteredApp {
    /// The numeric App ID, used when signing an installation JWT.
    pub id: i64,
    /// The URL slug, e.g. `my-app`.
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub name: String,
    /// The registration's page on GitHub — where the human goes to INSTALL it.
    #[serde(default)]
    pub html_url: String,
    /// OAuth client id. Not a secret; it travels in an authorize URL.
    #[serde(default)]
    pub client_id: String,
    pub client_secret: Secret,
    /// The PEM-encoded private key. GitHub shows this exactly once, here.
    pub pem: Secret,
    /// `string or null` in GitHub's schema: absent when the registration
    /// declared no webhook.
    #[serde(default)]
    pub webhook_secret: Option<Secret>,
    /// What GitHub actually recorded, which is not automatically what we asked
    /// for — see [`RegisteredApp::unrequested_permissions`].
    #[serde(default)]
    pub permissions: BTreeMap<String, String>,
    #[serde(default)]
    pub events: Vec<String>,
}

impl RegisteredApp {
    /// Permissions GitHub recorded that the manifest did not request.
    ///
    /// The manifest is the request, not the outcome. This is the negative-space
    /// check: an empty result is the claim "nothing was acquired by accident",
    /// and it is the only form of that claim which can fail.
    #[must_use]
    pub fn unrequested_permissions(&self) -> Vec<String> {
        self.permissions
            .iter()
            .filter(|(key, _)| !DEFAULT_PERMISSIONS.iter().any(|p| p.key == key.as_str()))
            .map(|(key, level)| format!("{key}={level}"))
            .collect()
    }

    /// The whole human-facing summary of a registration.
    ///
    /// Everything a person needs to finish onboarding, and not one byte of
    /// secret material. It names the INSTALL step because that is the second of
    /// the two clicks and the flow is not finished without it.
    #[must_use]
    pub fn report(&self) -> String {
        let mut out = format!(
            "Registered GitHub App '{}' (id {}, slug {}).\n  \
             private key, client secret and webhook secret were delivered directly and stored — \
             none of them was displayed.\n  \
             next: install it, which is a separate consent: {}/installations/new",
            self.name, self.id, self.slug, self.html_url
        );
        let extra = self.unrequested_permissions();
        if !extra.is_empty() {
            out.push_str(&format!(
                "\n  WARNING: GitHub recorded permissions the manifest did not request: {}",
                extra.join(", ")
            ));
        }
        out
    }
}

/// Redacting by hand rather than by derive, because a derived `Debug` on this
/// struct is precisely the leak.
impl std::fmt::Debug for RegisteredApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredApp")
            .field("id", &self.id)
            .field("slug", &self.slug)
            .field("client_id", &self.client_id)
            .field("permissions", &self.permissions)
            .field("events", &self.events)
            .field("client_secret", &REDACTED)
            .field("pem", &REDACTED)
            .field("webhook_secret", &REDACTED)
            .finish()
    }
}

/// Exchange the temporary code for the app's credentials.
///
/// UNAUTHENTICATED and SINGLE-SHOT: the code is itself the proof, it is spendable
/// exactly once, and it expires one hour after the CREATE click.
pub async fn convert_manifest(
    http: &reqwest::Client,
    code: &str,
) -> Result<RegisteredApp, CredentialError> {
    convert_manifest_at(http, GITHUB_API, code).await
}

/// The same call against an explicit base, so a test can point it at a mock
/// without the production path growing a configurable endpoint.
pub async fn convert_manifest_at(
    http: &reqwest::Client,
    api_base: &str,
    code: &str,
) -> Result<RegisteredApp, CredentialError> {
    let code = code.trim();
    if code.is_empty() {
        return Err(CredentialError::Invalid(
            "no manifest code came back from GitHub, so there is nothing to convert".into(),
        ));
    }

    let resp = http
        .post(format!(
            "{}/app-manifests/{}/conversions",
            api_base.trim_end_matches('/'),
            encode(code)
        ))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "think-and-ship")
        // No Authorization header, deliberately: this endpoint is unauthenticated
        // and the code is the credential. Sending a token here is a way to leak
        // one into a request that did not need it.
        .header("Content-Length", "0")
        .send()
        .await
        .map_err(|e| {
            CredentialError::Invalid(format!("converting the GitHub App manifest: {e}"))
        })?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        // The code is in the URL and can be echoed in a body. It is single-shot
        // and it is the whole credential set, so it is scrubbed before this
        // message reaches anyone's terminal or log.
        let body = redact_code(&body, code);
        return Err(CredentialError::Invalid(format!(
            "GitHub refused the manifest conversion with {status}: {body} — a manifest code is \
             valid for ONE HOUR and can be spent only once, so if the browser step happened a \
             while ago, or this conversion already ran, start the registration again"
        )));
    }

    let app = resp
        .json::<RegisteredApp>()
        .await
        .map_err(|e| CredentialError::Invalid(format!("the converted app was unreadable: {e}")))?;

    // The one log line this flow emits, and it is emitted on purpose: a
    // registration that leaves no trace is indistinguishable from one that never
    // happened. Fields are named individually rather than logging the struct,
    // because `?app` would render Debug and the next person to add a field would
    // be relying on someone remembering to redact it.
    tracing::info!(
        app_id = app.id,
        slug = %app.slug,
        permissions = app.permissions.len(),
        events = app.events.len(),
        "registered a GitHub App from a manifest"
    );

    Ok(app)
}

/// Replace the manifest code wherever a provider echoed it back.
///
/// Separate and named so it cannot be quietly dropped from the error path: the
/// only reason the failure branch is safe to print is this call.
fn redact_code(body: &str, code: &str) -> String {
    if code.is_empty() {
        return body.to_string();
    }
    body.replace(code, REDACTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> AppManifest {
        AppManifest::new(
            "think-and-ship",
            "https://example.com",
            "https://example.test/hooks/github",
            "http://127.0.0.1:54321/callback",
        )
    }

    /// THE assertion this test exists for: the EXACT set, by key and level.
    ///
    /// Written as a whole-set equality rather than a series of `contains`
    /// checks, because `contains` passes just as happily when a fourth
    /// permission has been added.
    #[test]
    fn the_permission_set_is_exactly_these_three() {
        let actual: Vec<(&str, &str)> = DEFAULT_PERMISSIONS
            .iter()
            .map(|p| (p.key, p.level))
            .collect();
        assert_eq!(
            actual,
            vec![
                ("issues", "write"),
                ("metadata", "read"),
                ("organization_projects", "write"),
            ],
            "a permission changed. That is allowed — but it must be a reviewed diff on THIS \
             line, not a surprise found in a settings page later."
        );
    }

    /// The naming trap, pinned. `projects` is the DISPLAY name; a manifest
    /// carrying it is rejected by GitHub.
    #[test]
    fn the_projects_permission_uses_its_schema_key_not_its_display_name() {
        let keys: Vec<&str> = DEFAULT_PERMISSIONS.iter().map(|p| p.key).collect();
        assert!(keys.contains(&"organization_projects"));
        assert!(
            !keys.contains(&"projects"),
            "'projects' is what the docs DISPLAY; the schema key is 'organization_projects' and \
             the display name fails validation at registration"
        );
    }

    /// Every permission carries a reason, and no key is both requested and
    /// excluded — the two lists cannot silently contradict each other.
    #[test]
    fn every_permission_is_justified_and_the_two_lists_do_not_overlap() {
        for p in DEFAULT_PERMISSIONS {
            assert!(!p.why.trim().is_empty(), "{} has no reason", p.key);
            assert!(
                matches!(p.level, "read" | "write" | "admin"),
                "{} has level '{}', which GitHub does not accept",
                p.key,
                p.level
            );
            assert!(
                !EXCLUDED_PERMISSIONS.iter().any(|(k, _)| *k == p.key),
                "{} is in both the requested and the excluded list",
                p.key
            );
        }
        for (key, why) in EXCLUDED_PERMISSIONS {
            assert!(!why.trim().is_empty(), "{key} is excluded with no reason");
        }
    }

    /// One event, because one event is all anything reads.
    #[test]
    fn the_event_set_is_exactly_issues() {
        assert_eq!(DEFAULT_EVENTS, &["issues"]);
    }

    /// The manifest renders the constants — not a hand-written copy of them,
    /// which is how the two drift.
    #[test]
    fn the_manifest_renders_the_pinned_permissions_and_events() {
        let json = manifest().to_json();

        let permissions = json["default_permissions"]
            .as_object()
            .expect("default_permissions must be an object");
        assert_eq!(permissions.len(), DEFAULT_PERMISSIONS.len());
        for p in DEFAULT_PERMISSIONS {
            assert_eq!(permissions[p.key], serde_json::Value::from(p.level));
        }

        assert_eq!(json["default_events"], serde_json::json!(["issues"]));
        // The two GitHub actually requires.
        assert_eq!(json["url"], "https://example.com");
        assert_eq!(
            json["hook_attributes"]["url"],
            "https://example.test/hooks/github"
        );
        // A registration nobody else can install by accident.
        assert_eq!(json["public"], false);
        assert_eq!(json["redirect_url"], "http://127.0.0.1:54321/callback");
    }

    /// Personal and organisation registrations are different endpoints, and
    /// getting them the wrong way round registers the app under the wrong owner.
    #[test]
    fn the_registration_url_differs_by_owner_and_carries_the_state() {
        let personal = Owner::Personal.registration_url("st ate");
        assert_eq!(
            personal,
            "https://github.com/settings/apps/new?state=st%20ate"
        );

        let org = Owner::Organization("Acme Inc".into()).registration_url("s1");
        assert_eq!(
            org,
            "https://github.com/organizations/Acme%20Inc/settings/apps/new?state=s1"
        );
        assert!(
            !org.contains("/settings/apps/new?state=s1") || org.contains("/organizations/"),
            "an org registration must not fall back to the personal endpoint"
        );
    }

    /// The manifest is JSON inside an HTML attribute, so it is all quotes. An
    /// unescaped one truncates the payload and GitHub receives a fragment.
    #[test]
    fn the_form_escapes_the_manifest_into_the_attribute() {
        let html = manifest_form_html(&manifest(), &Owner::Personal, "s-1");

        assert!(html.contains("method=\"post\""));
        assert!(html.contains("action=\"https://github.com/settings/apps/new?state=s-1\""));
        assert!(html.contains("name=\"manifest\""));
        // The JSON's own quotes are escaped, so the raw form never appears.
        assert!(
            !html.contains(r#""name":"think-and-ship""#),
            "raw JSON in the attribute would end the value at its first quote"
        );
        assert!(html.contains("&quot;organization_projects&quot;:&quot;write&quot;"));
        // Without scripting there is still a way forward.
        assert!(html.contains("<noscript>"));
        assert!(html.contains("Create GitHub App"));
    }

    #[test]
    fn attribute_escaping_covers_the_characters_that_break_out() {
        assert_eq!(
            escape_attr(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    /// A code that never arrived is refused before a request is made — an empty
    /// path segment would otherwise POST to a URL that means something else.
    #[tokio::test]
    async fn an_empty_code_is_refused_without_a_request() {
        let err = convert_manifest_at(&reqwest::Client::new(), "http://127.0.0.1:1", "  ")
            .await
            .expect_err("must refuse");
        assert!(err.to_string().contains("nothing to convert"), "{err}");
    }

    /// The code is single-shot and is the whole credential set. If GitHub echoes
    /// it into an error body, it must not reach a terminal.
    #[test]
    fn the_code_is_scrubbed_out_of_an_echoed_error_body() {
        let body = r#"{"message":"Not Found","code":"abc123"}"#;
        let scrubbed = redact_code(body, "abc123");
        assert!(!scrubbed.contains("abc123"), "{scrubbed}");
        assert!(scrubbed.contains(REDACTED), "{scrubbed}");
        // The rest of the body survives, because the manifest validation error
        // is the one thing that makes a rejection diagnosable.
        assert!(scrubbed.contains("Not Found"), "{scrubbed}");
    }

    /// The negative-space check on the outcome: the manifest is the request, and
    /// what GitHub records is a separate fact.
    #[test]
    fn a_permission_we_did_not_request_is_reported() {
        let mut app = app_fixture();
        assert!(app.unrequested_permissions().is_empty());
        assert!(!app.report().contains("WARNING"));

        app.permissions.insert("contents".into(), "write".into());
        assert_eq!(app.unrequested_permissions(), vec!["contents=write"]);
        assert!(app.report().contains("WARNING"), "{}", app.report());
        assert!(app.report().contains("contents=write"));
    }

    /// The report is the only human-facing rendering, so it is the one that must
    /// be provably clean.
    #[test]
    fn neither_the_report_nor_the_debug_rendering_carries_a_secret() {
        let app = app_fixture();
        for rendered in [app.report(), format!("{app:?}")] {
            for secret in ["PEM-PRIVATE-KEY", "CLIENT-SECRET-VALUE", "WEBHOOK-SECRET"] {
                assert!(
                    !rendered.contains(secret),
                    "'{secret}' leaked into: {rendered}"
                );
            }
            // Not vacuous: the rendering is real output about this app.
            assert!(rendered.contains("t-and-s"), "{rendered}");
        }
        // And the install step is named, because the flow is not done without it.
        assert!(app.report().contains("installations/new"));
    }

    fn app_fixture() -> RegisteredApp {
        RegisteredApp {
            id: 42,
            slug: "t-and-s".into(),
            node_id: "MDM6".into(),
            name: "think-and-ship".into(),
            html_url: "https://github.com/apps/t-and-s".into(),
            client_id: "Iv1.public".into(),
            client_secret: Secret::new("CLIENT-SECRET-VALUE"),
            pem: Secret::new("PEM-PRIVATE-KEY"),
            webhook_secret: Some(Secret::new("WEBHOOK-SECRET")),
            permissions: DEFAULT_PERMISSIONS
                .iter()
                .map(|p| (p.key.to_string(), p.level.to_string()))
                .collect(),
            events: vec!["issues".into()],
        }
    }
}
