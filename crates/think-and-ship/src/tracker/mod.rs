//! The tracker port — how a roadmap chunk reaches an external issue tracker.
//!
//! # What this module is for
//!
//! A provider-agnostic PORT with one adapter per destination, reached from the
//! `tracker_*` namespace of the `roadmap_*` tool family. It is not a family of
//! its own and its tools are not a fifth prefix bolted on: the state it moves —
//! which chunks are opted in, and what each one is bound to upstream — is
//! roadmap state, so the family that owns the plan claims the namespace that
//! mirrors it. See [`crate::mcp::unified::Family::prefixes`].
//!
//! The adapters actually registered are `github` (Issues), `github_projects`
//! (Projects v2) and `linear`; Jira is scaffolded — its body format in
//! [`adf`] and its credentials in [`credential::atlassian`] — but has no
//! registration, so `--provider jira` is refused rather than half-served.
//! [`registry::PROVIDERS`] is the truth, and the gate on this paragraph reads
//! that table rather than trusting the sentence.
//!
//! Linear, Jira and GitHub Projects are where most teams' work actually lives.
//! This module is the boundary through which a roadmap chunk is *projected* into
//! one of them and reconciled back — carrying the thing that makes this system
//! different from a generic sync tool: a chunk's acceptance criteria, its
//! dependencies, and its cross-references into the `think_*` reasoning and
//! `ship_*` proof that produced it.
//!
//! It is emphatically NOT an issue-CRUD surface. Linear, Atlassian and GitHub
//! each ship an official MCP server that creates and updates issues, and they do
//! it better than a third party could. Re-implementing that would spend the
//! whole effort on commodity. What no vendor can offer is the provenance behind
//! a plan, because they never see the reasoning.
//!
//! # Layering
//!
//! ```text
//!   domain   pure data: WorkItem, WorkItemState, TrackerCapabilities, ExternalRef
//!   port     the TrackerPort trait + its error and outcome types
//!   echo     is an inbound item our own write coming back? (pure)
//!   ownership who wins when both sides edited the same field (a table, not branches)
//!   sweep    the backstop: what changed since we last looked? (reports, never applies)
//!   concern  divergence becomes a signal a human will actually see (caller opts in)
//!   fake     an in-memory implementation, the only one this seam ships
//!   adf      Jira's body language: our Markdown body as an ADF tree (pure)
//!   projects_v2  GitHub Projects v2's board transport: GraphQL-only, owner-scoped
//!   registry the one place a provider key is bound to a constructor
//!   seed     giving a patch-only lane its first identities, naming no provider
//! ```
//!
//! The dependency direction is one-way FOR THE PROVIDER-FACING LAYER: `domain`,
//! `port` and every adapter know nothing about `roadmap`, so an adapter depends
//! on the port alone and never on the plan it happens to be serving — which is
//! what keeps "add a provider" from meaning "edit the core". The reciprocal
//! binding, [`TrackerLink`], lives with the roadmap because it is roadmap state:
//! a chunk's record of its twin.
//!
//! The BRIDGE layer is deliberately not one-way, and this comment used to claim
//! otherwise. `project`, `sweep` and `echo` read roadmap state, and `concern`
//! additionally writes signals — that is their job, and pretending the whole
//! module is provider-only made the rule sound stronger than it is. The rule
//! that actually holds: nothing an adapter can see knows about the plan.
//!
//! [`TrackerLink`]: crate::roadmap::domain::TrackerLink

pub mod adf;
pub mod budget;
pub mod concern;
pub mod config;
pub mod credential;
pub mod domain;
pub mod echo;
pub mod fake;
pub mod github;
pub mod linear;
pub mod outbox;
pub mod ownership;
pub mod port;
pub mod project;
pub mod projects_v2;
pub mod propose_consent;
pub mod receipt;
pub mod registry;
pub mod seed;
pub mod sweep;

pub use adf::{ADF_VERSION, plain_text, render_body};
pub use budget::{RateBudget, Spend, Transport};
pub use concern::{emit_divergence_concerns, propose_status_from_sweep, propose_titles_from_sweep};
pub use config::{CompanionLane, TrackerConfig, companion_lane, inherited_opt_in, should_project};
pub use credential::{Credential, CredentialPort, CredentialStore, Resolver};
pub use domain::{
    ExternalRef, GroupState, TrackerCapabilities, WorkGroup, WorkItem, WorkItemState,
};
pub use echo::{Verdict, classify};
pub use fake::FakeTracker;
pub use github::GithubTracker;
pub use linear::LinearTracker;
pub use outbox::TrackerOutbox;
pub use ownership::{Divergence, Field, Owner, Ownership, Reconciled, reconcile_fields};
pub use port::{TargetInfo, TrackerError, TrackerPort, UpsertOutcome};
pub use project::{ProjectionOutcome, ProjectionReport, project_all};
pub use projects_v2::{Board, BoardAddress, Op, OwnerKind, ProjectsV2Client};
pub use registry::{PROVIDERS, ProviderBuild, ProviderRegistration, RegistryError};
pub use seed::{SeedReport, seed_links_from};
pub use sweep::{SweepReport, Watermarks, reconcile};
