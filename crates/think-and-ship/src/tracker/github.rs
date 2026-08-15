//! GitHub Issues, as a [`TrackerPort`] adapter.
//!
//! First adapter because we already own both ends — the Worker, the inbound
//! webhook, the HMAC path — so building it first tests the *projector* rather
//! than an unfamiliar API.
//!
//! # No live network in this adapter
//!
//! There is deliberately no credential here and no way to supply one. Credential
//! custody is its own subsystem: tokens need encryption at rest, rotation, and a
//! guarantee they never reach the roadmap store or a git-mirrored partition, and
//! smuggling an environment variable in now would create exactly the call site
//! that work has to unpick. The adapter therefore takes an `api_base` and is
//! exercised against an in-process mock server. Every request shape below is
//! real; only the destination is a test double.
//!
//! # The two-identifier problem
//!
//! GitHub names an issue two different ways and both are needed. Paths use the
//! per-repository **number** (`/issues/12`). The dependency endpoint takes the
//! global **database id** (`{"issue_id": 2847…}`), which is a different integer
//! entirely. Neither can be derived from the other.
//!
//! `external_id` is `owner/repo#number`, because that is the form a human can
//! read in a link record and the form every path needs. Database ids are learned
//! from the responses this adapter already receives and cached for the run; a
//! dependency on an issue this run did not touch costs one extra `GET`, charged
//! to the REST budget like any other call. The alternative — storing both ids in
//! the link record — would push a GitHub-shaped field into a provider-agnostic
//! type, which is the coupling the whole seam exists to avoid.

use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use crate::infra::ExternalId;
use crate::tracker::budget::{RateBudget, Spend, Transport};
use crate::tracker::domain::{TrackerCapabilities, WorkItem, WorkItemState};
use crate::tracker::port::{TrackerError, TrackerPort, UpsertOutcome};
use crate::tracker::registry::{ProviderBuild, ProviderRegistration};

/// The provider key, in ONE place in this file.
///
/// It is both the string a human types into `--provider` and the answer this
/// adapter gives to [`TrackerPort::provider`]. Those two were separate literals
/// once; binding them here makes the drift between them unrepresentable within
/// this file, and the truth gate in `tests/tracker_provider_registry.rs` covers
/// the failure that remains possible — a registration block copied into a
/// sibling adapter and only half-renamed.
pub const PROVIDER: &str = "github";

/// This adapter's entry in the one registry, declared in the adapter's own
/// file. See [`crate::tracker::registry`] for why the composing table is an
/// explicit `const` rather than a distributed slice.
pub const REGISTRATION: ProviderRegistration = ProviderRegistration {
    key: PROVIDER,
    build: build_registered,
};

fn build_registered(request: &ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError> {
    let mut tracker = GithubTracker::new(request.target)?.with_trace_context(request.project);
    if let Some(credential) = request.credential {
        tracker = tracker.with_credential(credential);
    }
    Ok(Box::new(tracker))
}

/// The public GitHub REST base. Overridden in tests with a mock server's URI.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Media type and API version GitHub asks integrations to pin.
const ACCEPT: &str = "application/vnd.github+json";
const API_VERSION: &str = "2022-11-28";

/// GitHub Issues adapter.
pub struct GithubTracker {
    api_base: String,
    owner: String,
    repo: String,
    /// The complete Authorization header value, prefix included.
    authorization: Option<String>,
    http: reqwest::Client,
    budget: Mutex<RateBudget>,
    /// `number -> database id`, learned from responses. Saves the extra GET on
    /// every issue this run already touched.
    ids: Mutex<std::collections::HashMap<u64, u64>>,
    /// The W3C `traceparent` to send downstream, resolved ONCE at construction.
    /// `None` — the default — means no caller context, so no header.
    traceparent: Option<String>,
}

impl GithubTracker {
    /// Build an adapter for `owner/repo`.
    ///
    /// `target` is the opaque destination string the per-project config stores;
    /// GitHub parses it as `owner/repo`. Returns an error rather than guessing,
    /// because a malformed target would otherwise surface as a 404 from a URL
    /// nobody can read.
    pub fn new(target: &str) -> Result<Self, TrackerError> {
        let (owner, repo) = target.trim().split_once('/').ok_or_else(|| {
            TrackerError::Unsupported(format!(
                "'{target}' is not a GitHub repository — expected the form owner/repo"
            ))
        })?;
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return Err(TrackerError::Unsupported(format!(
                "'{target}' is not a GitHub repository — expected the form owner/repo"
            )));
        }
        Ok(Self {
            api_base: DEFAULT_API_BASE.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            authorization: None,
            http: reqwest::Client::new(),
            budget: Mutex::new(RateBudget::github()),
            ids: Mutex::new(std::collections::HashMap::new()),
            traceparent: None,
        })
    }

    /// Adopt the project's caller trace context, so every request this adapter
    /// makes is a child of our exported workspace span (SEP-414 downstream
    /// half). Resolved once here rather than per call: the context is
    /// last-writer-wins per project, and re-reading it mid-push would let a
    /// concurrent tool call move the parent of a run already in flight.
    #[must_use]
    pub fn with_trace_context(self, project: &str) -> Self {
        self.with_traceparent(crate::trace_context::outbound_traceparent(project))
    }

    /// Set the downstream `traceparent` directly. The seam
    /// [`Self::with_trace_context`] resolves onto, exposed because a caller
    /// embedding this crate may already hold a context and not want our file
    /// store — and because a test can reach it without mutating
    /// process-global environment.
    #[must_use]
    pub fn with_traceparent(mut self, traceparent: Option<String>) -> Self {
        self.traceparent = traceparent;
        self
    }

    /// Point the adapter at a different API root — a mock server in tests, or
    /// GitHub Enterprise Server later.
    #[must_use]
    pub fn with_api_base(mut self, base: &str) -> Self {
        self.api_base = base.trim_end_matches('/').to_string();
        self
    }

    /// Supply a bearer token directly. Retained for tests and the documented
    /// env-var fallback; the product path is [`Self::with_credential`].
    #[must_use]
    pub fn with_token(mut self, token: &str) -> Self {
        self.authorization = Some(format!("Bearer {token}"));
        self
    }

    /// Authenticate from the credential port.
    ///
    /// THE single wiring point for this provider — credential custody resolves
    /// the secret and its scheme, and the adapter never learns whether it came
    /// from an App installation, an OAuth flow or a pasted PAT.
    #[must_use]
    pub fn with_credential(mut self, credential: &crate::tracker::credential::Credential) -> Self {
        self.authorization = Some(credential.header_value());
        self
    }

    fn lock_budget(&self) -> std::sync::MutexGuard<'_, RateBudget> {
        self.budget.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Charge the REST bucket before spending it. Refusal surfaces as
    /// `RateLimited`, which `retryable()` already classifies as replayable —
    /// so an exhausted budget queues rather than being lost.
    fn charge_rest(&self, points: u32) -> Result<(), TrackerError> {
        match self.lock_budget().spend("github", Transport::Rest, points) {
            Spend::Ok => Ok(()),
            Spend::Exhausted { retry_after } => Err(TrackerError::RateLimited {
                retry_after_secs: Some(retry_after.as_secs()),
            }),
        }
    }

    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .request(method, url)
            .header("Accept", ACCEPT)
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", "think-and-ship");
        if let Some(auth) = &self.authorization {
            req = req.header("Authorization", auth.clone());
        }
        // SEP-414's downstream half. `None` — no caller context adopted — adds
        // no header at all, which is the point: an unparented call must not
        // acquire a fabricated root just because it went over HTTP.
        if let Some(tp) = &self.traceparent {
            req = req.header("traceparent", tp.clone());
        }
        req
    }

    /// Map a response into either its JSON body or the right [`TrackerError`],
    /// so `retryable()` — and therefore the outbox — behaves correctly without
    /// any call site re-deciding what a status means.
    async fn read(&self, resp: reqwest::Response) -> Result<serde_json::Value, TrackerError> {
        let status = resp.status();
        if status.is_success() {
            return resp
                .json()
                .await
                .map_err(|e| TrackerError::Transport(e.to_string()));
        }
        // A secondary rate limit arrives as 403 or 429, and GitHub tells us how
        // long to wait. Preserving that beats guessing a backoff.
        if status.as_u16() == 429 || status.as_u16() == 403 {
            let retry_after_secs = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let is_rate_limit = retry_after_secs.is_some()
                || resp
                    .headers()
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v == "0");
            if is_rate_limit {
                return Err(TrackerError::RateLimited { retry_after_secs });
            }
        }
        if status.as_u16() == 404 {
            return Err(TrackerError::NotFound(format!(
                "{}/{}",
                self.owner, self.repo
            )));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(TrackerError::Status {
            status: status.as_u16(),
            body,
        })
    }

    /// `owner/repo#number` — readable in a link record, and the only form every
    /// path needs.
    fn external_id_for(&self, number: u64) -> ExternalId {
        format!("{}/{}#{number}", self.owner, self.repo)
    }

    /// Pull the issue number back out of an `external_id` we minted.
    fn number_of(&self, external_id: &str) -> Result<u64, TrackerError> {
        external_id
            .rsplit_once('#')
            .and_then(|(_, n)| n.parse::<u64>().ok())
            .ok_or_else(|| {
                TrackerError::NotFound(format!(
                    "'{external_id}' is not a GitHub issue reference (expected owner/repo#number)"
                ))
            })
    }

    fn remember(&self, number: u64, database_id: u64) {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(number, database_id);
    }

    /// The database id for an issue number, from cache or one cheap GET.
    async fn database_id(&self, number: u64) -> Result<u64, TrackerError> {
        if let Some(id) = self
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&number)
        {
            return Ok(*id);
        }
        self.charge_rest(1)?;
        let url = format!(
            "{}/repos/{}/{}/issues/{number}",
            self.api_base, self.owner, self.repo
        );
        let resp = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        let body = self.read(resp).await?;
        let id = body
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| TrackerError::Status {
                status: 200,
                body: "issue response carried no id".to_string(),
            })?;
        self.remember(number, id);
        Ok(id)
    }
}

/// GitHub's issue state vocabulary, plus the `state_reason` that distinguishes
/// "done" from "cancelled" — without it, an obsoleted chunk and a finished one
/// look identical upstream.
fn state_fields(state: WorkItemState) -> (&'static str, Option<&'static str>) {
    match state {
        WorkItemState::Todo | WorkItemState::InProgress => ("open", None),
        WorkItemState::Done => ("closed", Some("completed")),
        WorkItemState::Cancelled => ("closed", Some("not_planned")),
    }
}

fn state_from(state: &str, reason: Option<&str>) -> WorkItemState {
    match (state, reason) {
        ("closed", Some("not_planned")) => WorkItemState::Cancelled,
        ("closed", _) => WorkItemState::Done,
        _ => WorkItemState::Todo,
    }
}

#[async_trait]
impl TrackerPort for GithubTracker {
    fn provider(&self) -> &str {
        PROVIDER
    }

    /// Note the limit that is NOT expressed here: issue dependencies exist on
    /// github.com only, not on GitHub Enterprise Server (cli/cli#11757). This
    /// adapter targets github.com, so it declares them supported; a GHES variant
    /// must set `blocking_links: false` and take the prose fallback rather than
    /// 404 on every relation. `capabilities()` is the seam that makes that a
    /// one-field difference instead of a fork.
    fn capabilities(&self) -> TrackerCapabilities {
        TrackerCapabilities {
            blocking_links: true,
            labels: true,
            assignee: true,
            // GitHub rejects an issue body over 65,536 characters.
            max_body_len: Some(65_536),
            required_fields: Vec::new(),
        }
    }

    async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
        let (state, state_reason) = state_fields(item.state);
        let mut payload = json!({
            "title": item.title,
            "body": item.body,
            "labels": item.labels,
        });
        if let Some(a) = &item.assignee {
            payload["assignees"] = json!([a]);
        }

        let (method, url) = match &item.external_id {
            // Identity decides create-vs-patch, never the title.
            Some(id) => {
                let number = self.number_of(id)?;
                // State only travels on a patch: a create is always `open`, and
                // sending `state` to the create endpoint is rejected.
                payload["state"] = json!(state);
                if let Some(r) = state_reason {
                    payload["state_reason"] = json!(r);
                }
                (
                    reqwest::Method::PATCH,
                    format!(
                        "{}/repos/{}/{}/issues/{number}",
                        self.api_base, self.owner, self.repo
                    ),
                )
            }
            None => (
                reqwest::Method::POST,
                format!(
                    "{}/repos/{}/{}/issues",
                    self.api_base, self.owner, self.repo
                ),
            ),
        };

        self.charge_rest(1)?;
        let created = method == reqwest::Method::POST;
        let resp = self
            .request(method, url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        let body = self.read(resp).await?;

        let number = body
            .get("number")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| TrackerError::Status {
                status: 200,
                body: "issue response carried no number".to_string(),
            })?;
        if let Some(id) = body.get("id").and_then(serde_json::Value::as_u64) {
            self.remember(number, id);
        }

        Ok(UpsertOutcome {
            external_id: self.external_id_for(number),
            // `updated_at` is GitHub's only concurrency token for an issue.
            version: body
                .get("updated_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            created,
        })
    }

    async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError> {
        self.charge_rest(1)?;
        let url = format!(
            "{}/repos/{}/{}/issues",
            self.api_base, self.owner, self.repo
        );
        let resp = self
            .request(reqwest::Method::GET, url)
            .query(&[("since", since), ("state", "all"), ("per_page", "100")])
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        let body = self.read(resp).await?;

        let Some(rows) = body.as_array() else {
            return Ok(Vec::new());
        };
        Ok(rows
            .iter()
            // Every pull request is an issue on this endpoint, but not every
            // issue is a pull request. Projected work items are never PRs.
            .filter(|row| row.get("pull_request").is_none())
            .filter_map(|row| {
                let number = row.get("number")?.as_u64()?;
                if let Some(id) = row.get("id").and_then(serde_json::Value::as_u64) {
                    self.remember(number, id);
                }
                Some(WorkItem {
                    // Reading an item back does not resolve its container: the
                    // projector authors grouping, it never learns it from upstream.
                    group: None,
                    external_id: Some(self.external_id_for(number)),
                    title: row.get("title")?.as_str().unwrap_or_default().to_string(),
                    body: row
                        .get("body")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    state: state_from(
                        row.get("state").and_then(serde_json::Value::as_str)?,
                        row.get("state_reason").and_then(serde_json::Value::as_str),
                    ),
                    labels: row
                        .get("labels")
                        .and_then(serde_json::Value::as_array)
                        .map(|ls| {
                            ls.iter()
                                .filter_map(|l| {
                                    l.get("name")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    assignee: row
                        .get("assignee")
                        .and_then(|a| a.get("login"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    version: row
                        .get("updated_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect())
    }

    async fn relate_items(
        &self,
        from: &ExternalId,
        blocked_by: &[ExternalId],
    ) -> Result<(), TrackerError> {
        let number = self.number_of(from)?;

        // Converge, don't append: the port's contract is that `blocked_by` is
        // the full set. Read what is declared, then add and remove the
        // difference — re-POSTing an existing dependency is a 422.
        self.charge_rest(1)?;
        let url = format!(
            "{}/repos/{}/{}/issues/{number}/dependencies/blocked_by",
            self.api_base, self.owner, self.repo
        );
        let resp = self
            .request(reqwest::Method::GET, url.clone())
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        let existing = self.read(resp).await?;
        let existing_ids: Vec<u64> = existing
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| r.get("id").and_then(serde_json::Value::as_u64))
                    .collect()
            })
            .unwrap_or_default();

        let mut wanted = Vec::new();
        for dep in blocked_by {
            wanted.push(self.database_id(self.number_of(dep)?).await?);
        }

        for id in wanted.iter().filter(|id| !existing_ids.contains(id)) {
            self.charge_rest(1)?;
            let resp = self
                .request(reqwest::Method::POST, url.clone())
                .json(&json!({ "issue_id": id }))
                .send()
                .await
                .map_err(|e| TrackerError::Transport(e.to_string()))?;
            self.read(resp).await?;
        }
        for id in existing_ids.iter().filter(|id| !wanted.contains(id)) {
            self.charge_rest(1)?;
            let resp = self
                .request(reqwest::Method::DELETE, format!("{url}/{id}"))
                .send()
                .await
                .map_err(|e| TrackerError::Transport(e.to_string()))?;
            self.read(resp).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_target_is_refused_rather_than_guessed() {
        for bad in ["", "owner", "owner/", "/repo", "owner/repo/extra"] {
            assert!(
                GithubTracker::new(bad).is_err(),
                "'{bad}' must not parse as a repository"
            );
        }
        let ok = GithubTracker::new("owner/repo").expect("valid");
        assert_eq!(ok.provider(), "github");
    }

    /// The two-identifier trap, pinned: the id in the path is NOT the id in the
    /// dependency body.
    #[test]
    fn external_ids_round_trip_through_the_issue_number() {
        let gh = GithubTracker::new("owner/repo").expect("valid");
        let id = gh.external_id_for(12);
        assert_eq!(id, "owner/repo#12");
        assert_eq!(gh.number_of(&id).expect("parses"), 12);
        assert!(gh.number_of("owner/repo").is_err());
    }

    /// An obsoleted chunk and a finished one must not look identical upstream.
    #[test]
    fn cancelled_and_done_are_distinguishable_states() {
        assert_eq!(
            state_fields(WorkItemState::Done),
            ("closed", Some("completed"))
        );
        assert_eq!(
            state_fields(WorkItemState::Cancelled),
            ("closed", Some("not_planned"))
        );
        assert_eq!(
            state_from("closed", Some("not_planned")),
            WorkItemState::Cancelled
        );
        assert_eq!(state_from("closed", None), WorkItemState::Done);
        assert_eq!(state_from("open", None), WorkItemState::Todo);
    }

    #[test]
    fn capabilities_declare_what_github_actually_supports() {
        let caps = GithubTracker::new("owner/repo")
            .expect("valid")
            .capabilities();
        assert!(caps.blocking_links, "issue dependencies went GA in 2025");
        assert_eq!(caps.max_body_len, Some(65_536));
        assert!(caps.required_fields.is_empty());
    }
}
