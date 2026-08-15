//! The cloudid hop: turning a 3LO token into somewhere to send a request.
//!
//! # Why this exists at all
//!
//! Every other provider in this system hands back a token you can immediately
//! use against a fixed base URL. Atlassian does not. A 3LO token is granted to
//! an ACCOUNT, not to a site, so it says nothing about *where* to call. The
//! address is assembled from a second fact — a `cloudid` — that only
//! `GET /oauth/token/accessible-resources` can tell you, and calls then go to
//! `https://api.atlassian.com/ex/jira/{cloudid}/…` rather than to the
//! `something.atlassian.net` domain the human recognises.
//!
//! # The choice is a human's, and it is made exactly once
//!
//! An account can have several Jira sites. Picking the first one is the
//! tempting implementation and the wrong one: it silently writes issues into
//! whichever site the API happened to list first, which looks like nothing
//! until someone finds their tickets in the wrong place. So [`select_site`] is
//! **pure and total** — zero sites and many sites are both errors that say what
//! to do, and only an unambiguous result is returned.
//!
//! The answer is then stored on the credential record (see
//! [`StoredCredential::site`]) rather than re-resolved per call, because the
//! moment of consent is the only moment a human is present to choose.
//!
//! [`StoredCredential::site`]: super::domain::StoredCredential::site

use serde::Deserialize;

use super::store::CredentialError;

/// Where Atlassian's account-level (not site-level) endpoints live.
const ATLASSIAN_API: &str = "https://api.atlassian.com";

/// One site the consenting account granted access to.
///
/// Named after what Atlassian returns rather than after "cloudid": `id` IS the
/// cloudid, and the human recognises the `url`, so both are kept — an error
/// that lists only opaque uuids is not an actionable error.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AccessibleResource {
    /// The cloudid. This is the value that goes in the API path.
    pub id: String,
    /// The site's display name, e.g. `my-team`.
    #[serde(default)]
    pub name: String,
    /// The site's human-facing URL, e.g. `https://my-team.atlassian.net`.
    #[serde(default)]
    pub url: String,
    /// The scopes actually granted on this site. Not all sites in a response
    /// necessarily carry the same ones.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl AccessibleResource {
    /// How this site should be named back to a human choosing between them.
    #[must_use]
    pub fn describe(&self) -> String {
        match (self.name.is_empty(), self.url.is_empty()) {
            (false, false) => format!("{} ({})", self.name, self.url),
            (false, true) => self.name.clone(),
            (true, false) => self.url.clone(),
            (true, true) => self.id.clone(),
        }
    }
}

/// The API base for a Jira Cloud REST call under a 3LO token.
///
/// Deliberately NOT the site's own domain: a 3LO token is rejected there. This
/// is the one place the `/ex/jira/{cloudid}` shape is written down.
#[must_use]
pub fn jira_api_base(cloudid: &str) -> String {
    format!("{ATLASSIAN_API}/ex/jira/{cloudid}")
}

/// Ask Atlassian which sites this token can reach.
pub async fn accessible_resources(
    http: &reqwest::Client,
    access_token: &str,
) -> Result<Vec<AccessibleResource>, CredentialError> {
    accessible_resources_at(http, ATLASSIAN_API, access_token).await
}

/// The same call against an explicit base, so a test can point it at a mock
/// without the production path growing a configurable endpoint.
pub async fn accessible_resources_at(
    http: &reqwest::Client,
    api_base: &str,
    access_token: &str,
) -> Result<Vec<AccessibleResource>, CredentialError> {
    let resp = http
        .get(format!(
            "{}/oauth/token/accessible-resources",
            api_base.trim_end_matches('/')
        ))
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            CredentialError::Invalid(format!("asking Atlassian which sites you granted: {e}"))
        })?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.text().await.unwrap_or_default();
        // A 401 here means the token is fine but the grant is not — worth
        // saying, because the obvious reading is "my token is broken".
        return Err(CredentialError::Invalid(format!(
            "Atlassian refused the accessible-resources lookup with {status}: {body} — the \
             token may lack the scopes the app asked for, or consent may have been revoked"
        )));
    }

    resp.json::<Vec<AccessibleResource>>()
        .await
        .map_err(|e| CredentialError::Invalid(format!("accessible-resources unreadable: {e}")))
}

/// Choose exactly one site, or explain why that is not possible.
///
/// PURE and TOTAL. `wanted` is an optional human selector matching a cloudid, a
/// url, or a name — case-insensitively for the last two, because nobody types a
/// site name with the capitalisation the API returns.
///
/// The many-sites branch is the reason this function is not a `.first()` call.
pub fn select_site(
    sites: &[AccessibleResource],
    wanted: Option<&str>,
) -> Result<AccessibleResource, CredentialError> {
    if let Some(wanted) = wanted.map(str::trim).filter(|w| !w.is_empty()) {
        let needle = wanted.to_ascii_lowercase();
        // Trailing slashes are trimmed on BOTH sides. Trimming only the stored
        // url was the first version, and it failed on the one input a human
        // actually produces: a URL pasted out of a browser's address bar.
        let needle_url = needle.trim_end_matches('/');
        let matched: Vec<&AccessibleResource> = sites
            .iter()
            .filter(|s| {
                s.id == wanted
                    || s.name.to_ascii_lowercase() == needle
                    || s.url.to_ascii_lowercase().trim_end_matches('/') == needle_url
            })
            .collect();
        return match matched.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(CredentialError::Invalid(format!(
                "no Atlassian site matching '{wanted}' was granted to this app.{}",
                offer(sites)
            ))),
            many => Err(CredentialError::Invalid(format!(
                "'{wanted}' matches {} of your Atlassian sites, so it cannot identify one — \
                 use the site id instead.{}",
                many.len(),
                offer(sites)
            ))),
        };
    }

    match sites {
        [one] => Ok(one.clone()),
        [] => Err(CredentialError::Invalid(
            "the Atlassian account that approved this granted no sites, so there is nowhere \
             to send a request. Check that you selected a site on the consent screen, and \
             that the app requests scopes the site actually offers."
                .into(),
        )),
        many => Err(CredentialError::Invalid(format!(
            "this Atlassian account granted {} sites and picking one for you would silently \
             write into the wrong place — name the one you mean.{}",
            many.len(),
            offer(sites)
        ))),
    }
}

/// The list a human needs to answer the question the error just asked. Empty
/// when there is nothing to offer, so a zero-site error does not trail a
/// pointless heading.
fn offer(sites: &[AccessibleResource]) -> String {
    if sites.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n  available:");
    for site in sites {
        out.push_str(&format!("\n    {}  id={}", site.describe(), site.id));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, name: &str) -> AccessibleResource {
        AccessibleResource {
            id: id.into(),
            name: name.into(),
            url: format!("https://{name}.atlassian.net"),
            scopes: vec!["write:jira-work".into()],
        }
    }

    #[test]
    fn the_api_base_is_the_ex_jira_form_not_the_site_domain() {
        let base = jira_api_base("cloud-1");
        assert_eq!(base, "https://api.atlassian.com/ex/jira/cloud-1");
        assert!(
            !base.contains("atlassian.net"),
            "a 3LO token is rejected at the site's own domain"
        );
    }

    #[test]
    fn exactly_one_site_needs_no_choice() {
        let chosen = select_site(&[site("c-1", "acme")], None).expect("one site");
        assert_eq!(chosen.id, "c-1");
    }

    /// ZERO. Not a panic, not an empty Ok — an error that names the cause.
    #[test]
    fn zero_sites_is_an_actionable_error() {
        let err = select_site(&[], None).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("granted no sites"), "got: {msg}");
        assert!(msg.contains("consent screen"), "got: {msg}");
        // Nothing to list, so no dangling heading.
        assert!(!msg.contains("available:"), "got: {msg}");
    }

    /// MANY. The assertion that would catch a silent first-match: the result
    /// must be an error, and it must NAME every candidate.
    #[test]
    fn many_sites_refuses_to_pick_and_lists_them_all() {
        let sites = [site("c-1", "acme"), site("c-2", "beta")];
        let err = select_site(&sites, None).expect_err("must refuse to guess");
        let msg = err.to_string();

        assert!(msg.contains("granted 2 sites"), "got: {msg}");
        assert!(msg.contains("wrong place"), "got: {msg}");
        for expected in ["acme", "beta", "c-1", "c-2"] {
            assert!(msg.contains(expected), "'{expected}' missing from: {msg}");
        }
    }

    #[test]
    fn a_selector_matches_a_cloudid_a_name_or_a_url_case_insensitively() {
        let sites = [site("c-1", "acme"), site("c-2", "beta")];

        assert_eq!(select_site(&sites, Some("c-2")).expect("by id").id, "c-2");
        assert_eq!(
            select_site(&sites, Some("ACME")).expect("by name").id,
            "c-1"
        );
        assert_eq!(
            select_site(&sites, Some("https://Beta.atlassian.net"))
                .expect("by url")
                .id,
            "c-2"
        );
        // A trailing slash is what a human pastes out of a browser.
        assert_eq!(
            select_site(&sites, Some("https://beta.atlassian.net/"))
                .expect("by url with slash")
                .id,
            "c-2"
        );
        // Whitespace and an empty selector fall back to the unambiguous rule
        // rather than matching nothing.
        assert!(select_site(&sites, Some("   ")).is_err());
    }

    #[test]
    fn an_unmatched_selector_says_what_was_available() {
        let sites = [site("c-1", "acme")];
        let err = select_site(&sites, Some("gamma")).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("no Atlassian site matching 'gamma'"), "{msg}");
        assert!(msg.contains("acme"), "{msg}");
    }

    /// Two sites sharing a name cannot be disambiguated by it, and saying so
    /// beats returning whichever came first.
    #[test]
    fn an_ambiguous_selector_is_refused_rather_than_resolved_by_order() {
        let mut second = site("c-2", "acme");
        second.url = "https://acme-eu.atlassian.net".into();
        let sites = [site("c-1", "acme"), second];

        let err = select_site(&sites, Some("acme")).expect_err("must refuse");
        assert!(err.to_string().contains("matches 2"), "{err}");
    }

    #[test]
    fn a_site_describes_itself_by_whatever_it_actually_has() {
        assert_eq!(
            site("c-1", "acme").describe(),
            "acme (https://acme.atlassian.net)"
        );

        let bare = AccessibleResource {
            id: "c-9".into(),
            name: String::new(),
            url: String::new(),
            scopes: vec![],
        };
        assert_eq!(bare.describe(), "c-9", "an id beats an empty string");
    }
}
