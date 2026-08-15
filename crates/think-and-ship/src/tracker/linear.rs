//! Linear, as a [`TrackerPort`] adapter — and the falsification test for the
//! seam.
//!
//! GitHub was the first adapter, but we own both ends of it, so it could not
//! tell us whether the seam was real or merely shaped around one API we already
//! understood. Linear is the second provider and it was chosen because it
//! disagrees with GitHub in three structural ways at once:
//!
//! 1. **GraphQL, not REST.** One endpoint, one verb, errors in the body.
//! 2. **Team-scoped workflow states.** There is no global "In Progress".
//! 3. **An inverted relation model.** See [`LinearTracker::relate_items`].
//!
//! If a port survives all three without changing, it is a port. If it does not,
//! the right time to learn that is now, before more layers are stacked on it.
//!
//! # No live network in this adapter either
//!
//! There is no credential path here beyond a token seam, for the same reason as
//! the GitHub adapter: credential custody is its own subsystem. Every request
//! shape below is real; only the destination is a mock.
//!
//! # Two things a GitHub-shaped adapter would get wrong
//!
//! **Errors arrive with HTTP 200.** A GraphQL server reports a failed mutation
//! in an `errors` array with a perfectly successful status line. Checking
//! `status.is_success()` — which is exactly what the REST adapter does, and
//! correctly — would read a rejected write as a success, and the projector would
//! then record a tracker link for an issue that does not exist. Every response
//! here is inspected for `errors` BEFORE its `data` is trusted.
//!
//! **Priority runs backwards, and is not monotone.** Linear's urgency is
//! `0 = none, 1 = urgent, 2 = high, 3 = medium, 4 = low`; our bands run
//! low-number-is-more-important. So the mapping reverses — and `0` is a hole in
//! the middle of the ordering rather than the top of it, because it means "no
//! priority" rather than "most urgent". Anything that assumes monotonicity here
//! silently makes the least important work look the most urgent.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::infra::ExternalId;
use crate::tracker::budget::{RateBudget, Spend, Transport};
use crate::tracker::domain::{TrackerCapabilities, WorkItem, WorkItemState};
use crate::tracker::port::{TargetInfo, TrackerError, TrackerPort, UpsertOutcome};
use crate::tracker::registry::{ProviderBuild, ProviderRegistration};

/// The provider key, in ONE place in this file. See
/// [`crate::tracker::github::PROVIDER`] for why it is bound rather than typed
/// twice.
pub const PROVIDER: &str = "linear";

/// This adapter's entry in the one registry, declared in the adapter's own file.
pub const REGISTRATION: ProviderRegistration = ProviderRegistration {
    key: PROVIDER,
    build: build_registered,
};

fn build_registered(request: &ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError> {
    let mut tracker = LinearTracker::new(request.target)?.with_trace_context(request.project);
    if let Some(credential) = request.credential {
        tracker = tracker.with_credential(credential);
    }
    Ok(Box::new(tracker))
}

/// Linear's single GraphQL endpoint. Overridden in tests with a mock server.
pub const DEFAULT_API_BASE: &str = "https://api.linear.app";

/// Linear's documented complexity budget for an API key: 3,000,000 points per
/// hour. Declared here rather than in `budget.rs` — an adapter naming its own
/// limits is what keeps the provider list out of the core.
const COMPLEXITY_PER_HOUR: u32 = 3_000_000;
const WINDOW_SECS: u64 = 3_600;

/// The scheme this adapter needs is now the SHARED one.
///
/// This enum was born here, because Linear is the provider that proved a token
/// and its presentation cannot be separated. Credential custody generalized it,
/// so the definition moved rather than being duplicated — two enums meaning the
/// same thing is how they drift.
pub use crate::tracker::credential::AuthScheme;

/// Linear's workflow-state categories. These are the stable thing; the NAMES
/// attached to them are per-team and must never be matched on.
const TYPE_TRIAGE: &str = "triage";
const TYPE_BACKLOG: &str = "backlog";
const TYPE_UNSTARTED: &str = "unstarted";
const TYPE_STARTED: &str = "started";
const TYPE_COMPLETED: &str = "completed";
const TYPE_CANCELED: &str = "canceled";

/// One team's discovered workflow, resolved once per run.
#[derive(Debug, Clone, Default)]
struct TeamSchema {
    team_id: String,
    /// `state type -> state id`. Names are deliberately not retained: keeping
    /// them would invite matching on them.
    states_by_type: HashMap<String, String>,
    /// `label name -> label id`. Names ARE retained here, unlike states,
    /// because a label has no type to match on — the name IS the identity, and
    /// `roadmap:<band>` is a name we author rather than one a team invented.
    labels_by_name: HashMap<String, String>,
}

/// The Linear adapter.
pub struct LinearTracker {
    api_base: String,
    /// The team key a human types (`ENG`), not a UUID.
    team_key: String,
    token: Option<String>,
    scheme: AuthScheme,
    http: reqwest::Client,
    budget: Mutex<RateBudget>,
    schema: Mutex<Option<TeamSchema>>,
    /// `identifier -> uuid`, learned from responses. Mutations and relations
    /// need the UUID; humans and link records want the identifier.
    ids: Mutex<HashMap<String, String>>,
    /// `project name -> uuid`, resolved once per run. Same reasoning as the
    /// team schema: the projector speaks names, the API needs ids, and looking
    /// one up per issue would spend the complexity budget on nothing.
    projects: Mutex<HashMap<String, String>>,
    /// The uuid of the roof `upsert_initiative` raised, held so every
    /// subsequent `upsert_group` can file its project under it. `None` means no
    /// initiative was asked for OR raising it failed — either way the groups
    /// degrade to standing on their own, which is the port's contract.
    initiative: Mutex<Option<String>>,
    /// The W3C `traceparent` to send downstream, resolved ONCE at construction.
    /// `None` — the default — means no caller context, so no header.
    traceparent: Option<String>,
}

impl LinearTracker {
    /// Build an adapter for one Linear team.
    ///
    /// `target` is the team KEY as it appears in issue identifiers — `ENG` for
    /// `ENG-42`. A UUID would be unreadable in a link record and unguessable by
    /// the human configuring it.
    pub fn new(target: &str) -> Result<Self, TrackerError> {
        let key = target.trim().to_ascii_uppercase();
        // ALPHANUMERIC ONLY, and this is stricter than it looks on purpose.
        //
        // Linear does not reject a key containing punctuation — it SANITIZES it
        // and creates the team under a different key than the one you asked
        // for. `WOW-AI` becomes `WOW`. That is worse than a rejection: a caller
        // that then looks the team up by the key it requested is told the team
        // does not exist, having just created it. Found the hard way, on a real
        // workspace, by exactly that sequence.
        //
        // So refuse here, before any network call, where the only cost is a
        // clear message.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(TrackerError::Unsupported(format!(
                "'{target}' is not a Linear team key — keys are letters and digits \
                 only, like ENG or WOW. Linear silently rewrites anything else \
                 (it would turn '{target}' into something shorter), which leaves \
                 the plan pointing at a team that does not exist under that name."
            )));
        }
        let mut budget = RateBudget::new();
        // Declared through the public API, so adding a provider does not edit
        // the budget module.
        budget.configure(
            "linear",
            Transport::GraphQl,
            COMPLEXITY_PER_HOUR,
            WINDOW_SECS,
        );

        Ok(Self {
            api_base: DEFAULT_API_BASE.to_string(),
            team_key: key,
            token: None,
            scheme: AuthScheme::Raw,
            http: reqwest::Client::new(),
            budget: Mutex::new(budget),
            schema: Mutex::new(None),
            ids: Mutex::new(HashMap::new()),
            projects: Mutex::new(HashMap::new()),
            initiative: Mutex::new(None),
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

    /// Set the downstream `traceparent` directly. See
    /// [`GithubTracker::with_traceparent`](crate::tracker::GithubTracker::with_traceparent).
    #[must_use]
    pub fn with_traceparent(mut self, traceparent: Option<String>) -> Self {
        self.traceparent = traceparent;
        self
    }

    #[must_use]
    pub fn with_api_base(mut self, base: &str) -> Self {
        self.api_base = base.trim_end_matches('/').to_string();
        self
    }

    /// Supply a token and its scheme directly. Retained for tests and the
    /// documented env-var fallback; the product path is
    /// [`Self::with_credential`].
    #[must_use]
    pub fn with_token(mut self, token: &str, scheme: AuthScheme) -> Self {
        self.token = Some(token.to_string());
        self.scheme = scheme;
        self
    }

    /// Authenticate from the credential port.
    ///
    /// THE single wiring point for this provider. The credential carries the
    /// scheme, which is the whole reason this adapter could not have been wired
    /// with a bare string.
    #[must_use]
    pub fn with_credential(mut self, credential: &crate::tracker::credential::Credential) -> Self {
        self.token = Some(credential.secret().expose().to_string());
        self.scheme = credential.scheme();
        self
    }

    #[must_use]
    pub fn team_key(&self) -> &str {
        &self.team_key
    }

    fn endpoint(&self) -> String {
        format!("{}/graphql", self.api_base)
    }

    /// Charge the GraphQL bucket. Linear bills complexity points; every query
    /// here is small and shallow, so a flat estimate per call is honest enough
    /// to protect the budget without pretending to a precision we do not have.
    fn charge(&self, points: u32) -> Result<(), TrackerError> {
        let spent = self.budget.lock().unwrap_or_else(|e| e.into_inner()).spend(
            "linear",
            Transport::GraphQl,
            points,
        );
        match spent {
            Spend::Ok => Ok(()),
            Spend::Exhausted { retry_after } => Err(TrackerError::RateLimited {
                retry_after_secs: Some(retry_after.as_secs()),
            }),
        }
    }

    /// The ONE network primitive. A GraphQL API has a single endpoint and a
    /// single verb, so an adapter that grows a helper per operation is
    /// importing REST habits it does not need.
    async fn graphql(&self, query: &str, variables: Value) -> Result<Value, TrackerError> {
        self.charge(1)?;

        let mut req = self
            .http
            .post(self.endpoint())
            .header("Content-Type", "application/json");
        // SEP-414's downstream half. `None` — no caller context adopted — adds
        // no header at all, which is the point: an unparented call must not
        // acquire a fabricated root just because it went over HTTP.
        if let Some(tp) = &self.traceparent {
            req = req.header("traceparent", tp.clone());
        }
        if let Some(t) = &self.token {
            req = match self.scheme {
                AuthScheme::Raw => req.header("Authorization", t.clone()),
                AuthScheme::Bearer => req.bearer_auth(t),
            };
        }

        let resp = req
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        // Transport-level rate limiting still arrives as a status code.
        if status == 429 {
            let retry_after_secs = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            return Err(TrackerError::RateLimited { retry_after_secs });
        }
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(TrackerError::Status { status, body });
        }

        let envelope: Value = resp
            .json()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        Self::read_envelope(envelope)
    }

    /// Pull `data` out of a GraphQL envelope, or turn its `errors` into the
    /// right [`TrackerError`].
    ///
    /// Split out and pure so the classification is testable without a server —
    /// this is the piece a REST-shaped adapter gets wrong, so it deserves direct
    /// tests rather than only incidental coverage.
    fn read_envelope(envelope: Value) -> Result<Value, TrackerError> {
        if let Some(errors) = envelope.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            let code = errors
                .iter()
                .find_map(|e| e.pointer("/extensions/code").and_then(Value::as_str))
                .unwrap_or_default()
                .to_ascii_uppercase();
            let message = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");

            // Linear signals throttling in the error body, not only by status.
            // Misreading it as a contract error would drop the write instead of
            // queueing it.
            if code.contains("RATELIMIT") || code.contains("RATE_LIMIT") {
                return Err(TrackerError::RateLimited {
                    retry_after_secs: None,
                });
            }
            if code.contains("AUTHENTICATION") || code.contains("FORBIDDEN") {
                return Err(TrackerError::Status {
                    status: 401,
                    body: message,
                });
            }
            // Everything else is a contract rejection: the same query will fail
            // the same way forever, so it must NOT be queued for replay.
            return Err(TrackerError::Status {
                status: 400,
                body: message,
            });
        }

        envelope
            .get("data")
            .cloned()
            .ok_or_else(|| TrackerError::Transport("GraphQL response carried no data".into()))
    }

    /// The uuid of a project by NAME, or `None` when the team has no such
    /// project.
    ///
    /// Scoped to the configured team. A workspace may hold many projects and
    /// several could share a name across teams; resolving unscoped would attach
    /// our issues to a stranger's project that merely matches a string.
    async fn project_id_for(&self, name: &str) -> Result<Option<String>, TrackerError> {
        if let Ok(cache) = self.projects.lock()
            && let Some(id) = cache.get(name)
        {
            return Ok(Some(id.clone()));
        }
        let team_id = self.team_schema().await?.team_id;
        let data = self
            .graphql(
                "query TeamProjects($id: String!) { \
                   team(id: $id) { projects(first: 250) { nodes { id name } } } }",
                json!({ "id": team_id }),
            )
            .await?;
        let mut found = None;
        if let Some(nodes) = data
            .pointer("/team/projects/nodes")
            .and_then(Value::as_array)
            && let Ok(mut cache) = self.projects.lock()
        {
            for n in nodes {
                if let (Some(id), Some(nm)) = (
                    n.get("id").and_then(Value::as_str),
                    n.get("name").and_then(Value::as_str),
                ) {
                    cache.insert(nm.to_string(), id.to_string());
                }
            }
            found = cache.get(name).cloned();
        }
        Ok(found)
    }

    /// Our three-valued container state as Linear's project state string.
    ///
    /// Linear also has `paused` and `canceled`; both are absent here and that is
    /// deliberate. They record a human's intent to stop, which the roadmap does
    /// not know, so mapping something onto them would overwrite a decision with
    /// a guess. A project a human paused keeps its state until a chunk moves.
    fn project_state(state: crate::tracker::domain::GroupState) -> &'static str {
        use crate::tracker::domain::GroupState;
        match state {
            GroupState::NotStarted => "planned",
            GroupState::Active => "started",
            GroupState::Complete => "completed",
        }
    }

    /// Our three-valued state as Linear's INITIATIVE status — a third
    /// vocabulary, not the project one: `InitiativeStatus` is a closed enum
    /// (`Proposed|Planned|Active|Completed|Canceled`) where projects use
    /// lowercase state strings. Conflating the two is a 400 waiting to happen.
    ///
    /// `Proposed` and `Canceled` are never authored, for the `paused` reason
    /// above: both record a human's judgement about the roadmap as a whole,
    /// which the projector does not have.
    fn initiative_status(state: crate::tracker::domain::GroupState) -> &'static str {
        use crate::tracker::domain::GroupState;
        match state {
            GroupState::NotStarted => "Planned",
            GroupState::Active => "Active",
            GroupState::Complete => "Completed",
        }
    }

    /// Resolve the team's id and its workflow states, once per run.
    ///
    /// This is the anti-corruption work Linear forces and GitHub does not. A
    /// team's states are user-defined, so the only stable handle is the state
    /// TYPE. Matching on names would break the moment a team renamed "Todo" to
    /// "Up next" — which teams do constantly, and which is not a schema change
    /// from their point of view.
    async fn team_schema(&self) -> Result<TeamSchema, TrackerError> {
        if let Some(cached) = self
            .schema
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Ok(cached);
        }

        let data = self
            .graphql(
                "query TeamSchema($key: String!) { \
                   teams(filter: { key: { eq: $key } }, first: 1) { \
                     nodes { id key states { nodes { id type position } } \
                             labels { nodes { id name } } } } }",
                json!({ "key": self.team_key }),
            )
            .await?;

        let node = data
            .pointer("/teams/nodes/0")
            .ok_or_else(|| TrackerError::NotFound(format!("Linear team '{}'", self.team_key)))?;
        let team_id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| TrackerError::Transport("team node carried no id".into()))?
            .to_string();

        // A team can have SEVERAL states of one type — a default Linear team has
        // both "In Progress" and "In Review" as `started` — so the map must pick
        // deliberately rather than by arrival.
        //
        // This once took the first of each type, on the stated assumption that
        // Linear returns states in workflow order. The live smoke falsified that
        // outright: a real team came back at positions 0, 5, 4, 1002, 3, 2, 1,
        // so "first wins" filed every in-progress chunk under In Review. Sort by
        // POSITION, which is the field that actually means workflow order, and
        // take the earliest state of each category.
        let mut ordered: Vec<(f64, &str, &str)> = Vec::new();
        if let Some(states) = node.pointer("/states/nodes").and_then(Value::as_array) {
            for s in states {
                if let (Some(id), Some(ty)) = (
                    s.get("id").and_then(Value::as_str),
                    s.get("type").and_then(Value::as_str),
                ) {
                    // A state with no position sorts last rather than first: an
                    // unknown position is not evidence of being the earliest.
                    let position = s
                        .get("position")
                        .and_then(Value::as_f64)
                        .unwrap_or(f64::MAX);
                    ordered.push((position, ty, id));
                }
            }
        }
        ordered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut states_by_type = HashMap::new();
        for (_, ty, id) in ordered {
            states_by_type
                .entry(ty.to_string())
                .or_insert_with(|| id.to_string());
        }

        // Labels are keyed by name because a label has no stable type the way a
        // workflow state does. Discovered once per run alongside the states, so
        // projecting fifty chunks costs one label query rather than fifty.
        let mut labels_by_name = HashMap::new();
        if let Some(labels) = node.pointer("/labels/nodes").and_then(Value::as_array) {
            for l in labels {
                if let (Some(id), Some(name)) = (
                    l.get("id").and_then(Value::as_str),
                    l.get("name").and_then(Value::as_str),
                ) {
                    labels_by_name.insert(name.to_string(), id.to_string());
                }
            }
        }

        let schema = TeamSchema {
            team_id,
            states_by_type,
            labels_by_name,
        };
        *self.schema.lock().unwrap_or_else(|e| e.into_inner()) = Some(schema.clone());
        Ok(schema)
    }

    /// Label ids for `names`, creating any the team does not have yet.
    ///
    /// The cache is read-through AND write-back: a label minted here lands in
    /// the in-memory schema immediately, so projecting fifty chunks that share
    /// a band creates the label ONCE rather than fifty times. Without the
    /// write-back this function would be a duplicate-label factory.
    ///
    /// A creation that fails is logged and skipped rather than failing the
    /// whole projection. Losing a label costs a hash mismatch that the echo
    /// fence reports as drift; failing the write costs the issue itself.
    async fn label_ids_for(&self, names: &[String]) -> Result<Vec<String>, TrackerError> {
        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            if let Some(id) = self
                .schema
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|s| s.labels_by_name.get(name).cloned())
            {
                ids.push(id);
                continue;
            }

            let team_id = self.team_schema().await?.team_id;
            let created = self
                .graphql(
                    "mutation MakeLabel($input: IssueLabelCreateInput!) { \
                       issueLabelCreate(input: $input) { success issueLabel { id name } } }",
                    json!({ "input": { "name": name, "teamId": team_id } }),
                )
                .await;

            match created {
                Ok(data) => {
                    if let Some(id) = data
                        .pointer("/issueLabelCreate/issueLabel/id")
                        .and_then(Value::as_str)
                    {
                        if let Some(schema) = self
                            .schema
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_mut()
                        {
                            schema.labels_by_name.insert(name.clone(), id.to_string());
                        }
                        ids.push(id.to_string());
                    }
                }
                Err(e) => tracing::warn!(
                    target: "think_and_ship::tracker",
                    "could not create Linear label '{name}': {e}; the item is written without it"
                ),
            }
        }
        Ok(ids)
    }

    /// Every `roadmap:*` label the team knows about except `keep`.
    ///
    /// These are the labels WE author, so they are the only ones we may remove.
    /// A team's own labels — Bug, Infra, whatever they invented — are theirs,
    /// and clobbering them is exactly the silent destruction the conflict
    /// policy exists to prevent.
    ///
    /// This is the CANDIDATE set, not the wire payload: Linear rejects removing
    /// a label the issue does not carry ("Label not on issue", a 400 that fails
    /// the whole patch), so the caller intersects this with the labels actually
    /// on the issue. The first full-width live push refuted the no-op
    /// assumption this doc used to state — every patch failed on it.
    fn stale_band_labels(schema: &TeamSchema, keep: &[String]) -> Vec<String> {
        schema
            .labels_by_name
            .iter()
            .filter(|(name, _)| name.starts_with(BAND_PREFIX))
            .filter(|(_, id)| !keep.contains(id))
            .map(|(_, id)| id.clone())
            .collect()
    }

    /// The id of a state matching our canonical state, or `None` when the team
    /// has no analogue.
    ///
    /// `None` is a real answer, not a failure: the projector should let Linear
    /// apply the team's own default rather than invent a state. Each canonical
    /// state has an ordered list of acceptable types so a team that has, say, no
    /// `unstarted` column still gets something sensible.
    fn state_id_for(schema: &TeamSchema, state: WorkItemState) -> Option<String> {
        let preference: &[&str] = match state {
            WorkItemState::Todo => &[TYPE_UNSTARTED, TYPE_BACKLOG, TYPE_TRIAGE],
            WorkItemState::InProgress => &[TYPE_STARTED],
            WorkItemState::Done => &[TYPE_COMPLETED],
            WorkItemState::Cancelled => &[TYPE_CANCELED],
        };
        preference
            .iter()
            .find_map(|ty| schema.states_by_type.get(*ty).cloned())
    }

    /// Map our priority band onto Linear's urgency scale.
    ///
    /// `WorkItem` has no priority field — by design, since it is a narrow
    /// canonical model — so priority reaches an adapter only through the
    /// `roadmap:<band>` label the projector emits. Translating it here, at the
    /// adapter's own boundary, is exactly where provider vocabulary belongs.
    ///
    /// Note the reversal AND the discontinuity: our most important band maps to
    /// Linear's `1`, and our least important maps to `0` — which sits at the
    /// bottom of Linear's ordering while being numerically first.
    fn urgency_from_labels(labels: &[String]) -> Option<u8> {
        labels.iter().find_map(|l| {
            match l
                .strip_prefix("roadmap:")?
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "critical" => Some(1),
                "high" => Some(2),
                "medium" => Some(3),
                "low" => Some(4),
                "later" => Some(0),
                _ => None,
            }
        })
    }

    fn remember(&self, identifier: &str, uuid: &str) {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identifier.to_string(), uuid.to_string());
    }

    /// The UUID behind an identifier, from cache or one lookup.
    ///
    /// Same shape as the GitHub adapter's number-to-database-id cache, for the
    /// same reason: the id a human reads and the id the API needs are different,
    /// and pushing both into the shared link record would put provider structure
    /// into a provider-agnostic type.
    async fn uuid_of(&self, identifier: &str) -> Result<String, TrackerError> {
        if let Some(id) = self
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(identifier)
        {
            return Ok(id.clone());
        }
        let data = self
            .graphql(
                "query IssueId($id: String!) { issue(id: $id) { id identifier } }",
                json!({ "id": identifier }),
            )
            .await?;
        let uuid = data
            .pointer("/issue/id")
            .and_then(Value::as_str)
            .ok_or_else(|| TrackerError::NotFound(identifier.to_string()))?
            .to_string();
        self.remember(identifier, &uuid);
        Ok(uuid)
    }

    /// The label ids an issue currently carries.
    ///
    /// One read per patch, and it is not optional: `removedLabelIds` may only
    /// name labels the issue actually holds, or Linear fails the entire update
    /// with "Label not on issue".
    async fn label_ids_on(&self, uuid: &str) -> Result<Vec<String>, TrackerError> {
        let data = self
            .graphql(
                "query IssueLabels($id: String!) { issue(id: $id) { labels { nodes { id } } } }",
                json!({ "id": uuid }),
            )
            .await?;
        Ok(data
            .pointer("/issue/labels/nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|n| n.get("id").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Read `id`/`identifier`/`updatedAt` out of a mutation payload and cache
    /// the id pair.
    fn outcome_from(&self, issue: &Value, created: bool) -> Result<UpsertOutcome, TrackerError> {
        let identifier = issue
            .get("identifier")
            .and_then(Value::as_str)
            .ok_or_else(|| TrackerError::Transport("issue payload carried no identifier".into()))?;
        if let Some(uuid) = issue.get("id").and_then(Value::as_str) {
            self.remember(identifier, uuid);
        }
        Ok(UpsertOutcome {
            external_id: identifier.to_string(),
            version: issue
                .get("updatedAt")
                .and_then(Value::as_str)
                .map(str::to_string),
            created,
        })
    }
}

/// The fields every issue mutation asks back for. One place, so create and
/// patch cannot drift into returning different shapes.
const ISSUE_FIELDS: &str = "id identifier updatedAt";

/// The namespace for labels this system authors. Everything under it is ours to
/// create and to remove; everything outside it belongs to the team.
const BAND_PREFIX: &str = "roadmap:";

#[async_trait]
impl TrackerPort for LinearTracker {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn capabilities(&self) -> TrackerCapabilities {
        TrackerCapabilities {
            // Native, via issueRelationCreate — see relate_items for the
            // direction trap.
            blocking_links: true,
            labels: true,
            // HONESTLY false. Linear assigns by user UUID only, our canonical
            // assignee is a display string no Linear mutation accepts, and the
            // ownership table says assignees are never ours to author ("We
            // never assign anyone"). So this adapter neither writes assignees
            // nor reads them back — a human's assignment is preserved because
            // no mutation this adapter sends ever mentions the field.
            assignee: false,
            // Linear does not document a body ceiling low enough to enforce, and
            // inventing one would truncate content it would have accepted.
            max_body_len: None,
            required_fields: Vec::new(),
        }
    }

    async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
        let schema = self.team_schema().await?;
        let state_id = Self::state_id_for(&schema, item.state);
        if state_id.is_none() {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "Linear team '{}' has no workflow state for {:?}; leaving the team's default",
                self.team_key, item.state
            );
        }

        let mut input = json!({ "title": item.title, "description": item.body });
        if let Some(sid) = state_id {
            input["stateId"] = json!(sid);
        }
        if let Some(p) = Self::urgency_from_labels(&item.labels) {
            input["priority"] = json!(p);
        }

        // Labels are WRITTEN, not merely read for their priority. This adapter
        // declares `labels: true`, and until this existed that claim was false:
        // the band label was consumed for its urgency and then dropped, so
        // every item's content hash disagreed with its own readback and the
        // echo fence saw drift on every projection.
        let label_ids = self.label_ids_for(&item.labels).await?;

        // The container. Set on BOTH create and patch — an issue whose chunk
        // was regrouped must move, and `projectId` is the same field either way.
        // Resolved by name, never invented: a group with no project upstream is
        // left unfiled rather than silently created here, because `upsert_group`
        // is the one place allowed to create one and the projector calls it first.
        if let Some(group) = &item.group
            && let Some(pid) = self.project_id_for(group).await?
        {
            input["projectId"] = json!(pid);
        }

        match &item.external_id {
            // Identity decides create-vs-patch, never the title.
            Some(identifier) => {
                // ADDITIVE on patch, never a replace-set: `labelIds` would
                // overwrite the whole list and delete labels the team added,
                // which the conflict policy says are theirs. We add ours and
                // remove only stale labels from our own namespace.
                input["addedLabelIds"] = json!(label_ids);

                let uuid = self.uuid_of(identifier).await?;
                // Only remove what is really there: the stale set is a
                // candidate list, and Linear 400s the whole patch on a removal
                // of a label the issue does not carry.
                let current = self.label_ids_on(&uuid).await?;
                let stale: Vec<String> = Self::stale_band_labels(&schema, &label_ids)
                    .into_iter()
                    .filter(|id| current.contains(id))
                    .collect();
                input["removedLabelIds"] = json!(stale);
                let data = self
                    .graphql(
                        &format!(
                            "mutation Patch($id: String!, $input: IssueUpdateInput!) {{ \
                               issueUpdate(id: $id, input: $input) {{ \
                                 success issue {{ {ISSUE_FIELDS} }} }} }}"
                        ),
                        json!({ "id": uuid, "input": input }),
                    )
                    .await?;
                let issue = data.pointer("/issueUpdate/issue").ok_or_else(|| {
                    TrackerError::Transport("issueUpdate returned no issue".into())
                })?;
                self.outcome_from(issue, false)
            }
            None => {
                // A new issue carries no labels yet, so the replace-set and the
                // additive form are the same thing here — and `labelIds` is the
                // only one `IssueCreateInput` accepts.
                input["labelIds"] = json!(label_ids);
                input["teamId"] = json!(schema.team_id);
                let data = self
                    .graphql(
                        &format!(
                            "mutation Create($input: IssueCreateInput!) {{ \
                               issueCreate(input: $input) {{ \
                                 success issue {{ {ISSUE_FIELDS} }} }} }}"
                        ),
                        json!({ "input": input }),
                    )
                    .await?;
                let issue = data.pointer("/issueCreate/issue").ok_or_else(|| {
                    TrackerError::Transport("issueCreate returned no issue".into())
                })?;
                self.outcome_from(issue, true)
            }
        }
    }

    async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError> {
        let data = self
            .graphql(
                "query Changed($key: String!, $since: DateTimeOrDuration!) { \
                   issues(filter: { team: { key: { eq: $key } }, updatedAt: { gt: $since } }, \
                          first: 100) { \
                     nodes { id identifier title description updatedAt priority \
                             state { type } labels { nodes { name } } } } }",
                json!({ "key": self.team_key, "since": since }),
            )
            .await?;

        let Some(rows) = data.pointer("/issues/nodes").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        Ok(rows
            .iter()
            .filter_map(|row| {
                let identifier = row.get("identifier").and_then(Value::as_str)?;
                if let Some(uuid) = row.get("id").and_then(Value::as_str) {
                    self.remember(identifier, uuid);
                }
                Some(WorkItem {
                    // Reading an item back does not resolve its container: the
                    // projector authors grouping, it never learns it from upstream.
                    group: None,
                    external_id: Some(identifier.to_string()),
                    title: row
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    body: row
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    state: state_from_type(
                        row.pointer("/state/type")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                    ),
                    labels: row
                        .pointer("/labels/nodes")
                        .and_then(Value::as_array)
                        .map(|ls| {
                            ls.iter()
                                .filter_map(|l| {
                                    l.get("name").and_then(Value::as_str).map(str::to_string)
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    // Deliberately not read. The display name Linear would hand
                    // back is an identifier no mutation of ours could ever send
                    // (assignment is by user UUID), so surfacing it would put an
                    // unconvergeable field into the echo-fence hash and raise
                    // divergence noise nothing can resolve. See `capabilities`.
                    // Deliberately not read. The display name Linear would hand
                    // back is an identifier no mutation of ours could ever send
                    // (assignment is by user UUID), so surfacing it would put an
                    // unconvergeable field into the echo-fence hash and raise
                    // divergence noise nothing can resolve. See `capabilities`.
                    assignee: None,
                    version: row
                        .get("updatedAt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect())
    }

    /// Declare that `from` is blocked by each of `blocked_by`.
    ///
    /// # The direction trap
    ///
    /// Linear has NO `blocked_by` relation type. It stores ONE relation and
    /// renders it from both ends: marking A as *blocking* B makes B display
    /// "Blocked by A". So to say "`from` is blocked by X" we must create the
    /// relation with `issueId` = **X, the blocker** and `relatedIssueId` =
    /// `from`. That is the opposite argument order from GitHub, where you post
    /// to the blocked issue's own collection.
    ///
    /// Getting this backwards does not error — it silently inverts the entire
    /// dependency graph, which is worse than a failure, so the test pins WHICH
    /// id lands in WHICH field rather than merely that a call was made.
    async fn relate_items(
        &self,
        from: &ExternalId,
        blocked_by: &[ExternalId],
    ) -> Result<(), TrackerError> {
        let blocked = self.uuid_of(from).await?;
        for blocker_identifier in blocked_by {
            let blocker = self.uuid_of(blocker_identifier).await?;
            self.graphql(
                "mutation Relate($input: IssueRelationCreateInput!) { \
                   issueRelationCreate(input: $input) { success } }",
                json!({
                    "input": {
                        // The BLOCKER is the subject of a `blocks` relation.
                        "issueId": blocker,
                        "relatedIssueId": blocked,
                        "type": "blocks",
                    }
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Ensure a Linear PROJECT exists for this group and carries the derived
    /// state.
    ///
    /// Idempotent as the port requires: resolve by REMEMBERED UUID first, the
    /// name only when no uuid was ever recorded, create only when absent, and
    /// patch the state only when it actually differs. The projector calls this
    /// for every group on every push, so an unconditional write would churn
    /// the workspace and burn the complexity budget for nothing.
    ///
    /// OWNERSHIP: the name is ours at creation only
    /// — an upstream rename is observed, logged, and left standing, and the
    /// remembered uuid is why that costs nothing. The state is patched only
    /// when the current value is one WE author (`planned`/`started`/
    /// `completed`); `paused` and `canceled` record a human's intent to stop,
    /// which the roadmap does not know, and are never patched over.
    ///
    /// WHAT IS DELIBERATELY NOT SENT: targetDate, lead, description and health.
    /// The roadmap has priorities, not deadlines, and no notion of who is
    /// running a workstream — writing those would be the projector inventing
    /// opinions, and every one of them is a field a human will want to own.
    async fn upsert_group(
        &self,
        group: &crate::tracker::domain::WorkGroup,
    ) -> Result<UpsertOutcome, TrackerError> {
        let wanted = Self::project_state(group.state);
        let roof = self
            .initiative
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // IDENTITY FIRST. A remembered uuid outranks the name — the name is
        // the one field a human plausibly edited since our last push. A
        // remembered uuid that fails to resolve is an ERROR, not a fall
        // through to create: "deleted" and "transport hiccup" are not
        // reliably distinguishable here, and guessing wrong mints a duplicate.
        let resolved = match &group.external_id {
            Some(uuid) => Some(uuid.clone()),
            None => self.project_id_for(&group.name).await?,
        };

        if let Some(id) = resolved {
            // Read name, state AND initiative membership in one query: an
            // unchanged project must cost zero writes, and the membership read
            // riding along is what lets an already-linked project skip the
            // join mutation too — the read-before-write lesson the label
            // removal taught, applied at birth instead of after a live 400.
            let data = self
                .graphql(
                    "query ProjectState($id: String!) { project(id: $id) { name state \
                       initiatives(first: 50) { nodes { id } } } }",
                    json!({ "id": id }),
                )
                .await?;
            // Seed the name→uuid cache under OUR name and the upstream one, so
            // `upsert_item`'s projectId resolution follows the remembered
            // identity instead of missing on a renamed project.
            let actual = data
                .pointer("/project/name")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Ok(mut cache) = self.projects.lock() {
                cache.insert(group.name.clone(), id.clone());
                if let Some(actual) = &actual {
                    cache.insert(actual.clone(), id.clone());
                }
            }
            if let Some(actual) = &actual
                && actual != &group.name
            {
                tracing::info!(
                    target: "think_and_ship::tracker",
                    "project for group '{}' is named '{actual}' upstream — the rename is a \
                     human's and is left standing",
                    group.name
                );
            }
            // THE LINK COMES BEFORE THE STATE EARLY-RETURN, deliberately: on
            // the first push after the roof exists, every already-mirrored
            // project has an unchanged state and would skip right past a link
            // placed below it.
            if let Some(initiative_id) = &roof {
                let linked = data
                    .pointer("/project/initiatives/nodes")
                    .and_then(Value::as_array)
                    .is_some_and(|nodes| {
                        nodes
                            .iter()
                            .any(|n| n.get("id").and_then(Value::as_str) == Some(initiative_id))
                    });
                if !linked {
                    self.graphql(
                        "mutation LinkProjectToInitiative($input: InitiativeToProjectCreateInput!) { \
                           initiativeToProjectCreate(input: $input) { success } }",
                        json!({ "input": { "initiativeId": initiative_id, "projectId": id } }),
                    )
                    .await?;
                }
            }
            let current = data
                .pointer("/project/state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let outcome = UpsertOutcome {
                external_id: id.clone(),
                version: None,
                created: false,
            };
            if current == wanted {
                return Ok(outcome);
            }
            // THE VOCABULARY GUARD. Only a state we author may be re-derived;
            // anything else is a human's judgement about the work, and
            // patching it back on every push is the silent overwrite this
            // chunk exists to kill.
            if !matches!(current, "planned" | "started" | "completed") {
                tracing::info!(
                    target: "think_and_ship::tracker",
                    "project for group '{}' sits in state '{current}', outside the projector's \
                     vocabulary — a human's decision, left standing",
                    group.name
                );
                return Ok(outcome);
            }
            self.graphql(
                "mutation SetProjectState($id: String!, $input: ProjectUpdateInput!) { \
                   projectUpdate(id: $id, input: $input) { success } }",
                json!({ "id": id, "input": { "state": wanted } }),
            )
            .await?;
            return Ok(outcome);
        }

        // Absent: create it against the configured team. `teamIds` is required —
        // a project with no team is not a thing Linear will make.
        let team_id = self.team_schema().await?.team_id;
        let data = self
            .graphql(
                "mutation CreateProject($input: ProjectCreateInput!) { \
                   projectCreate(input: $input) { success project { id name } } }",
                json!({
                    "input": {
                        "name": group.name,
                        "teamIds": [team_id],
                        "state": wanted,
                    }
                }),
            )
            .await?;
        if data.pointer("/projectCreate/success") != Some(&Value::Bool(true)) {
            return Err(TrackerError::Status {
                status: 400,
                body: format!(
                    "Linear declined to create project '{}' without saying why: {data}",
                    group.name
                ),
            });
        }
        // The id is REQUIRED now: it is the identity the caller records, and a
        // create that answered success without one would leave the very next
        // push resolving by name again — the rename hole reopened.
        let created_id = data
            .pointer("/projectCreate/project/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                TrackerError::Transport("projectCreate succeeded but returned no project id".into())
            })?;
        // Cache the new id under the name we asked for AND the name Linear
        // recorded. The team-key episode is the precedent: a provider may
        // normalize what you send, and caching only the request would make the
        // very next lookup miss and create a duplicate.
        if let Some(actual) = data
            .pointer("/projectCreate/project/name")
            .and_then(Value::as_str)
            && let Ok(mut cache) = self.projects.lock()
        {
            cache.insert(group.name.clone(), created_id.clone());
            cache.insert(actual.to_string(), created_id.clone());
        }
        // A project born after the roof joins it at birth — no membership read
        // needed, because a project that did not exist a moment ago belongs to
        // nothing.
        if let Some(initiative_id) = &roof {
            self.graphql(
                "mutation LinkProjectToInitiative($input: InitiativeToProjectCreateInput!) { \
                   initiativeToProjectCreate(input: $input) { success } }",
                json!({ "input": { "initiativeId": initiative_id, "projectId": created_id } }),
            )
            .await?;
        }
        Ok(UpsertOutcome {
            external_id: created_id,
            version: None,
            created: true,
        })
    }

    /// Ensure the workspace INITIATIVE exists and carries the derived status,
    /// remembering its uuid so this run's `upsert_group` calls can file their
    /// projects under it.
    ///
    /// Same idiom as `upsert_group` one level up: resolve by remembered uuid
    /// first, name only when no uuid was ever recorded, create only when
    /// absent, patch the status only when it differs AND the current value is
    /// one we author. Initiatives are WORKSPACE-scoped, not team-scoped —
    /// there is no team to scope the name to, which makes the remembered uuid
    /// matter MORE here: a renamed initiative resolved by name would bind to
    /// anything in the workspace that happens to match.
    ///
    /// WHAT IS DELIBERATELY NOT SENT, following the project rule: owner,
    /// targetDate, description, icon, color. The status is the whole of our
    /// claim, and even it uses only the three honest values —
    /// `Self::initiative_status` explains why `Proposed` and `Canceled` are
    /// never authored, and the vocabulary guard is why they are never patched
    /// over either.
    async fn upsert_initiative(
        &self,
        initiative: &crate::tracker::domain::WorkGroup,
    ) -> Result<UpsertOutcome, TrackerError> {
        let wanted = Self::initiative_status(initiative.state);

        // IDENTITY FIRST, as for projects: the uuid the caller remembered
        // outranks the name, and a uuid that fails to resolve is an error
        // rather than a licence to create a duplicate.
        let found = match &initiative.external_id {
            Some(uuid) => {
                let data = self
                    .graphql(
                        "query InitiativeById($id: String!) { \
                           initiative(id: $id) { id name status } }",
                        json!({ "id": uuid }),
                    )
                    .await?;
                let name = data
                    .pointer("/initiative/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !name.is_empty() && name != initiative.name {
                    tracing::info!(
                        target: "think_and_ship::tracker",
                        "the initiative for '{}' is named '{name}' upstream — the rename is a \
                         human's and is left standing",
                        initiative.name
                    );
                }
                let status = data
                    .pointer("/initiative/status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Some((uuid.clone(), status.to_string()))
            }
            // Resolve by name. No team scoping exists for initiatives, so an
            // eq filter on the exact name is the strongest identity available
            // until a uuid has been recorded.
            None => {
                let data = self
                    .graphql(
                        "query Initiatives($filter: InitiativeFilter) { \
                           initiatives(filter: $filter, first: 5) { nodes { id name status } } }",
                        json!({ "filter": { "name": { "eq": initiative.name } } }),
                    )
                    .await?;
                data.pointer("/initiatives/nodes")
                    .and_then(Value::as_array)
                    .and_then(|nodes| {
                        nodes.iter().find_map(|n| {
                            let id = n.get("id").and_then(Value::as_str)?;
                            let status =
                                n.get("status").and_then(Value::as_str).unwrap_or_default();
                            Some((id.to_string(), status.to_string()))
                        })
                    })
            }
        };

        if let Some((id, status)) = found {
            *self.initiative.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
            let outcome = UpsertOutcome {
                external_id: id.clone(),
                version: None,
                created: false,
            };
            if status == wanted {
                return Ok(outcome);
            }
            // The vocabulary guard, in the initiative's OWN closed set: a
            // status we never author is a human's, and stays.
            if !matches!(status.as_str(), "Planned" | "Active" | "Completed") {
                tracing::info!(
                    target: "think_and_ship::tracker",
                    "initiative '{}' sits in status '{status}', outside the projector's \
                     vocabulary — a human's decision, left standing",
                    initiative.name
                );
                return Ok(outcome);
            }
            self.graphql(
                "mutation SetInitiativeStatus($id: String!, $input: InitiativeUpdateInput!) { \
                   initiativeUpdate(id: $id, input: $input) { success } }",
                json!({ "id": id, "input": { "status": wanted } }),
            )
            .await?;
            return Ok(outcome);
        }

        let data = self
            .graphql(
                "mutation CreateInitiative($input: InitiativeCreateInput!) { \
                   initiativeCreate(input: $input) { success initiative { id } } }",
                json!({ "input": { "name": initiative.name, "status": wanted } }),
            )
            .await?;
        if data.pointer("/initiativeCreate/success") != Some(&Value::Bool(true)) {
            return Err(TrackerError::Status {
                status: 400,
                body: format!(
                    "Linear declined to create initiative '{}' without saying why: {data}",
                    initiative.name
                ),
            });
        }
        let id = data
            .pointer("/initiativeCreate/initiative/id")
            .and_then(Value::as_str)
            .map(str::to_string);
        // A create that answered success but no id would leave every project
        // unlinked while looking healthy — say so instead.
        let id = id.ok_or_else(|| {
            TrackerError::Transport(
                "initiativeCreate succeeded but returned no initiative id".into(),
            )
        })?;
        *self.initiative.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
        Ok(UpsertOutcome {
            external_id: id,
            version: None,
            created: true,
        })
    }

    /// Resolve the team by key, reporting what was found.
    ///
    /// Deliberately routed through `Self::team_schema` rather than a fresh
    /// query: that is the function whose `NotFound` every other verb already
    /// depends on, so a probe cannot disagree with the thing it is meant to
    /// predict. It is memoized per run, so probing costs nothing a later push
    /// was not going to pay anyway.
    async fn probe_target(&self) -> Result<TargetInfo, TrackerError> {
        let schema = self.team_schema().await?;
        Ok(TargetInfo {
            key: self.team_key.clone(),
            name: self.team_key.clone(),
            detail: format!(
                "{} workflow state(s), {} label(s) already known",
                schema.states_by_type.len(),
                schema.labels_by_name.len()
            ),
        })
    }

    /// Create the team, then prove it is usable by resolving it back.
    ///
    /// The round trip is the point. `teamCreate` reporting success is not
    /// evidence the adapter can now work — the team needs workflow states before
    /// a single issue can be filed, and Linear seeds those asynchronously. So
    /// this re-probes and returns the RESOLVED team, which means a caller that
    /// gets `Ok` holds something push can actually use.
    ///
    /// On permission failure Linear's own message travels out untouched, inside
    /// the `Status` variant `graphql` already produces from the GraphQL error
    /// envelope. Creating a team is not something every key may do, and a
    /// generic "failed" would leave the human guessing between a bad key, a
    /// missing seat and an admin-only workspace.
    async fn create_target(&self, display_name: &str) -> Result<TargetInfo, TrackerError> {
        let name = display_name.trim();
        let name = if name.is_empty() {
            &self.team_key
        } else {
            name
        };

        let data = self
            .graphql(
                "mutation CreateTeam($input: TeamCreateInput!) { \
                   teamCreate(input: $input) { success team { id key name } } }",
                json!({ "input": { "name": name, "key": self.team_key } }),
            )
            .await?;

        // `success: false` with no GraphQL error is a documented shape, so a
        // silent false must not read as a created team.
        if data.pointer("/teamCreate/success") != Some(&Value::Bool(true)) {
            return Err(TrackerError::Status {
                status: 400,
                body: format!(
                    "Linear declined to create team '{}' without saying why: {data}",
                    self.team_key
                ),
            });
        }

        // TRUST THE RESPONSE, NOT THE REQUEST. Linear assigns the key; asking
        // for one is a suggestion it may rewrite. The constructor now refuses
        // the inputs known to be rewritten, but a server-side rule we do not
        // know about would otherwise put us right back in the state this guard
        // exists for: a team created, and then declared missing because we
        // looked for the name we asked for instead of the one we got.
        //
        // The team EXISTS at this point, so the message must say so — otherwise
        // the obvious next move is to run the command again and make a second.
        let assigned = data
            .pointer("/teamCreate/team/key")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !assigned.is_empty() && assigned != self.team_key {
            return Err(TrackerError::Unsupported(format!(
                "Linear created the team but assigned it the key '{assigned}', not \
                 the '{}' that was asked for — Linear rewrites keys it does not \
                 accept verbatim.\n\nTHE TEAM NOW EXISTS. Do not run this again, or \
                 you will create a second one. Re-run pointing at the real key:\n  \
                 --into {assigned}",
                self.team_key
            )));
        }

        // Drop any memoized miss from the probe that preceded this, or the
        // re-resolve below would report the team still absent.
        if let Ok(mut cached) = self.schema.lock() {
            *cached = None;
        }
        self.probe_target().await
    }
}

/// Linear's state TYPE to our canonical state. Types are stable; names are not.
fn state_from_type(ty: &str) -> WorkItemState {
    match ty {
        TYPE_STARTED => WorkItemState::InProgress,
        TYPE_COMPLETED => WorkItemState::Done,
        TYPE_CANCELED => WorkItemState::Cancelled,
        _ => WorkItemState::Todo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_with(types: &[(&str, &str)]) -> TeamSchema {
        TeamSchema {
            team_id: "team-uuid".into(),
            states_by_type: types
                .iter()
                .map(|(t, id)| ((*t).to_string(), (*id).to_string()))
                .collect(),
            labels_by_name: HashMap::new(),
        }
    }

    /// `stale_band_labels` may only ever propose removing labels from OUR
    /// namespace. A team's own labels are theirs, and clobbering them is the
    /// silent destruction the conflict policy exists to prevent.
    #[test]
    fn only_our_own_namespace_is_ever_proposed_for_removal() {
        let schema = TeamSchema {
            team_id: "team-uuid".into(),
            states_by_type: HashMap::new(),
            labels_by_name: [
                ("roadmap:critical", "lbl-critical"),
                ("roadmap:later", "lbl-later"),
                ("Bug", "lbl-bug"),
                ("Infra", "lbl-infra"),
            ]
            .into_iter()
            .map(|(n, i)| (n.to_string(), i.to_string()))
            .collect(),
        };

        let keep = vec!["lbl-critical".to_string()];
        let mut stale = LinearTracker::stale_band_labels(&schema, &keep);
        stale.sort();

        assert_eq!(
            stale,
            vec!["lbl-later".to_string()],
            "only the OTHER roadmap:* label may be removed — never the team's own"
        );
    }

    /// An empty keep-set means we are setting no band, so every band label of
    /// ours is stale. Still no team label is touched.
    #[test]
    fn setting_no_band_clears_ours_and_still_spares_theirs() {
        let schema = TeamSchema {
            team_id: "team-uuid".into(),
            states_by_type: HashMap::new(),
            labels_by_name: [("roadmap:high", "lbl-high"), ("Bug", "lbl-bug")]
                .into_iter()
                .map(|(n, i)| (n.to_string(), i.to_string()))
                .collect(),
        };

        let stale = LinearTracker::stale_band_labels(&schema, &[]);
        assert_eq!(stale, vec!["lbl-high".to_string()]);
    }

    #[test]
    fn a_malformed_team_key_is_refused_rather_than_guessed() {
        for bad in ["", "  ", "owner/repo", "two words"] {
            assert!(LinearTracker::new(bad).is_err(), "'{bad}' must not parse");
        }
        let ok = LinearTracker::new("eng").expect("valid");
        assert_eq!(ok.provider(), "linear");
        // Keys are upper-case in every identifier Linear renders.
        assert_eq!(ok.team_key(), "ENG");
    }

    /// THE REGRESSION, from a real workspace. `--into WOW-AI` passed validation,
    /// Linear created the team under the key `WOW` (it strips punctuation), and
    /// the create path then re-probed for `WOW-AI`, found nothing, and reported
    /// FAILURE on a team it had just made — leaving a real team behind and an
    /// error on screen.
    ///
    /// A key Linear would rewrite must therefore be refused BEFORE any network
    /// call, where the only cost is a message.
    #[test]
    fn a_key_linear_would_silently_rewrite_is_refused_before_any_network_call() {
        for rewritten in ["WOW-AI", "my_team", "a.b", "TEAM!", "acme-eng"] {
            let err = LinearTracker::new(rewritten)
                .err()
                .unwrap_or_else(|| panic!("'{rewritten}' must be refused — Linear rewrites it"));
            let msg = err.to_string();
            assert!(
                msg.contains("letters and digits"),
                "the refusal must say WHY, so the fix is obvious: {msg}"
            );
        }
        // Alphanumeric keys stay valid — the guard must not become a blanket ban.
        for good in ["ENG", "WOW", "T2", "eng2"] {
            assert!(
                LinearTracker::new(good).is_ok(),
                "'{good}' is a legitimate key and must still work"
            );
        }
    }

    /// The anti-corruption property: a team whose states are named nothing like
    /// the defaults still maps correctly, because only the TYPE is consulted.
    #[test]
    fn states_map_by_type_never_by_name() {
        let team = schema_with(&[
            ("unstarted", "s-up-next"),
            ("started", "s-cooking"),
            ("completed", "s-shipped"),
            ("canceled", "s-binned"),
        ]);
        assert_eq!(
            LinearTracker::state_id_for(&team, WorkItemState::Todo).as_deref(),
            Some("s-up-next")
        );
        assert_eq!(
            LinearTracker::state_id_for(&team, WorkItemState::InProgress).as_deref(),
            Some("s-cooking")
        );
        assert_eq!(
            LinearTracker::state_id_for(&team, WorkItemState::Done).as_deref(),
            Some("s-shipped")
        );
        assert_eq!(
            LinearTracker::state_id_for(&team, WorkItemState::Cancelled).as_deref(),
            Some("s-binned")
        );
    }

    /// A team missing a category must degrade, not fail: Todo falls back through
    /// backlog to triage, and a genuinely absent category yields None so Linear
    /// applies the team's own default.
    #[test]
    fn a_missing_state_category_degrades_rather_than_failing() {
        let sparse = schema_with(&[("backlog", "s-someday"), ("started", "s-doing")]);
        assert_eq!(
            LinearTracker::state_id_for(&sparse, WorkItemState::Todo).as_deref(),
            Some("s-someday"),
            "Todo falls back to backlog when the team has no unstarted column"
        );
        assert!(
            LinearTracker::state_id_for(&sparse, WorkItemState::Done).is_none(),
            "no completed category means defer to Linear, not invent a state"
        );
    }

    /// The scales run in OPPOSITE directions, and Linear's 0 is a hole rather
    /// than the top: our most important band is Linear's 1, and our least
    /// important is Linear's 0.
    #[test]
    fn priority_reverses_and_is_not_monotone() {
        let band = |b: &str| LinearTracker::urgency_from_labels(&[format!("roadmap:{b}")]);
        assert_eq!(band("critical"), Some(1));
        assert_eq!(band("high"), Some(2));
        assert_eq!(band("medium"), Some(3));
        assert_eq!(band("low"), Some(4));
        assert_eq!(
            band("later"),
            Some(0),
            "0 means 'no priority', not 'urgent'"
        );

        // Ours ascends as importance falls; Linear's ascends too — except for
        // the wrap at `later`. Anything assuming monotonicity breaks here.
        assert!(band("critical") < band("low"));
        assert!(band("later") < band("critical"));
    }

    #[test]
    fn a_label_that_is_not_a_band_yields_no_priority() {
        assert_eq!(LinearTracker::urgency_from_labels(&[]), None);
        assert_eq!(
            LinearTracker::urgency_from_labels(&["bug".into(), "roadmap:nonsense".into()]),
            None
        );
    }

    /// THE difference a REST-shaped adapter gets wrong: a failed GraphQL
    /// mutation arrives with HTTP 200. Reading the status alone would record a
    /// tracker link for an issue that was never created.
    #[test]
    fn graphql_errors_are_read_from_the_body_not_the_status() {
        let rejected = json!({
            "errors": [{ "message": "Team not found", "extensions": { "code": "INVALID_INPUT" } }]
        });
        let err = LinearTracker::read_envelope(rejected).expect_err("must be an error");
        match err {
            TrackerError::Status { status, ref body } => {
                assert_eq!(status, 400);
                assert!(body.contains("Team not found"));
            }
            other => panic!("expected a contract rejection, got {other:?}"),
        }
        // A contract rejection must never be queued for replay.
        assert!(!err.retryable());
    }

    /// Throttling is reported in the error body too, and misreading it as a
    /// contract error would DROP the write instead of queueing it.
    #[test]
    fn a_throttling_error_body_is_retryable() {
        let throttled = json!({
            "errors": [{ "message": "slow down", "extensions": { "code": "RATELIMITED" } }]
        });
        let err = LinearTracker::read_envelope(throttled).expect_err("must be an error");
        assert!(matches!(err, TrackerError::RateLimited { .. }));
        assert!(err.retryable(), "throttling must queue, not drop");
    }

    #[test]
    fn a_clean_envelope_yields_its_data() {
        let ok = json!({ "data": { "issueCreate": { "success": true } } });
        let data = LinearTracker::read_envelope(ok).expect("data");
        assert_eq!(data.pointer("/issueCreate/success"), Some(&json!(true)));
    }

    /// An empty `errors` array is not an error — some servers include the key
    /// unconditionally.
    #[test]
    fn an_empty_errors_array_is_not_a_failure() {
        let ok = json!({ "errors": [], "data": { "x": 1 } });
        assert!(LinearTracker::read_envelope(ok).is_ok());
    }

    #[test]
    fn capabilities_declare_what_linear_actually_supports() {
        let caps = LinearTracker::new("ENG").expect("valid").capabilities();
        assert!(caps.blocking_links, "issueRelationCreate is native");
        assert!(caps.required_fields.is_empty());
        assert_eq!(caps.max_body_len, None);
    }

    /// The budget is the adapter's own declaration, on the GraphQL bucket.
    #[test]
    fn the_adapter_declares_its_own_graphql_budget() {
        let linear = LinearTracker::new("ENG").expect("valid");
        let budget = linear.budget.lock().expect("lock");
        assert_eq!(
            budget.remaining("linear", Transport::GraphQl),
            Some(COMPLEXITY_PER_HOUR)
        );
        // It never declared a REST bucket, so REST is unlimited-by-absence
        // rather than accidentally shared.
        assert_eq!(budget.remaining("linear", Transport::Rest), None);
    }

    #[test]
    fn state_types_round_trip_back_to_canonical_states() {
        assert_eq!(state_from_type("started"), WorkItemState::InProgress);
        assert_eq!(state_from_type("completed"), WorkItemState::Done);
        assert_eq!(state_from_type("canceled"), WorkItemState::Cancelled);
        assert_eq!(state_from_type("backlog"), WorkItemState::Todo);
        assert_eq!(state_from_type("unstarted"), WorkItemState::Todo);
        // An unknown type from a future Linear must not panic.
        assert_eq!(state_from_type("brand-new"), WorkItemState::Todo);
    }
}
