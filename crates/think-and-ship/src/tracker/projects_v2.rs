//! GitHub Projects v2 — the board transport, and what the board will let us say.
//!
//! The file is layered. The network half: how a board is addressed, how a
//! request is charged, how a GraphQL answer is read. The anti-corruption half
//! at the bottom of the file: what fields the board actually has, which of
//! them can hold a lifecycle, and — the part that matters — when to REFUSE
//! rather than write. The identity half below that: a board item is ATTACHED
//! to the issue the Issues adapter already linked, never minted as a parallel
//! twin. The projector wiring comes last, and everything here can be proven
//! against an in-process mock with no credential, exactly as
//! [`github`](crate::tracker::github) chose for REST.
//!
//! # Why a second GitHub client
//!
//! [`GithubTracker`](crate::tracker::GithubTracker) speaks REST, because GitHub
//! Issues is a REST API. Projects v2 has no REST surface at all: Projects
//! (classic) had one and was sunset (github.blog changelog `2024-05-23`, echoed
//! in octokit/graphql-schema's own deprecation comment), and the replacement is
//! GraphQL-only. So this is the same provider reached over a different
//! transport, which is precisely the distinction [`RateBudget`] was keyed on.
//!
//! # The address, and the two nouns it has to keep apart
//!
//! Projects (classic) was **repository**-scoped: `/OWNER/REPO/projects/1`. A
//! Projects v2 board is **owner**-scoped and can span repositories, so
//! `owner/repo` — the address every other GitHub thing in this codebase uses —
//! cannot name one. Worse, the owner's KIND is load-bearing: GraphQL exposes
//! `organization(login:)` and `user(login:)` as two different root fields with
//! no common ancestor carrying `projectV2`, and a login string does not say
//! which it is. Guessing costs a wasted request AND collides with the envelope
//! rule below, because asking `organization()` about a user is not a null — it
//! is a top-level GraphQL error.
//!
//! So the address carries the kind, in the shape GitHub's own URL already uses
//! and a human can therefore paste:
//!
//! ```text
//!   https://github.com/orgs/acme/projects/12    (or the bare orgs/acme/projects/12)
//!   https://github.com/users/alrik/projects/3   (or the bare users/alrik/projects/3)
//! ```
//!
//! # What a request costs, which is not what the sibling adapter charges
//!
//! GitHub uses the word *point* for two unrelated quantities, and reading them
//! as one is the mistake this module exists to not make:
//!
//! - The **secondary** limit, stated identically on the REST and the GraphQL
//!   page: no more than 900 points/minute against REST endpoints and no more
//!   than 2,000 points/minute against the GraphQL endpoint, where a point is a
//!   per-request WEIGHT — `GET`/`HEAD`/`OPTIONS` and *GraphQL requests without
//!   mutations* cost 1, `POST`/`PATCH`/`PUT`/`DELETE` and *GraphQL requests with
//!   mutations* cost 5.
//! - The **primary** limits, which are separate per transport and in different
//!   units again: 5,000 *requests* per hour for REST, 5,000 *points* per hour
//!   for GraphQL where that point is query COMPLEXITY (connections divided by
//!   100), a property of the query text rather than of the verb.
//!
//! [`RateBudget::github`] configures the SECONDARY limits, which is what this
//! module charges and is correct. The primary limits are not modelled — one
//! bucket per `(provider, transport)` cannot hold two windows, and no verb
//! weight can express a complexity score. That gap is recorded rather than
//! papered over; see `projects-v2-primary-limit-unmodelled` on the roadmap.
//!
//! [`LinearTracker`](crate::tracker::LinearTracker) charges a flat 1 for every
//! call including mutations. That is honest for Linear, which bills complexity
//! and nothing else. Copying it here would under-charge every write by 5x
//! against a limit GitHub actually enforces, so [`Op`] exists to make the verb
//! visible at the call site.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tracker::budget::{RateBudget, Spend, Transport};
use crate::tracker::credential::AuthScheme;
use crate::tracker::domain::{TrackerCapabilities, WorkItem, WorkItemState};
use crate::tracker::port::{TrackerError, TrackerPort, UpsertOutcome};
use crate::tracker::registry::{ProviderBuild, ProviderRegistration};

/// GitHub's API host. Overridden in tests with a mock server.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// GitHub's documented secondary limit for the GraphQL endpoint: 2,000 points
/// per minute. Declared here rather than in `budget.rs` — an adapter naming its
/// own limits is what keeps the provider list out of the core.
const GRAPHQL_POINTS_PER_MINUTE: u32 = 2_000;
const WINDOW_SECS: u64 = 60;

/// Which verb a GraphQL request carries, because GitHub charges them
/// differently.
///
/// An enum rather than a bare `u32` at each call site: the weight is a fact
/// about GitHub's meter, not a number a caller should be free to invent, and a
/// future operation added here has to state its verb to compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// "GraphQL requests without mutations" — 1 point.
    Query,
    /// "GraphQL requests with mutations" — 5 points.
    Mutation,
}

impl Op {
    /// The secondary-limit weight GitHub charges for this verb.
    #[must_use]
    pub fn points(self) -> u32 {
        match self {
            Self::Query => 1,
            Self::Mutation => 5,
        }
    }
}

/// Whether a board's owner is an organisation or a user.
///
/// Not derivable from the login: the two are different GraphQL root fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    Organization,
    User,
}

impl OwnerKind {
    /// The GraphQL root field that resolves this kind of owner.
    #[must_use]
    pub fn root_field(self) -> &'static str {
        match self {
            Self::Organization => "organization",
            Self::User => "user",
        }
    }

    /// The URL segment GitHub itself uses, and therefore the one a human pastes.
    #[must_use]
    pub fn url_segment(self) -> &'static str {
        match self {
            Self::Organization => "orgs",
            Self::User => "users",
        }
    }

    /// Which kind of credential can actually reach a board of this owner kind.
    ///
    /// Total by construction, so a third owner kind could not be added without
    /// answering this question for it.
    #[must_use]
    pub fn required_credential(self) -> BoardCredential {
        match self {
            Self::Organization => BoardCredential::InstallationToken,
            Self::User => BoardCredential::UserToken,
        }
    }
}

/// Which credential a Projects v2 board will actually accept — the answer to a
/// question that cost a whole chunk to settle, recorded here so the board lane
/// reads it instead of re-deriving it.
///
/// THE FINDING: a GitHub App INSTALLATION token cannot reach a USER-owned
/// board at all. It is not a scope that was forgotten; there is no user-projects
/// permission in the App permission vocabulary to ask for.
///
/// THE EVIDENCE, and its asymmetry, stated honestly because half of it is
/// documentary rather than probed:
///
/// - GitHub's permissions reference files `organization_projects` under
///   ORGANIZATION permissions, and no user-projects key exists anywhere in the
///   set. Fine-grained PATs share that vocabulary and have no account-level
///   Projects permission either, so this is a property of the permission MODEL
///   rather than an oversight specific to Apps.
/// - GitHub support confirmed the limitation directly in
///   github.com/orgs/community/discussions/46681, where a staff member also
///   conceded it is undocumented. Nothing since reverses it.
/// - PROBED LIVE (the positive half only): a user token carrying the `project`
///   scope resolves `users/AlrikOlson/projects/1`, and `organization(login:)`
///   on that same login returns a genuine NOT_FOUND.
/// - NOT PROBED (the negative half): that an installation token is refused.
///   Confirming it needs a real registered GitHub App, which
///   `github-app-installation-token` will make possible.
///
/// THE TRAP: GitHub's own "Using the API to manage Projects" page names only
/// the `project` / `read:project` scopes, lists installation access tokens as a
/// supported token type, and never distinguishes user-owned from org-owned
/// boards. It is the page a reader would consult first and it is the one source
/// that reads as a yes. Do not "correct" this enum against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardCredential {
    /// A GitHub App installation token, carrying `organization_projects`.
    InstallationToken,
    /// A user token — a classic PAT, an OAuth-app token, or a GitHub App
    /// user-to-server token — carrying the `project` (write) or `read:project`
    /// (read) scope. The only lane that reaches a user-owned board.
    UserToken,
}

impl BoardCredential {
    /// The grant this credential must carry, named the way the provider names
    /// it — an App PERMISSION key on one side, an OAuth SCOPE on the other.
    /// They are deliberately different vocabularies, and flattening them into
    /// one string is how the two lanes get confused for each other.
    #[must_use]
    pub fn required_grant(self) -> &'static str {
        match self {
            Self::InstallationToken => "organization_projects",
            Self::UserToken => "project",
        }
    }

    /// Whether the GitHub App credential lane can serve this board at all.
    ///
    /// The projector wiring's actual hazard: the App credential in the board
    /// lane looks correct, compiles, and then cannot see the only board this
    /// project has ever touched.
    #[must_use]
    pub fn reachable_by_github_app_installation(self) -> bool {
        matches!(self, Self::InstallationToken)
    }
}

/// A board, addressed the way GitHub's own URL addresses one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardAddress {
    pub owner_kind: OwnerKind,
    pub login: String,
    pub number: u64,
}

impl BoardAddress {
    /// Parse a board address, accepting the full URL or its path.
    ///
    /// Every refusal names what was actually looked for, because the failure
    /// modes here are all "you pasted a different GitHub noun" and a bare
    /// "invalid target" would leave the human guessing which one.
    pub fn parse(target: &str) -> Result<Self, TrackerError> {
        let raw = target.trim();
        if raw.is_empty() {
            return Err(TrackerError::Unsupported(
                "no Projects v2 board was given — a board address looks like \
                 orgs/ACME/projects/12 or users/LOGIN/projects/3, which is the \
                 path of the board's own URL on github.com"
                    .into(),
            ));
        }

        // Strip the scheme and host so a pasted URL and a typed path are the
        // same input from here down.
        let path = raw
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.")
            .trim_start_matches("github.com")
            .trim_matches('/');

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        // The classic shape, which is a DIFFERENT product rather than a typo.
        // Caught before the generic error so the message can say so.
        if parts.len() == 4 && parts[2] == "projects" && !matches!(parts[0], "orgs" | "users") {
            return Err(TrackerError::Unsupported(format!(
                "'{raw}' is a repository-scoped projects URL — that is Projects \
                 (classic), which GitHub sunset and which has no ProjectV2 \
                 behind it. A Projects v2 board is owned by an organisation or a \
                 user, so its address starts with orgs/ or users/, as in \
                 orgs/{}/projects/12",
                parts[0]
            )));
        }

        if parts.len() != 4 || parts[2] != "projects" {
            return Err(TrackerError::Unsupported(format!(
                "'{raw}' is not a Projects v2 board address. Expected \
                 orgs/LOGIN/projects/NUMBER or users/LOGIN/projects/NUMBER — the \
                 path of the board's own URL. Note this is NOT owner/repo: a \
                 board belongs to an organisation or a user and can span \
                 repositories, so a repository cannot name one."
            )));
        }

        let owner_kind = match parts[0] {
            "orgs" => OwnerKind::Organization,
            "users" => OwnerKind::User,
            other => {
                return Err(TrackerError::Unsupported(format!(
                    "'{other}' is not a board owner kind — a Projects v2 board is \
                     reached as orgs/LOGIN/projects/NUMBER or \
                     users/LOGIN/projects/NUMBER. The kind cannot be guessed from \
                     the login: GitHub's GraphQL schema exposes organization() \
                     and user() as different root fields."
                )));
            }
        };

        let login = parts[1].to_string();
        if login.is_empty() {
            return Err(TrackerError::Unsupported(format!(
                "'{raw}' names no owner login between {} and projects",
                owner_kind.url_segment()
            )));
        }

        let number = parts[3].parse::<u64>().map_err(|_| {
            TrackerError::Unsupported(format!(
                "'{}' is not a board number — the last segment of a Projects v2 \
                 address is the board's number on github.com, as in \
                 {}/{login}/projects/12",
                parts[3],
                owner_kind.url_segment()
            ))
        })?;

        Ok(Self {
            owner_kind,
            login,
            number,
        })
    }

    /// The address as GitHub writes it. Round-trips through [`Self::parse`].
    #[must_use]
    pub fn as_path(&self) -> String {
        format!(
            "{}/{}/projects/{}",
            self.owner_kind.url_segment(),
            self.login,
            self.number
        )
    }
}

/// A resolved board: what every later sub-chunk needs before it can do anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// The `ProjectV2` node id. The only thing the field and item mutations take.
    pub id: String,
    pub title: String,
    pub number: u64,
}

/// The Projects v2 GraphQL client.
pub struct ProjectsV2Client {
    api_base: String,
    address: BoardAddress,
    token: Option<String>,
    scheme: AuthScheme,
    http: reqwest::Client,
    budget: Mutex<RateBudget>,
    /// The resolved board, held so the whole run costs one resolution rather
    /// than one per item — the same reasoning as Linear's team schema.
    board: Mutex<Option<Board>>,
    /// The board's discovered fields, held for the same reason: a schema cannot
    /// change mid-run, and re-fetching it per item would spend the budget on an
    /// answer we already have.
    field_schema: Mutex<Option<FieldSchema>>,
    /// Board items already attached this run, keyed by the `external_id` the
    /// Issues adapter minted. THE DECISION made concrete: the item id is
    /// derived state that lives here for the length of a run, never a second
    /// identifier in the provider-agnostic link record.
    items: Mutex<HashMap<String, BoardItem>>,
    /// The W3C `traceparent` to send downstream, resolved ONCE at construction.
    /// `None` — no caller context — means no header at all.
    traceparent: Option<String>,
}

impl ProjectsV2Client {
    /// Build a client for one board.
    pub fn new(target: &str) -> Result<Self, TrackerError> {
        let address = BoardAddress::parse(target)?;
        let mut budget = RateBudget::new();
        // Declared through the public API, so adding a provider does not edit
        // the budget module.
        budget.configure(
            "github",
            Transport::GraphQl,
            GRAPHQL_POINTS_PER_MINUTE,
            WINDOW_SECS,
        );

        Ok(Self {
            api_base: DEFAULT_API_BASE.to_string(),
            address,
            token: None,
            scheme: AuthScheme::Bearer,
            http: reqwest::Client::new(),
            budget: Mutex::new(budget),
            board: Mutex::new(None),
            field_schema: Mutex::new(None),
            items: Mutex::new(HashMap::new()),
            traceparent: None,
        })
    }

    /// Adopt the project's caller trace context (SEP-414 downstream half).
    #[must_use]
    pub fn with_trace_context(self, project: &str) -> Self {
        self.with_traceparent(crate::trace_context::outbound_traceparent(project))
    }

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

    /// Supply a token and its scheme directly. Retained for tests; the product
    /// path is [`Self::with_credential`].
    #[must_use]
    pub fn with_token(mut self, token: &str, scheme: AuthScheme) -> Self {
        self.token = Some(token.to_string());
        self.scheme = scheme;
        self
    }

    /// Authenticate from the credential port — THE single wiring point for this
    /// provider's board transport. No environment variable is read here.
    #[must_use]
    pub fn with_credential(mut self, credential: &crate::tracker::credential::Credential) -> Self {
        self.token = Some(credential.secret().expose().to_string());
        self.scheme = credential.scheme();
        self
    }

    #[must_use]
    pub fn address(&self) -> &BoardAddress {
        &self.address
    }

    fn endpoint(&self) -> String {
        format!("{}/graphql", self.api_base)
    }

    /// Charge the github/GraphQl bucket for one request of this verb.
    ///
    /// Separate from [`Self::graphql`] so the accounting can be tested without a
    /// server, and so the 5x mutation weight is a fact of this function rather
    /// than of a literal at a call site.
    fn charge(&self, op: Op) -> Result<(), TrackerError> {
        let spent = self.budget.lock().unwrap_or_else(|e| e.into_inner()).spend(
            "github",
            Transport::GraphQl,
            op.points(),
        );
        match spent {
            Spend::Ok => Ok(()),
            Spend::Exhausted { retry_after } => Err(TrackerError::RateLimited {
                retry_after_secs: Some(retry_after.as_secs()),
            }),
        }
    }

    /// Points left in this client's GraphQL bucket this window.
    #[must_use]
    pub fn remaining_points(&self) -> Option<u32> {
        self.budget
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remaining("github", Transport::GraphQl)
    }

    /// The ONE network primitive. A GraphQL API has a single endpoint and a
    /// single verb, so an adapter that grows a helper per operation is importing
    /// REST habits it does not need. `op` is what it costs, not what it does.
    pub async fn graphql(
        &self,
        op: Op,
        query: &str,
        variables: Value,
    ) -> Result<Value, TrackerError> {
        self.charge(op)?;

        let mut req = self
            .http
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            // GitHub rejects an API request with no User-Agent.
            .header("User-Agent", "think-and-ship");
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
        // Transport-level rate limiting still arrives as a status code — and on
        // GitHub a SECONDARY limit arrives as 403 at least as often as 429,
        // which is why `github.rs::read` has always treated the two together.
        // This client used to check 429 alone, so a secondary limit on the
        // GraphQL endpoint became `Status { 403 }`, which `retryable()` reports
        // as false, which the projector reports as a contract rejection and
        // never queues. That is the data loss this adapter's outbox criterion
        // exists to prevent, on the exact status GitHub is most likely to send.
        //
        // The headers do the discriminating, the same way `github.rs` does it:
        // an ordinary permission 403 carries neither, and must stay a hard
        // failure rather than be retried forever against a token that will
        // never be allowed.
        if status == 429 || status == 403 {
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
    /// THE piece a REST-shaped client gets wrong: GitHub answers a failed
    /// GraphQL request with **200 OK** and an `errors` array. Reading only the
    /// status would record a write that never happened. Split out and pure so
    /// the classification is testable without a server.
    ///
    /// GitHub's classification differs from Linear's in the one way that
    /// matters: a missing owner or board arrives as `type: "NOT_FOUND"`, which
    /// must become [`TrackerError::NotFound`] rather than a generic 400 — the
    /// delivery layer treats both as non-retryable, but only one of them tells a
    /// human they pasted the wrong address.
    fn read_envelope(envelope: Value) -> Result<Value, TrackerError> {
        if let Some(errors) = envelope.get("errors").and_then(Value::as_array)
            && !errors.is_empty()
        {
            // GitHub puts the discriminator in `type`, unlike Linear's
            // `extensions.code`. Both are read: a client that knows only its
            // own provider's spelling silently degrades every error to 400.
            let kind = errors
                .iter()
                .find_map(|e| {
                    e.get("type")
                        .and_then(Value::as_str)
                        .or_else(|| e.pointer("/extensions/code").and_then(Value::as_str))
                })
                .unwrap_or_default()
                .to_ascii_uppercase();
            let message = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("; ");

            if kind.contains("RATE_LIMITED") || kind.contains("RATELIMIT") {
                return Err(TrackerError::RateLimited {
                    retry_after_secs: None,
                });
            }
            if kind.contains("NOT_FOUND") {
                return Err(TrackerError::NotFound(message));
            }
            if kind.contains("FORBIDDEN") || kind.contains("UNAUTHORIZED") {
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

    /// The query that resolves the configured address to a node id.
    ///
    /// Built rather than constant because the ROOT FIELD differs by owner kind
    /// and GraphQL has no variable for a field name. The operation name is
    /// stable (`query BoardId`) so a mock can route on it.
    fn board_query(&self) -> String {
        format!(
            "query BoardId($login: String!, $number: Int!) {{ \
               {}(login: $login) {{ projectV2(number: $number) {{ id title number }} }} }}",
            self.address.owner_kind.root_field()
        )
    }

    /// Resolve the board once per run.
    ///
    /// A board that resolves to `null` without a GraphQL error is the case worth
    /// naming: the owner exists but has no board with that number, which reads
    /// as success to anything that only checks for `errors`.
    pub async fn board(&self) -> Result<Board, TrackerError> {
        if let Ok(cached) = self.board.lock()
            && let Some(board) = cached.as_ref()
        {
            return Ok(board.clone());
        }

        let data = self
            .graphql(
                Op::Query,
                &self.board_query(),
                json!({ "login": self.address.login, "number": self.address.number }),
            )
            .await?;

        let pointer = format!("/{}/projectV2", self.address.owner_kind.root_field());
        let node = data
            .pointer(&pointer)
            .filter(|v| !v.is_null())
            .ok_or_else(|| {
                TrackerError::NotFound(format!(
                    "no Projects v2 board number {} owned by {} '{}' — looked it up as \
                 {}",
                    self.address.number,
                    match self.address.owner_kind {
                        OwnerKind::Organization => "organisation",
                        OwnerKind::User => "user",
                    },
                    self.address.login,
                    self.address.as_path()
                ))
            })?;

        let id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| TrackerError::Transport("board carried no node id".into()))?
            .to_string();
        let board = Board {
            id,
            title: node
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            number: node
                .get("number")
                .and_then(Value::as_u64)
                .unwrap_or(self.address.number),
        };

        if let Ok(mut cached) = self.board.lock() {
            *cached = Some(board.clone());
        }
        Ok(board)
    }
}

// ---------------------------------------------------------------------------
// Field discovery — the board's fields are the user's, not ours.
// ---------------------------------------------------------------------------

/// The `dataType` GitHub gives a single-select field. THE stable discriminator,
/// and the reason no field is ever found by its name here.
const DATA_TYPE_SINGLE_SELECT: &str = "SINGLE_SELECT";

/// GitHub caps every GraphQL connection page at 100 items. Asking for the
/// maximum is not an optimisation — it is what makes the second page rare
/// enough that the pagination loop is cheap, while still existing.
const FIELD_PAGE_SIZE: u32 = 100;

/// The canonical states, in the order a refusal message should list them.
const CANONICAL_STATES: [WorkItemState; 4] = [
    WorkItemState::Todo,
    WorkItemState::InProgress,
    WorkItemState::Done,
    WorkItemState::Cancelled,
];

/// The priority bands this system authors, matching `band_of` in the projector.
const CANONICAL_BANDS: [&str; 5] = ["critical", "high", "medium", "low", "later"];

/// One option on a single-select field.
///
/// The `id` is NOT a node id. GitHub mints option ids as bare 8-character hex
/// (`f75ad846`) while field ids are long and prefixed (`PVTSSF_…`), which the
/// live capture in `tests/fixtures/projects_v2_fields.json` proves. Anything
/// that prefix-checks one, or tries to resolve one through `node(id:)`, is
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    pub id: String,
    pub name: String,
}

/// One field on a board, as the board's owner defined it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardField {
    pub id: String,
    pub name: String,
    /// GitHub's own `dataType`. Stable across boards; the name is not.
    pub data_type: String,
    /// Empty for every field that is not a single-select.
    pub options: Vec<SelectOption>,
}

impl BoardField {
    #[must_use]
    pub fn is_single_select(&self) -> bool {
        self.data_type == DATA_TYPE_SINGLE_SELECT
    }

    /// The option whose name means `state` to a human, if the board has one.
    ///
    /// Name matching, and deliberately so — see [`FieldSchema`] for why there is
    /// no alternative and why this is the layer that has to be careful.
    #[must_use]
    pub fn option_for_state(&self, state: WorkItemState) -> Option<&SelectOption> {
        self.option_matching(state_synonyms(state))
    }

    /// The option meaning `band`, if the board has one.
    #[must_use]
    pub fn option_for_band(&self, band: &str) -> Option<&SelectOption> {
        self.option_matching(band_synonyms(band))
    }

    fn option_matching(&self, synonyms: &[&str]) -> Option<&SelectOption> {
        // Synonym order is preference order: an "In Progress" and a "Doing" on
        // the same board must resolve the same way on every run, so the first
        // synonym that matches anything wins rather than the first option that
        // matches anything.
        synonyms
            .iter()
            .find_map(|want| self.options.iter().find(|o| normalise(&o.name) == *want))
    }

    /// The option names as a human would read them back, for a refusal message.
    fn option_names(&self) -> String {
        if self.options.is_empty() {
            return "no options at all".to_string();
        }
        self.options
            .iter()
            .map(|o| format!("'{}'", o.name))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// One GraphQL field node, or `None` when it carries no id — see
/// [`FieldSchema::from_field_nodes`].
fn read_field(node: &Value) -> Option<BoardField> {
    Some(BoardField {
        id: node.get("id").and_then(Value::as_str)?.to_string(),
        name: node
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        data_type: node
            .get("dataType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        options: node
            .get("options")
            .and_then(Value::as_array)
            .map(|os| {
                os.iter()
                    .filter_map(|o| {
                        Some(SelectOption {
                            id: o.get("id").and_then(Value::as_str)?.to_string(),
                            name: o
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Lower-case, alphanumerics only — so `In Progress`, `in-progress` and
/// `InProgress` are one thing, while nothing else is folded together.
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The names a human might have given an option meaning this canonical state,
/// most-preferred first.
///
/// This table is the ENTIRE anti-corruption surface of field discovery,
/// because it is the only place provider vocabulary is guessed rather than
/// read. Everything
/// upstream of it — which field, which kind — comes from GitHub's own stable
/// `dataType`.
fn state_synonyms(state: WorkItemState) -> &'static [&'static str] {
    match state {
        WorkItemState::Todo => &[
            "todo", "backlog", "upnext", "ready", "new", "open", "triage",
        ],
        WorkItemState::InProgress => &[
            "inprogress",
            "doing",
            "started",
            "active",
            "wip",
            "inreview",
            "review",
        ],
        WorkItemState::Done => &[
            "done",
            "completed",
            "complete",
            "shipped",
            "closed",
            "merged",
        ],
        WorkItemState::Cancelled => &[
            "cancelled",
            "canceled",
            "wontdo",
            "wontfix",
            "notplanned",
            "abandoned",
            "dropped",
        ],
    }
}

/// The names a human might have given an option meaning this priority band.
fn band_synonyms(band: &str) -> &'static [&'static str] {
    match band {
        "critical" => &["critical", "urgent", "highest", "blocker", "p0"],
        "high" => &["high", "important", "p1"],
        "medium" => &["medium", "normal", "moderate", "p2"],
        "low" => &["low", "minor", "p3"],
        "later" => &["later", "someday", "lowest", "icebox", "p4"],
        _ => &[],
    }
}

/// A board's discovered fields, resolved once per run.
///
/// # The asymmetry this type exists to hold
///
/// The live capture settled that every FIELD carries a stable `dataType`, so
/// finding the single-selects needs no name matching at all — the same rule
/// `linear.rs` states, "types are stable, names are not", holds exactly.
///
/// It stops at the field. A `ProjectV2SingleSelectFieldOption` carries `id`,
/// `name`, `color` and `description` and nothing else: there is no type, no
/// category, no semantic marker anywhere on an option. So mapping a canonical
/// state onto an option is forced onto a human-editable string, and no amount of
/// care makes that reliable. Hence the shape of this module: match structurally
/// where GitHub gives us structure, guess by name only where it gives us
/// nothing, and REFUSE rather than guess wrong — a wrong status is corruption of
/// someone's board, and an absent one is merely an absent one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldSchema {
    fields: Vec<BoardField>,
}

impl FieldSchema {
    #[must_use]
    pub fn new(fields: Vec<BoardField>) -> Self {
        Self { fields }
    }

    /// Read one page of GraphQL field nodes.
    ///
    /// Public so a test can drive the REAL parser over the REAL captured
    /// payload rather than over a hand-built approximation of it — the whole
    /// reason `fixtures/projects_v2_fields.json` exists.
    ///
    /// A node with no `id` is not an error: the union can grow a member our
    /// fragments do not name, and GitHub answers that with an empty object. A
    /// field we cannot describe is one we must never write to, so skipping it is
    /// the conservative reading rather than a silent loss of something usable.
    #[must_use]
    pub fn from_field_nodes(nodes: &Value) -> Self {
        Self::new(
            nodes
                .as_array()
                .map(|ns| ns.iter().filter_map(read_field).collect())
                .unwrap_or_default(),
        )
    }

    #[must_use]
    pub fn fields(&self) -> &[BoardField] {
        &self.fields
    }

    /// Every single-select on the board, in board order, found by `dataType`.
    pub fn single_selects(&self) -> impl Iterator<Item = &BoardField> {
        self.fields.iter().filter(|f| f.is_single_select())
    }

    /// The single-select this board uses as a lifecycle, or `None`.
    ///
    /// A board may define several single-selects — Status, Priority, Size, a
    /// team's own taxonomy — and none of them is marked as "the status one".
    /// So the field that covers the MOST canonical states wins, ties broken by
    /// board order, and a field covering none is not a candidate at all. That is
    /// a structural argument rather than a name match: a Priority field with
    /// Critical/High/Low covers zero lifecycle states and therefore cannot be
    /// mistaken for one.
    #[must_use]
    pub fn status_field(&self) -> Option<&BoardField> {
        self.best_single_select(|f| {
            CANONICAL_STATES
                .iter()
                .filter(|s| f.option_for_state(**s).is_some())
                .count()
        })
    }

    /// The single-select this board uses for priority, or `None`.
    ///
    /// Never the status field, even if its options happen to score: one field
    /// cannot hold two meanings, and the lifecycle is the one that must win.
    #[must_use]
    pub fn band_field(&self) -> Option<&BoardField> {
        let status_id = self.status_field().map(|f| f.id.clone());
        self.best_single_select(|f| {
            if Some(&f.id) == status_id.as_ref() {
                return 0;
            }
            CANONICAL_BANDS
                .iter()
                .filter(|b| f.option_for_band(b).is_some())
                .count()
        })
    }

    /// The highest-scoring single-select, preferring the EARLIER field on a tie
    /// so the choice is stable across runs. `max_by_key` would take the later
    /// one, which is the opposite of board order.
    fn best_single_select(&self, score: impl Fn(&BoardField) -> usize) -> Option<&BoardField> {
        let mut best: Option<(usize, &BoardField)> = None;
        for field in self.single_selects() {
            let s = score(field);
            if s > 0 && best.is_none_or(|(bs, _)| s > bs) {
                best = Some((s, field));
            }
        }
        best.map(|(_, f)| f)
    }
}

/// A resolved single-select write: which field, which option.
///
/// Produced only by a resolver that already refused the impossible cases, so a
/// caller holding one of these knows the board really has somewhere to put the
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldWrite {
    pub field_id: String,
    pub option_id: String,
}

/// What to do when a board's lifecycle field cannot express a state.
///
/// DECIDED by the human, about their own board. The criterion is deliberately
/// NOT "is it Cancelled" —
/// it is **does this state's truth survive on the item without the board**,
/// which is what makes Cancelled the answer and stops a future state from
/// joining it by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unexpressible {
    /// Refuse loudly and write nothing. The state has nowhere else to live, so
    /// silence would lose it.
    Refuse,
    /// Leave the column as it is and report it once at the end of the run.
    ///
    /// Only sound because the ISSUE already carries the fact: `github.rs` writes
    /// `Cancelled` as `("closed", Some("not_planned"))` and reads it back, so
    /// nothing is lost by declining to say it a second time in a vocabulary the
    /// board does not have.
    LeaveUnchanged,
}

/// The decided policy, total over the state so adding a fifth state forces a
/// choice rather than inheriting one.
///
/// THE COST, stated here rather than buried: leaving the column alone means a
/// chunk obsoleted while *In Progress* keeps reading "In Progress" on a closed
/// issue, so a board filter on Status is wrong for exactly those items. That
/// was accepted deliberately over writing "Done", which would assert something
/// false about abandoned work in a column that is read on its own.
#[must_use]
pub fn unexpressible_policy(state: WorkItemState) -> Unexpressible {
    match state {
        // Recorded losslessly on the issue as closed/not_planned.
        WorkItemState::Cancelled => Unexpressible::LeaveUnchanged,
        // These are what a lifecycle field is FOR. A board that cannot express
        // them is misconfigured, and saying so is more use than staying quiet.
        WorkItemState::Todo | WorkItemState::InProgress | WorkItemState::Done => {
            Unexpressible::Refuse
        }
    }
}

/// Where a state landed: a real write, or a deliberate non-write.
///
/// The two used to be one `Err`, which conflated "your board is misconfigured"
/// with "this is the ordinary shape of a default board". The projector needs
/// them apart: the first should stop a run, the second should be counted and
/// summarised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusPlacement {
    /// The board can say it; here is where.
    Write(FieldWrite),
    /// The board cannot say it, and by policy that is not an error.
    LeftUnchanged(LeftUnchanged),
}

/// One item whose status was deliberately not written, carrying everything the
/// end-of-run summary needs so the caller does not have to reconstruct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftUnchanged {
    pub board: String,
    pub state: WorkItemState,
    /// The lifecycle field's real options, for the "add one yourself" advice.
    pub offered: String,
}

/// The refusal for a board with no lifecycle field at all.
///
/// A free function so `status_write_for` and `status_placement_for` cannot
/// drift into telling a human two different stories about the same board.
fn no_lifecycle_field_refusal(
    board: &str,
    state: WorkItemState,
    schema: &FieldSchema,
) -> TrackerError {
    let single_selects: Vec<&str> = schema.single_selects().map(|f| f.name.as_str()).collect();
    let saw = if single_selects.is_empty() {
        "it defines no single-select field at all".to_string()
    } else {
        format!(
            "its single-select fields ({}) carry no option resembling any of todo / in progress / \
             done / cancelled",
            single_selects.join(", ")
        )
    };
    TrackerError::Unsupported(format!(
        "board {board} has nowhere to record the state '{}': {saw}. Nothing was written. Add a \
         single-select field yourself — GitHub's default one is called Status, with Todo / In \
         Progress / Done — and run this again; no field will ever be created in your board on \
         your behalf.",
        state.as_str()
    ))
}

/// The refusal for a lifecycle field that exists but has no matching option.
fn no_option_refusal(field: &BoardField, board: &str, state: WorkItemState) -> TrackerError {
    TrackerError::Unsupported(format!(
        "the single-select '{}' on board {board} has no option meaning '{}' — it offers {}. \
         Nothing was written: an option is yours to add, not ours to create.",
        field.name,
        state.as_str(),
        field.option_names()
    ))
}

/// Fold any number of non-writes into ONE sentence — the decision's
/// "announce once per run, never once per item" made concrete.
///
/// Returns `None` for an empty slice: nothing happened, so nothing is said.
/// The staleness is named explicitly, because a human who is not told will
/// find out by filtering the board and being misled.
#[must_use]
pub fn summarise_left_unchanged(items: &[LeftUnchanged]) -> Option<String> {
    let first = items.first()?;
    Some(format!(
        "{} item(s) could not have their status recorded on board {}: it offers {} and nothing \
         meaning '{}'. Their issues ARE closed as not planned, so the outcome is not lost — but \
         their status column still shows whatever it showed before, so filtering this board by \
         status will not find them. Add a '{}' option to the lifecycle field to record it here \
         too; none was created for you.",
        items.len(),
        first.board,
        first.offered,
        first.state.as_str(),
        first.state.as_str(),
    ))
}

impl ProjectsV2Client {
    /// The fields query, paginated.
    ///
    /// `first: 100` with `pageInfo` followed to exhaustion, decided here rather
    /// than inherited: the captured board has 13 fields and so fits GitHub's own
    /// `first: 20` example, which proves nothing about a board carrying custom
    /// fields. The three inline fragments are the union's three members —
    /// `ProjectV2FieldConfiguration` is `Field | IterationField |
    /// SingleSelectField` — and this exact text is the one already proven
    /// against the real API by the live capture.
    fn fields_query() -> String {
        format!(
            "query BoardFields($id: ID!, $after: String) {{ \
               node(id: $id) {{ ... on ProjectV2 {{ \
                 fields(first: {FIELD_PAGE_SIZE}, after: $after) {{ \
                   totalCount \
                   pageInfo {{ hasNextPage endCursor }} \
                   nodes {{ \
                     ... on ProjectV2Field {{ id name dataType }} \
                     ... on ProjectV2IterationField {{ id name dataType }} \
                     ... on ProjectV2SingleSelectField {{ id name dataType options {{ id name }} }} \
                   }} }} }} }} }}"
        )
    }

    /// Discover the board's fields, once per run.
    ///
    /// Paid once for the same reason [`Self::board`] is: a schema fetched per
    /// item would spend the point budget on an answer that cannot change
    /// mid-run, and the identity layer wants it for every item it writes.
    pub async fn field_schema(&self) -> Result<FieldSchema, TrackerError> {
        if let Ok(cached) = self.field_schema.lock()
            && let Some(schema) = cached.as_ref()
        {
            return Ok(schema.clone());
        }

        let board = self.board().await?;
        let query = Self::fields_query();
        let mut fields: Vec<BoardField> = Vec::new();
        let mut after: Option<String> = None;

        loop {
            let data = self
                .graphql(Op::Query, &query, json!({ "id": board.id, "after": after }))
                .await?;
            let connection = data
                .pointer("/node/fields")
                .filter(|v| !v.is_null())
                .ok_or_else(|| {
                    TrackerError::Transport(format!(
                        "board {} answered the fields query with no field connection — \
                     the node id resolved to something that is not a ProjectV2",
                        self.address.as_path()
                    ))
                })?;

            if let Some(nodes) = connection.get("nodes") {
                fields.extend(FieldSchema::from_field_nodes(nodes).fields);
            }

            let has_next = connection
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let cursor = connection
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            // A server that says "more" but hands back the same cursor — or
            // none — would spin here forever, which is a worse failure than a
            // short read.
            match cursor {
                Some(c) if has_next && Some(&c) != after.as_ref() => after = Some(c),
                _ => break,
            }
        }

        let schema = FieldSchema::new(fields);
        if let Ok(mut cached) = self.field_schema.lock() {
            *cached = Some(schema.clone());
        }
        Ok(schema)
    }

    /// Resolve where a canonical state goes on this board, or REFUSE.
    ///
    /// This is the call that must happen BEFORE any mutation. It never creates a
    /// field and never creates an option: a board is the human's, and inventing
    /// vocabulary in it is exactly the corruption the anti-corruption layer
    /// exists to prevent. Every refusal names the board, what was wanted, and
    /// what the board actually offers, because the fix is a human's to make.
    pub async fn status_write_for(&self, state: WorkItemState) -> Result<FieldWrite, TrackerError> {
        let schema = self.field_schema().await?;

        let Some(field) = schema.status_field() else {
            return Err(no_lifecycle_field_refusal(
                &self.address.as_path(),
                state,
                &schema,
            ));
        };

        let Some(option) = field.option_for_state(state) else {
            return Err(no_option_refusal(field, &self.address.as_path(), state));
        };

        Ok(FieldWrite {
            field_id: field.id.clone(),
            option_id: option.id.clone(),
        })
    }

    /// [`status_write_for`](Self::status_write_for), but applying the decided
    /// policy for a state the board cannot express.
    ///
    /// This is the call the projector should make. The difference is only in
    /// the ONE case that a default board makes ordinary: a `Cancelled` with no
    /// option becomes `LeftUnchanged` instead of an error, to be summarised once
    /// at the end of the run rather than raised once per item.
    ///
    /// A board with no lifecycle field at all is still a hard refusal whatever
    /// the state — that is a misconfiguration, not the ordinary shape of a
    /// board, and it is worth stopping for.
    pub async fn status_placement_for(
        &self,
        state: WorkItemState,
    ) -> Result<StatusPlacement, TrackerError> {
        let schema = self.field_schema().await?;

        // The refusals are built by the same two functions `status_write_for`
        // uses, so the two paths cannot drift into saying different things
        // about the same board.
        let Some(field) = schema.status_field() else {
            return Err(no_lifecycle_field_refusal(
                &self.address.as_path(),
                state,
                &schema,
            ));
        };

        if let Some(option) = field.option_for_state(state) {
            return Ok(StatusPlacement::Write(FieldWrite {
                field_id: field.id.clone(),
                option_id: option.id.clone(),
            }));
        }

        match unexpressible_policy(state) {
            Unexpressible::Refuse => Err(no_option_refusal(field, &self.address.as_path(), state)),
            Unexpressible::LeaveUnchanged => Ok(StatusPlacement::LeftUnchanged(LeftUnchanged {
                board: self.address.as_path(),
                state,
                offered: field.option_names(),
            })),
        }
    }

    /// Resolve where a priority band goes on this board. `None` is a real
    /// answer, not a failure.
    ///
    /// The deliberate asymmetry with [`Self::status_write_for`]: a board with no
    /// priority vocabulary simply does not record priority, and that costs a
    /// reader nothing. A board with no lifecycle vocabulary cannot be given one
    /// without changing what its columns mean, so that case refuses instead.
    pub async fn band_write_for(&self, band: &str) -> Result<Option<FieldWrite>, TrackerError> {
        let schema = self.field_schema().await?;
        Ok(schema.band_field().and_then(|field| {
            field.option_for_band(band).map(|option| FieldWrite {
                field_id: field.id.clone(),
                option_id: option.id.clone(),
            })
        }))
    }
}

// ---------------------------------------------------------------------------
// Identity — one chunk, one twin per provider.
// ---------------------------------------------------------------------------

/// The GitHub issue coordinate the Issues adapter already minted:
/// `owner/repo#number`.
///
/// Parsing rather than accepting any string is what makes the DRAFT failure mode
/// unreachable. A `ProjectV2` item wraps an issue, a pull request, or a draft;
/// a draft has no issue behind it, so a chunk that acquired one would own a
/// second twin that can never be reconciled back to anything the Issues
/// adapter knows. There
/// is no draft mutation anywhere in this module and no entry point that could
/// reach one — the only way in demands a coordinate that already exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCoordinate {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl IssueCoordinate {
    /// Read back an `external_id` minted by
    /// [`GithubTracker`](crate::tracker::GithubTracker).
    ///
    /// Deliberately strict about all three parts. A title, a bare number or a
    /// `owner/repo` with no issue would each otherwise reach the API as a
    /// lookup that cannot succeed, and the refusal is more use one layer earlier
    /// where it can still name the shape that would have worked.
    pub fn parse(external_id: &str) -> Result<Self, TrackerError> {
        let refuse = || {
            TrackerError::NotFound(format!(
                "'{external_id}' is not a GitHub issue reference (expected owner/repo#number). \
                 A board item wraps an issue that already exists; no draft item is ever created, \
                 because a draft would be a second twin with no issue behind it."
            ))
        };

        let (repo_path, number) = external_id.rsplit_once('#').ok_or_else(refuse)?;
        let number: u64 = number.parse().map_err(|_| refuse())?;
        let (owner, repo) = repo_path.split_once('/').ok_or_else(refuse)?;
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return Err(refuse());
        }

        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        })
    }

    /// The form the link record holds. Round-trips through [`Self::parse`].
    #[must_use]
    pub fn as_external_id(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }
}

/// A board item that wraps a real issue.
///
/// `id` is DERIVED STATE and the whole point of the identity layer. It is a
/// GitHub-and-board-specific node id; putting it in the shared link record
/// beside `owner/repo#number` would give one chunk two GitHub twins with no rule
/// about which is authoritative when they disagree. Both sibling adapters
/// already refused the same trade — `github.rs` keeps issue database ids out of
/// `external_id`, and `linear.rs::uuid_of` states the general reason: the id a
/// human reads and the id the API needs are different, and pushing both into the
/// shared link record puts provider structure into a provider-agnostic type.
///
/// REJECTED ALTERNATIVE, with its real advantage: persisting the item id would
/// save one query point per item per run and would survive a board rename. That
/// is not nothing. It loses to the coupling above, and the point cost is bounded
/// by the same per-run cache [`ProjectsV2Client::board`] and
/// [`ProjectsV2Client::field_schema`] already use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardItem {
    /// The `ProjectV2Item` node id — what the field mutations take.
    pub id: String,
    /// The issue node id this item wraps. Never null: a draft item has none, and
    /// no path here can produce one.
    pub content_id: String,
}

/// Resolve `owner/repo#number` to the issue's node id.
///
/// The operation name is stable (`query IssueNode`) so a mock can route on it,
/// matching the convention [`ProjectsV2Client::board_query`] set.
const ISSUE_NODE_QUERY: &str = "query IssueNode($owner: String!, $repo: String!, $number: Int!) { \
     repository(owner: $owner, name: $repo) { issue(number: $number) { id } } }";

/// Attach an existing issue to the board.
///
/// `addProjectV2ItemById` is the one GitHub documents for this, and its
/// idempotency is the API's rather than ours: "If you try to add an item that
/// already exists, the existing item ID is returned instead." That claim is
/// DOCUMENTED AND UNPROBED — it comes from the same "Using the API to manage
/// Projects" page that reads as a yes on installation tokens and is wrong
/// there. It is a different kind of claim, and this page is the API's own
/// reference for it, but nothing in this repo has yet watched the real endpoint
/// do it. The per-run cache is therefore not redundant: it is the half we own.
const ADD_ITEM_MUTATION: &str = "mutation AddBoardItem($board: ID!, $content: ID!) { \
     addProjectV2ItemById(input: {projectId: $board, contentId: $content}) { item { id } } }";

/// Set one single-select field on one board item.
///
/// The verb the rest of this file never had. Everything before this could
/// RESOLVE a write —
/// [`ProjectsV2Client::status_placement_for`] hands back a [`FieldWrite`]
/// naming the field and the option — and nothing could perform one, so the
/// resolvers had no caller that spent them.
///
/// `projectId` is required alongside `itemId`, which is why this takes the
/// board rather than deriving everything from the item: an item id is only
/// meaningful inside the board that issued it, and GitHub asks for both.
///
/// Like [`ADD_ITEM_MUTATION`], this shape is DOCUMENTED AND UNPROBED — no test
/// here has watched the real endpoint accept it. What the tests below do prove
/// is the shape we send, asserted on the wire rather than on our own belief
/// about it.
const SET_FIELD_MUTATION: &str = "mutation SetBoardField($board: ID!, $item: ID!, $field: ID!, $option: String!) { \
     updateProjectV2ItemFieldValue(input: {projectId: $board, itemId: $item, fieldId: $field, \
     value: {singleSelectOptionId: $option}}) { projectV2Item { id } } }";

impl ProjectsV2Client {
    /// The board item for an issue already linked by the Issues adapter,
    /// attaching it if it is not there yet.
    ///
    /// THE ONLY WAY to obtain a [`BoardItem`], which is what makes "one chunk,
    /// one twin per provider" structural rather than a convention. It takes the
    /// `external_id` the link record already holds, so it cannot invent an
    /// identity, and it returns an item whose id the caller is expected to spend
    /// within the run and then forget.
    ///
    /// Costs one query point to resolve the issue plus five to mutate, on the
    /// first call for a given issue and never again this run.
    pub async fn attach_issue(&self, external_id: &str) -> Result<BoardItem, TrackerError> {
        // Parse FIRST: a coordinate that cannot be read must not spend a point
        // or reach the network, and this is the refusal that makes a draft item
        // unreachable rather than merely undesired.
        let coordinate = IssueCoordinate::parse(external_id)?;
        let key = coordinate.as_external_id();

        if let Ok(cached) = self.items.lock()
            && let Some(item) = cached.get(&key)
        {
            return Ok(item.clone());
        }

        let board = self.board().await?;
        let content_id = self.issue_node_id(&coordinate).await?;

        let data = self
            .graphql(
                Op::Mutation,
                ADD_ITEM_MUTATION,
                json!({ "board": board.id, "content": content_id }),
            )
            .await?;

        let id = data
            .pointer("/addProjectV2ItemById/item/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                TrackerError::Transport(format!(
                    "board {} accepted the item for {key} but returned no item id",
                    self.address.as_path()
                ))
            })?
            .to_string();

        let item = BoardItem { id, content_id };
        if let Ok(mut cached) = self.items.lock() {
            cached.insert(key, item.clone());
        }
        Ok(item)
    }

    /// The issue's node id, which is the only thing the item mutation takes.
    ///
    /// A repository or issue that does not exist arrives as `null` inside a
    /// successful envelope rather than as a GraphQL error, which is the same
    /// trap [`Self::board`] names: anything that only checks for `errors` would
    /// read it as success and then post a mutation with a null content id.
    async fn issue_node_id(&self, coordinate: &IssueCoordinate) -> Result<String, TrackerError> {
        let data = self
            .graphql(
                Op::Query,
                ISSUE_NODE_QUERY,
                json!({
                    "owner": coordinate.owner,
                    "repo": coordinate.repo,
                    "number": coordinate.number,
                }),
            )
            .await?;

        data.pointer("/repository/issue/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                TrackerError::NotFound(format!(
                    "no issue {} — nothing was added to board {}",
                    coordinate.as_external_id(),
                    self.address.as_path()
                ))
            })
    }

    /// Perform a resolved single-select write.
    ///
    /// Takes a [`FieldWrite`] rather than two strings, so the only way to reach
    /// this is through a resolver that already refused the impossible cases —
    /// a board with no lifecycle field, or a state it has no option for. There
    /// is no path here that invents a field id or an option id.
    ///
    /// Idempotent at the provider: setting the option an item already has is a
    /// no-op upstream, which is what lets the projector re-run a push without
    /// the board reading as churn.
    pub async fn write_single_select(
        &self,
        item: &BoardItem,
        write: &FieldWrite,
    ) -> Result<(), TrackerError> {
        let board = self.board().await?;
        self.graphql(
            Op::Mutation,
            SET_FIELD_MUTATION,
            json!({
                "board": board.id,
                "item": item.id,
                "field": write.field_id,
                "option": write.option_id,
            }),
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TrackerPort wiring — the board reaches the projector.
// ---------------------------------------------------------------------------

/// The provider key, in ONE place in this file — the string a human types into
/// `--provider`, the string every link record is filed under, and the answer
/// [`TrackerPort::provider`] gives. Bound once so the three cannot disagree,
/// the idiom `github.rs` established.
///
/// Deliberately NOT `github`. A board and a repository's issues are different
/// destinations with different link records: the same chunk can be mirrored to
/// both, and a shared key would make one overwrite the other's `external_id`.
pub const PROVIDER: &str = "github_projects";

/// This adapter's entry in the one registry, declared in the adapter's own
/// file. Registering it costs exactly one line in
/// [`crate::tracker::registry::PROVIDERS`].
pub const REGISTRATION: ProviderRegistration = ProviderRegistration {
    key: PROVIDER,
    build: build_registered,
};

fn build_registered(request: &ProviderBuild<'_>) -> Result<Box<dyn TrackerPort>, TrackerError> {
    let mut client = ProjectsV2Client::new(request.target)?.with_trace_context(request.project);
    if let Some(credential) = request.credential {
        client = client.with_credential(credential);
    }
    Ok(Box::new(ProjectsV2Tracker::new(client)))
}

/// A GitHub Projects v2 board, as a [`TrackerPort`].
///
/// # Why this port can only patch
///
/// Every other adapter can create its own items. This one cannot, and the
/// reason is structural rather than unfinished work: a board item wraps content
/// that already exists, and [`IssueCoordinate::parse`] is the only door onto
/// one. The alternative GitHub offers — a DRAFT item, which a board can mint
/// from nothing — carries no issue, so it could never be reconciled with the
/// `github` provider's link record for the same chunk, and the identity layer
/// made it unreachable on purpose rather than merely discouraged.
///
/// So [`Self::upsert_item`] refuses an item with no `external_id`, naming the
/// fix. The projector reports that as a rejection and continues to the next
/// chunk, which is the right shape: one chunk that has not reached GitHub
/// Issues yet must not cost the board the chunks that have.
///
/// # What it does not do
///
/// It reports no labels, no assignee and no blocking links, because a board has
/// none of those. In particular it does NOT claim labels in order to receive
/// the `roadmap:<band>` label the projector would then send: that would buy
/// [`ProjectsV2Client::band_write_for`] a caller at the cost of a false answer
/// in the capability contract, and the contract is what every other degradation
/// in the projector is decided from.
pub struct ProjectsV2Tracker {
    client: ProjectsV2Client,
    /// States this run declined to write, accumulated for ONE end-of-run
    /// sentence. Per-item warnings were the thing the decision
    /// specifically rejected: a board of obsoleted chunks would otherwise emit
    /// the same paragraph once per chunk.
    left_unchanged: Mutex<Vec<LeftUnchanged>>,
}

/// The refusal for a chunk the board cannot file because nothing has minted its
/// issue yet.
///
/// A free function for the same reason `no_lifecycle_field_refusal` is one:
/// there is exactly one wording, so no second path can tell a human a different
/// story about the same situation.
fn no_issue_to_file_refusal(board: &str, title: &str) -> TrackerError {
    TrackerError::Unsupported(format!(
        "'{title}' has no GitHub issue yet, and a board files issues that already exist rather \
         than creating them. Mirror this roadmap to GitHub Issues first — `tracker on --provider \
         github --into <owner>/<repo>` then `tracker push` — and the board will pick it up on the \
         next push. Nothing was written to board {board}."
    ))
}

impl ProjectsV2Tracker {
    /// Wrap a configured client.
    #[must_use]
    pub fn new(client: ProjectsV2Client) -> Self {
        Self {
            client,
            left_unchanged: Mutex::new(Vec::new()),
        }
    }

    /// The run's declined writes as ONE sentence, DRAINING the accumulator.
    ///
    /// Draining is what makes "announced once per run" structural rather than a
    /// convention: a second call returns `None` because there is nothing left
    /// to say, so no arrangement of callers can double-report the staleness.
    /// [`Drop`] calls this, which is why the announcement happens exactly once
    /// even for a caller that never asks — the adapter is built per run by the
    /// registry and dropped when the run ends.
    pub fn take_left_unchanged_summary(&self) -> Option<String> {
        let drained: Vec<LeftUnchanged> = match self.left_unchanged.lock() {
            Ok(mut held) => std::mem::take(&mut *held),
            Err(_) => return None,
        };
        summarise_left_unchanged(&drained)
    }

    fn note_left_unchanged(&self, item: LeftUnchanged) {
        if let Ok(mut held) = self.left_unchanged.lock() {
            held.push(item);
        }
    }
}

impl Drop for ProjectsV2Tracker {
    fn drop(&mut self) {
        if let Some(summary) = self.take_left_unchanged_summary() {
            tracing::warn!(target: "think_and_ship::tracker", "{summary}");
        }
    }
}

#[async_trait]
impl TrackerPort for ProjectsV2Tracker {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn capabilities(&self) -> TrackerCapabilities {
        TrackerCapabilities {
            blocking_links: false,
            labels: false,
            assignee: false,
            max_body_len: None,
            required_fields: Vec::new(),
        }
    }

    async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
        let Some(external_id) = item.external_id.as_deref() else {
            return Err(no_issue_to_file_refusal(
                &self.client.address().as_path(),
                &item.title,
            ));
        };

        // ATTACH FIRST: the field mutation takes the item id, which only
        // attaching can produce. The order is the contract, not a preference.
        let board_item = self.client.attach_issue(external_id).await?;

        // `status_placement_for`, never `status_write_for`. The difference is
        // the whole of the decision: a `Cancelled` a default board cannot express
        // is the ORDINARY shape of a board rather than a misconfiguration, and
        // it is counted here instead of stopping the run.
        match self.client.status_placement_for(item.state).await? {
            StatusPlacement::Write(write) => {
                self.client.write_single_select(&board_item, &write).await?;
            }
            StatusPlacement::LeftUnchanged(left) => self.note_left_unchanged(left),
        }

        Ok(UpsertOutcome {
            external_id: external_id.to_string(),
            // The board never mints an identity, and `created` here would be a
            // claim we cannot support even if it did: `attach_issue` cannot
            // distinguish a first attach from a per-run cache hit from
            // GitHub's documented-and-unprobed "the existing item id is
            // returned instead". `false` is the only honest answer.
            created: false,
            // A board item carries no concurrency token, so there is nothing to
            // remember. `None` keeps the projector's fence comparing our own
            // content hash rather than a version that would never move.
            version: None,
        })
    }

    async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError> {
        let _ = since;
        Err(TrackerError::Unsupported(format!(
            "board {} cannot be asked what changed since a timestamp — a ProjectV2 item has no \
             updated-at the API filters on. The issues themselves answer this through the github \
             provider.",
            self.client.address().as_path()
        )))
    }

    /// `Ok(None)`, always — and deliberately, rather than inheriting the
    /// default that filters a from-the-beginning [`Self::fetch_since`].
    ///
    /// The board holds no field this system authors: the title, body, labels
    /// and assignee all live on the issue, and the one column written here is
    /// derived from the chunk's own status. So there is nothing for the
    /// ownership policy to reconcile against, and answering "I hold no prior
    /// version of this" is true. The default would instead surface the
    /// `fetch_since` refusal above into a path the projector swallows with
    /// `unwrap_or(None)`, reaching the same answer by way of an error nobody
    /// reads.
    async fn fetch_one(&self, external_id: &str) -> Result<Option<WorkItem>, TrackerError> {
        let _ = external_id;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn select(name: &str, options: &[(&str, &str)]) -> BoardField {
        BoardField {
            id: format!("PVTSSF_{name}"),
            name: name.to_string(),
            data_type: DATA_TYPE_SINGLE_SELECT.to_string(),
            options: options
                .iter()
                .map(|(id, n)| SelectOption {
                    id: (*id).to_string(),
                    name: (*n).to_string(),
                })
                .collect(),
        }
    }

    fn plain(name: &str, data_type: &str) -> BoardField {
        BoardField {
            id: format!("PVTF_{name}"),
            name: name.to_string(),
            data_type: data_type.to_string(),
            options: Vec::new(),
        }
    }

    #[test]
    fn an_org_url_and_its_bare_path_parse_the_same() {
        let from_url = BoardAddress::parse("https://github.com/orgs/acme/projects/12").unwrap();
        let from_path = BoardAddress::parse("orgs/acme/projects/12").unwrap();
        assert_eq!(from_url, from_path);
        assert_eq!(from_url.owner_kind, OwnerKind::Organization);
        assert_eq!(from_url.login, "acme");
        assert_eq!(from_url.number, 12);
        assert_eq!(from_url.as_path(), "orgs/acme/projects/12");
    }

    #[test]
    fn a_user_board_resolves_to_the_user_root_field() {
        let addr = BoardAddress::parse("https://github.com/users/alrik/projects/3").unwrap();
        assert_eq!(addr.owner_kind, OwnerKind::User);
        assert_eq!(addr.owner_kind.root_field(), "user");
        assert_eq!(addr.login, "alrik");
        assert_eq!(addr.number, 3);
    }

    /// The two root fields must not be confusable — this is the whole reason the
    /// address carries the kind.
    #[test]
    fn the_owner_kinds_use_different_root_fields() {
        assert_ne!(
            OwnerKind::Organization.root_field(),
            OwnerKind::User.root_field()
        );
        assert_eq!(OwnerKind::Organization.root_field(), "organization");
    }

    /// owner/repo is the address every OTHER GitHub thing here uses, so it is
    /// the mistake most likely to be made, and it must not parse.
    #[test]
    fn owner_repo_is_refused_because_a_repository_cannot_own_a_board() {
        let err = BoardAddress::parse("acme/think-and-ship").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("orgs/LOGIN/projects/NUMBER"),
            "the refusal must show the shape that would work, got: {msg}"
        );
        assert!(
            msg.contains("NOT owner/repo"),
            "the refusal must name the confusion it is resolving, got: {msg}"
        );
    }

    /// The classic-vs-v2 trap, named rather than lumped into a generic parse
    /// error: a repository-scoped projects URL is a different PRODUCT.
    #[test]
    fn a_classic_repository_scoped_projects_url_says_so() {
        let err = BoardAddress::parse("https://github.com/acme/repo/projects/1").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("classic"),
            "a repo-scoped projects URL must be named as Projects (classic), got: {msg}"
        );
        assert!(
            msg.contains("orgs/acme/projects/12"),
            "the refusal must suggest the v2 shape for the same owner, got: {msg}"
        );
    }

    #[test]
    fn a_non_numeric_board_number_is_refused() {
        let err = BoardAddress::parse("orgs/acme/projects/board").unwrap_err();
        assert!(err.to_string().contains("not a board number"));
    }

    #[test]
    fn an_empty_target_is_refused_with_the_shape_that_would_work() {
        let err = BoardAddress::parse("   ").unwrap_err();
        assert!(err.to_string().contains("orgs/ACME/projects/12"));
    }

    /// THE weight that separates this client from the Linear one. A flat charge
    /// of 1 would under-count every write by 5x against a limit GitHub enforces.
    #[test]
    fn a_mutation_costs_five_points_and_a_query_costs_one() {
        assert_eq!(Op::Query.points(), 1);
        assert_eq!(Op::Mutation.points(), 5);
    }

    /// The charge must come from the github/GraphQl bucket specifically, and
    /// exhausting it must not touch the REST bucket the Issues adapter spends.
    #[test]
    fn charging_spends_the_graphql_bucket_and_leaves_rest_alone() {
        let client = ProjectsV2Client::new("orgs/acme/projects/12").unwrap();
        assert_eq!(client.remaining_points(), Some(2_000));

        client.charge(Op::Mutation).unwrap();
        assert_eq!(
            client.remaining_points(),
            Some(1_995),
            "a mutation must cost 5 points, not 1"
        );

        client.charge(Op::Query).unwrap();
        assert_eq!(client.remaining_points(), Some(1_994));

        // The REST bucket is a different key entirely and this client never
        // configured it, so it must read as unlimited rather than as spent.
        let budget = client.budget.lock().unwrap();
        assert_eq!(
            budget.remaining("github", Transport::Rest),
            None,
            "the board client must not have touched GitHub's REST bucket"
        );
    }

    /// Exhaustion must be refused as RateLimited with a retry hint, not as a
    /// generic failure the delivery layer would treat as a contract rejection.
    #[test]
    fn an_exhausted_bucket_refuses_with_a_retry_hint() {
        let client = ProjectsV2Client::new("orgs/acme/projects/12").unwrap();
        for _ in 0..400 {
            client.charge(Op::Mutation).unwrap();
        }
        assert_eq!(client.remaining_points(), Some(0));
        let err = client.charge(Op::Query).unwrap_err();
        assert!(
            matches!(
                err,
                TrackerError::RateLimited {
                    retry_after_secs: Some(60)
                }
            ),
            "expected a rate-limit refusal carrying the window, got {err:?}"
        );
        assert!(err.retryable(), "a rate limit must queue, never be dropped");
    }

    /// A 200 OK carrying errors is a FAILURE. This is the single most important
    /// behaviour in the module: the status code says nothing.
    #[test]
    fn a_two_hundred_carrying_errors_is_not_success() {
        let envelope = json!({
            "data": null,
            "errors": [{ "type": "FORBIDDEN", "message": "Resource not accessible" }]
        });
        let err = ProjectsV2Client::read_envelope(envelope).unwrap_err();
        assert!(
            matches!(err, TrackerError::Status { status: 401, .. }),
            "a FORBIDDEN envelope must classify as auth, got {err:?}"
        );
    }

    /// GitHub spells the discriminator `type`, Linear spells it
    /// `extensions.code`. Reading only one degrades every error to a generic 400.
    #[test]
    fn a_not_found_type_becomes_not_found_not_a_generic_four_hundred() {
        let envelope = json!({
            "errors": [{
                "type": "NOT_FOUND",
                "message": "Could not resolve to an Organization with the login of 'nope'."
            }]
        });
        let err = ProjectsV2Client::read_envelope(envelope).unwrap_err();
        match err {
            TrackerError::NotFound(msg) => assert!(msg.contains("nope")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_rate_limited_envelope_is_retryable() {
        let envelope = json!({
            "errors": [{ "type": "RATE_LIMITED", "message": "API rate limit exceeded" }]
        });
        let err = ProjectsV2Client::read_envelope(envelope).unwrap_err();
        assert!(matches!(err, TrackerError::RateLimited { .. }));
        assert!(err.retryable());
    }

    #[test]
    fn a_clean_envelope_yields_its_data() {
        let envelope = json!({ "data": { "organization": { "projectV2": { "id": "PVT_1" } } } });
        let data = ProjectsV2Client::read_envelope(envelope).unwrap();
        assert_eq!(
            data.pointer("/organization/projectV2/id").unwrap(),
            &json!("PVT_1")
        );
    }

    /// An envelope with neither data nor errors is a transport fault, not an
    /// empty success — returning `Ok(null)` here would let a caller read a
    /// missing board as an absent one.
    #[test]
    fn an_envelope_with_no_data_is_a_transport_error() {
        let err = ProjectsV2Client::read_envelope(json!({})).unwrap_err();
        assert!(matches!(err, TrackerError::Transport(_)));
    }

    /// The query must name the root field the ADDRESS chose, not a fixed one.
    #[test]
    fn the_board_query_follows_the_owner_kind() {
        let org = ProjectsV2Client::new("orgs/acme/projects/12").unwrap();
        let q = org.board_query();
        assert!(q.contains("organization(login: $login)"), "got: {q}");
        assert!(!q.contains("user(login:"), "got: {q}");

        let user = ProjectsV2Client::new("users/alrik/projects/3").unwrap();
        let q = user.board_query();
        assert!(q.contains("user(login: $login)"), "got: {q}");
        assert!(!q.contains("organization(login:"), "got: {q}");
    }

    /// Construction must refuse a bad address before any network object exists,
    /// so a typo cannot become a request.
    #[test]
    fn a_client_cannot_be_built_for_a_repository() {
        assert!(ProjectsV2Client::new("acme/think-and-ship").is_err());
    }

    // -- the unexpressible-state policy --------------------------------------

    /// The DECISION, pinned as a whole-enum sweep rather than one spot check:
    /// across every canonical state, EXACTLY ONE is left unchanged and it is
    /// Cancelled. A contains-check would let a second state join it silently.
    #[test]
    fn exactly_one_canonical_state_is_left_unchanged_and_it_is_cancelled() {
        let left: Vec<WorkItemState> = CANONICAL_STATES
            .into_iter()
            .filter(|s| unexpressible_policy(*s) == Unexpressible::LeaveUnchanged)
            .collect();
        assert_eq!(
            left,
            vec![WorkItemState::Cancelled],
            "only Cancelled survives on the issue as closed/not_planned; anything else \
             left unchanged would go unrecorded entirely"
        );
    }

    /// The other half of the same decision: the states a lifecycle field exists
    /// FOR must still refuse loudly, because nothing else records them.
    #[test]
    fn the_states_a_board_is_for_still_refuse() {
        for state in [
            WorkItemState::Todo,
            WorkItemState::InProgress,
            WorkItemState::Done,
        ] {
            assert_eq!(
                unexpressible_policy(state),
                Unexpressible::Refuse,
                "{state:?} has nowhere else to live, so silence would lose it"
            );
        }
    }

    /// "Announce ONCE per run, never once per item" — many items collapse to
    /// one sentence, and that sentence must name the cost the decision accepted
    /// rather than bury it.
    #[test]
    fn many_non_writes_become_one_sentence_that_admits_the_staleness() {
        let item = |state| LeftUnchanged {
            board: "users/AlrikOlson/projects/1".to_string(),
            state,
            offered: "'Todo', 'In Progress', 'Done'".to_string(),
        };
        let items = vec![
            item(WorkItemState::Cancelled),
            item(WorkItemState::Cancelled),
            item(WorkItemState::Cancelled),
        ];

        let summary = summarise_left_unchanged(&items).expect("three items must be reported");
        assert!(summary.starts_with('3'), "count the items: {summary}");
        assert!(
            summary.contains("users/AlrikOlson/projects/1"),
            "name the board: {summary}"
        );
        assert!(
            summary.contains("'Todo', 'In Progress', 'Done'"),
            "name what the board does offer: {summary}"
        );
        // The load-bearing half: the decision accepted a stale column, so the
        // human has to be told rather than discover it by filtering.
        assert!(
            summary.contains("filtering this board by status will not find them"),
            "the summary must admit the staleness: {summary}"
        );
        assert!(
            summary.contains("closed as not planned"),
            "the summary must say where the outcome DID survive: {summary}"
        );
        assert!(
            summary.contains("none was created for you"),
            "the summary must promise nothing was invented: {summary}"
        );
    }

    /// Nothing happened, so nothing is said — a run with no obsoleted chunks
    /// must not print a summary about zero of them.
    #[test]
    fn no_non_writes_produces_no_sentence_at_all() {
        assert_eq!(summarise_left_unchanged(&[]), None);
    }

    // -- which credential a board will accept ---------------------------------

    /// THE finding, pinned in the direction that costs money to get wrong: a
    /// user-owned board is NOT reachable by a GitHub App installation token,
    /// and the credential it does need is a user token.
    ///
    /// Asserted against the DOGFOOD board's real address rather than a
    /// hand-built enum, so the pin covers the parse too — that path is the one
    /// the projector wiring will actually hold.
    #[test]
    fn a_user_owned_board_is_out_of_reach_of_a_github_app_installation() {
        let board = BoardAddress::parse("users/AlrikOlson/projects/1").unwrap();
        assert_eq!(board.owner_kind, OwnerKind::User);

        let credential = board.owner_kind.required_credential();
        assert_eq!(credential, BoardCredential::UserToken);
        assert!(
            !credential.reachable_by_github_app_installation(),
            "the App lane cannot serve a user-owned board; a user token is the only lane that reaches one"
        );
    }

    /// The other half, present so the test above is a real distinction and not
    /// a function that returns the same answer for everything.
    #[test]
    fn an_org_owned_board_is_exactly_what_the_installation_token_is_for() {
        let board = BoardAddress::parse("orgs/acme/projects/12").unwrap();
        assert_eq!(board.owner_kind, OwnerKind::Organization);

        let credential = board.owner_kind.required_credential();
        assert_eq!(credential, BoardCredential::InstallationToken);
        assert!(credential.reachable_by_github_app_installation());
    }

    /// The two grants come from DIFFERENT vocabularies — an App permission key
    /// and an OAuth scope — and the whole failure mode this records is someone
    /// treating them as one thing. Pin the exact strings so a rename has to be
    /// a deliberate diff.
    #[test]
    fn the_two_lanes_ask_for_different_grants_in_different_vocabularies() {
        assert_eq!(
            BoardCredential::InstallationToken.required_grant(),
            "organization_projects"
        );
        assert_eq!(BoardCredential::UserToken.required_grant(), "project");
        assert_ne!(
            BoardCredential::InstallationToken.required_grant(),
            BoardCredential::UserToken.required_grant()
        );
    }

    /// The cross-module tie: the permission the org lane says it needs must be
    /// one the manifest actually REQUESTS. Without this, a rename on either
    /// side leaves both files internally consistent and jointly wrong.
    #[test]
    fn the_org_lane_names_a_permission_the_manifest_really_asks_for() {
        use crate::tracker::credential::github_app::DEFAULT_PERMISSIONS;

        let needed = BoardCredential::InstallationToken.required_grant();
        let granted = DEFAULT_PERMISSIONS
            .iter()
            .find(|p| p.key == needed)
            .unwrap_or_else(|| panic!("the manifest does not request `{needed}`"));
        assert_eq!(
            granted.level, "write",
            "the projector writes to the board, so read is not enough"
        );
    }

    // -- field discovery, and what it refuses --------------------------------

    /// THE load-bearing rule here. A board that calls its lifecycle
    /// something else entirely must still be discovered, because the FIELD
    /// carries a stable `dataType` even though its name is the owner's.
    #[test]
    fn the_status_field_is_found_by_datatype_even_when_it_is_not_called_status() {
        let schema = FieldSchema::new(vec![
            plain("Title", "TITLE"),
            select(
                "Où en est-on",
                &[
                    ("f75ad846", "À faire"),
                    ("47fc9ee4", "Doing"),
                    ("98236657", "Shipped"),
                ],
            ),
        ]);

        let field = schema
            .status_field()
            .expect("a single-select must be discoverable without matching its name");
        assert_eq!(field.name, "Où en est-on");
        assert_eq!(
            field
                .option_for_state(WorkItemState::InProgress)
                .unwrap()
                .id,
            "47fc9ee4"
        );
        assert_eq!(
            field.option_for_state(WorkItemState::Done).unwrap().id,
            "98236657"
        );
        // "À faire" is French for Todo and we do not translate — an unmatched
        // state is an honest None, not a wrong guess.
        assert!(field.option_for_state(WorkItemState::Todo).is_none());
    }

    /// A board with several single-selects has no marker saying which is the
    /// lifecycle, so the choice must be structural: a Priority field covers zero
    /// canonical states and therefore can never be mistaken for one.
    #[test]
    fn a_priority_single_select_is_never_mistaken_for_the_lifecycle() {
        let schema = FieldSchema::new(vec![
            select(
                "Priority",
                &[("aa", "Critical"), ("bb", "High"), ("cc", "Low")],
            ),
            select(
                "Stage",
                &[("dd", "Todo"), ("ee", "In Progress"), ("ff", "Done")],
            ),
        ]);

        assert_eq!(schema.status_field().unwrap().name, "Stage");
        assert_eq!(schema.band_field().unwrap().name, "Priority");
        assert_eq!(
            schema
                .band_field()
                .unwrap()
                .option_for_band("high")
                .unwrap()
                .id,
            "bb"
        );
    }

    /// One field cannot hold two meanings. When the same single-select scores on
    /// both, the lifecycle wins and priority goes unrecorded — writing a band
    /// into the status column would be exactly the corruption this refuses.
    #[test]
    fn the_lifecycle_field_is_never_also_used_for_the_priority_band() {
        let schema = FieldSchema::new(vec![select(
            "Status",
            &[("aa", "Todo"), ("bb", "Done"), ("cc", "Low")],
        )]);

        assert_eq!(schema.status_field().unwrap().name, "Status");
        assert!(
            schema.band_field().is_none(),
            "the only single-select is already the lifecycle; priority must go unrecorded"
        );
    }

    /// Ties go to board order, so the same board resolves the same way on every
    /// run. `max_by_key` takes the LAST maximum, which would be the opposite.
    #[test]
    fn a_tie_between_single_selects_resolves_to_the_earlier_field() {
        let first = select("First", &[("a", "Todo"), ("b", "Done")]);
        let second = select("Second", &[("c", "Todo"), ("d", "Done")]);
        let schema = FieldSchema::new(vec![first, second]);
        assert_eq!(schema.status_field().unwrap().name, "First");
    }

    /// Spelling is the owner's business: `In Progress`, `in-progress` and
    /// `InProgress` are one option to a human and must be one to us.
    #[test]
    fn option_names_are_matched_past_case_and_punctuation() {
        for spelling in ["In Progress", "in-progress", "InProgress", "IN_PROGRESS"] {
            let field = select("S", &[("x", spelling)]);
            assert!(
                field.option_for_state(WorkItemState::InProgress).is_some(),
                "{spelling:?} must read as in progress"
            );
        }
    }

    /// Synonym order is preference order, so a board offering two acceptable
    /// options lands on the same one every run rather than on whichever the
    /// owner happened to list first.
    #[test]
    fn a_board_offering_two_acceptable_options_picks_the_preferred_one() {
        let field = select("S", &[("a", "Doing"), ("b", "In Progress")]);
        assert_eq!(
            field
                .option_for_state(WorkItemState::InProgress)
                .unwrap()
                .id,
            "b",
            "'In Progress' outranks 'Doing' regardless of the board's ordering"
        );
    }

    /// A field with no analogue must produce a refusal a human can act on: it
    /// has to name the board, the state, and what the board actually offers.
    #[test]
    fn a_refusal_names_the_options_the_board_actually_has() {
        let field = select("Stage", &[("a", "Todo"), ("b", "Done")]);
        assert!(field.option_for_state(WorkItemState::Cancelled).is_none());
        let names = field.option_names();
        assert!(
            names.contains("'Todo'") && names.contains("'Done'"),
            "got: {names}"
        );
    }

    /// Non-single-select fields must never be considered — an ITERATION field
    /// has no options at all and a TEXT field's value is free-form.
    #[test]
    fn only_single_selects_are_candidates() {
        let schema = FieldSchema::new(vec![
            plain("Iteration", "ITERATION"),
            plain("Status", "TEXT"),
        ]);
        assert!(schema.status_field().is_none());
        assert_eq!(schema.single_selects().count(), 0);
    }

    /// The query must ask for the connection maximum and carry the cursor —
    /// pagination decided rather than inherited from one board's luck.
    #[test]
    fn the_fields_query_paginates_explicitly() {
        let q = ProjectsV2Client::fields_query();
        assert!(q.contains("fields(first: 100, after: $after)"), "got: {q}");
        assert!(q.contains("hasNextPage"), "got: {q}");
        assert!(q.contains("endCursor"), "got: {q}");
        assert!(
            q.contains("... on ProjectV2SingleSelectField") && q.contains("options { id name }"),
            "the single-select fragment must ask for its option ids: {q}"
        );
    }

    /// A union member our fragments do not name arrives as an empty object. A
    /// field we cannot describe must be skipped, never written to.
    #[test]
    fn an_undescribable_field_node_is_skipped_rather_than_half_read() {
        let nodes = json!([
            {},
            { "id": "PVTF_1", "name": "Title", "dataType": "TITLE" }
        ]);
        let schema = FieldSchema::from_field_nodes(&nodes);
        assert_eq!(schema.fields().len(), 1);
        assert_eq!(schema.fields()[0].id, "PVTF_1");
    }

    /// The coordinate the Issues adapter minted is read back whole and
    /// round-trips, so the board lane and the issue lane cannot drift into
    /// naming different things.
    #[test]
    fn the_external_id_the_issues_adapter_minted_round_trips_through_the_coordinate() {
        let coordinate = IssueCoordinate::parse("acme/widgets#42").expect("a real external_id");
        assert_eq!(coordinate.owner, "acme");
        assert_eq!(coordinate.repo, "widgets");
        assert_eq!(coordinate.number, 42);
        assert_eq!(coordinate.as_external_id(), "acme/widgets#42");
    }

    /// EVERY shape that is not a coordinate is refused, and the refusal names
    /// both the form that would have worked and the reason — a draft item would
    /// be a second twin with no issue behind it, which is the failure mode this
    /// parse exists to make unreachable.
    #[test]
    fn anything_that_is_not_an_issue_coordinate_is_refused_with_the_shape_and_the_reason() {
        for input in [
            "A title — not a coordinate",
            "42",
            "acme/widgets",
            "acme#42",
            "acme/widgets#not-a-number",
            "acme/widgets/extra#42",
            "/widgets#42",
            "acme/#42",
        ] {
            let err = IssueCoordinate::parse(input)
                .expect_err("{input} is not owner/repo#number and must be refused");
            match &err {
                TrackerError::NotFound(msg) => {
                    assert!(
                        msg.contains("owner/repo#number"),
                        "the refusal must name the shape that works, for {input}: {msg}"
                    );
                    assert!(
                        msg.contains("draft"),
                        "the refusal must say WHY, not only what, for {input}: {msg}"
                    );
                }
                other => panic!("expected NotFound for {input}, got {other:?}"),
            }
        }
    }
}
