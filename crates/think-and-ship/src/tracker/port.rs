//! The port: the only surface a tracker provider must implement.
//!
//! # The shape, and why it is this small
//!
//! Ports and Adapters. [`TrackerPort`] declares what the *projector* needs; each
//! provider supplies an adapter behind it. The value of the arrangement is
//! entirely in the narrowness of this trait — every verb added here becomes an
//! obligation for Linear, Jira, GitHub Issues, Projects v2 and everything after
//! them, so a verb that only one provider needs belongs in that adapter, not
//! here.
//!
//! Identify yourself, say what you can do, write, read what changed, and relate.
//! Notably absent are `delete`, `comment`, `list_projects`, `search` and every
//! other thing an issue-tracker API offers, because the projector never needs
//! them — and because this system is not trying to be an issue-CRUD surface. The
//! vendors ship their own MCP servers for that and do it better; what this seam
//! carries is the roadmap's plan and its provenance.
//!
//! [`TrackerPort::relate_items`] was added only after the seam met a real
//! projector. The original shape had no way to express "this is blocked by
//! that": [`WorkItem`] has no dep field, and adding one fails on ordering (a
//! blocking relation cannot be written before its target exists) and on hashing
//! (relations are resolved identity, not authored content). It carries a default
//! that refuses, so no existing implementor had to change — which is the test of
//! whether a port is actually extensible.
//!
//! # Dyn-compatibility
//!
//! The trait is `#[async_trait]` rather than using native `async fn` in traits.
//! Native AFIT (stable since Rust 1.75) is still not dyn-compatible — see
//! rust-lang/rust#133119 — and dynamic dispatch is the whole point: a registry
//! of `Box<dyn TrackerPort>` is what makes "adding a provider adds a file and
//! edits nothing" true. Static dispatch over an enum of providers would put the
//! provider list back in the core, which is the coupling this module exists to
//! remove.

use async_trait::async_trait;

use crate::infra::ExternalId;
use crate::tracker::domain::{TrackerCapabilities, WorkGroup, WorkItem};

/// What a write to a provider produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub external_id: ExternalId,
    /// The provider's post-write concurrency token, when it returns one.
    pub version: Option<String>,
    /// `true` when the item was created, `false` when an existing one was
    /// patched. Lets a caller distinguish a first projection from an update
    /// without a second round trip.
    pub created: bool,
}

/// Why a tracker call failed.
///
/// The variants exist to answer exactly one question — [`TrackerError::retryable`]
/// — because that is the question the delivery layer asks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackerError {
    /// The request never got an answer: DNS, TLS, connection, timeout.
    #[error("transport error: {0}")]
    Transport(String),
    /// The provider answered with a non-success status.
    #[error("provider returned {status}: {body}")]
    Status { status: u16, body: String },
    /// Explicitly rate limited. Separate from [`Self::Status`] because the
    /// retry delay is knowable, and because every provider bills differently
    /// (GitHub alone meters REST and GraphQL from separate budgets).
    #[error("rate limited by provider")]
    RateLimited { retry_after_secs: Option<u64> },
    /// The referenced item is gone upstream — deleted, moved, or never existed.
    #[error("no such item upstream: {0}")]
    NotFound(String),
    /// The provider cannot express what was asked. A configuration problem for
    /// a human, not something a retry can fix.
    #[error("unsupported by provider: {0}")]
    Unsupported(String),
}

impl TrackerError {
    /// Whether retrying could plausibly succeed.
    ///
    /// This mirrors the contract the cloud outbox already enforces — transport
    /// and 5xx queue for replay, a 4xx is a contract rejection that would fail
    /// forever and must be surfaced loudly rather than retried into a loop — so
    /// that when the projector rides the existing outbox it inherits one
    /// delivery policy instead of inventing a second.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::RateLimited { .. } => true,
            Self::Status { status, .. } => *status >= 500,
            Self::NotFound(_) | Self::Unsupported(_) => false,
        }
    }
}

/// A tracker provider, as the projector sees it.
///
/// Implementors must be `Send + Sync`: adapters are shared across tasks and
/// invoked concurrently.
#[async_trait]
pub trait TrackerPort: Send + Sync {
    /// The provider key used in `ext:<provider>/<id>` cross-refs and link
    /// records. Lowercase, stable — changing it orphans every existing link.
    fn provider(&self) -> &str;

    /// What this provider can express. Cheap and synchronous: an adapter that
    /// must call the API to know (Jira's per-project create metadata) caches the
    /// answer behind its own boundary rather than making every caller await.
    fn capabilities(&self) -> TrackerCapabilities;

    /// Create or patch the item. Implementations MUST be idempotent with
    /// respect to `item.external_id`: `Some` patches that item, `None` creates.
    /// Nothing else may be used to decide — matching on title would mint
    /// duplicates the moment a human renamed the ticket.
    async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError>;

    /// Items changed at or after `since` (an RFC-3339 stamp). The watermark
    /// backstop for missed webhooks; webhook delivery is best-effort everywhere.
    async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError>;

    /// Declare that `from` is blocked by exactly `blocked_by` — the full set,
    /// not a delta, so the adapter can converge by adding and removing.
    ///
    /// This is a verb rather than a field on [`WorkItem`] for two reasons that
    /// only appear once a real provider is in the picture. First, a blocking
    /// relation cannot be written before its target exists — GitHub's dependency
    /// endpoint takes the *database id* of an already-created issue — so the
    /// projector must upsert every item and only then wire the relations;
    /// folding relations into `upsert_item` would make projecting a chunk ahead
    /// of its blocker unrepresentable. Second, `WorkItem::content_hash` covers
    /// the content we author, and identity we merely resolved does not belong in
    /// it; keeping relations out preserves the no-op short-circuit's meaning.
    ///
    /// The default refuses, so a provider that cannot express blocking links
    /// gets the documented textual fallback without writing any code. Callers
    /// should consult [`TrackerCapabilities::blocking_links`] first and treat an
    /// `Unsupported` here as a degradation, not a failure.
    /// One item by its external id, or `None` when the provider has no such
    /// item.
    ///
    /// A default implementation is provided so no existing adapter changed when
    /// this verb arrived — the same courtesy `relate_items` got. It filters a
    /// from-the-beginning `fetch_since`, which is correct everywhere and
    /// efficient nowhere; an adapter with a real single-item endpoint should
    /// override it, and both shipped ones eventually should.
    ///
    /// The conflict policy needs this. Without a way to read what the tracker
    /// currently holds, "do not clobber the fields they own" is unenforceable:
    /// we would have nothing to compare our projection against.
    async fn fetch_one(&self, external_id: &str) -> Result<Option<WorkItem>, TrackerError> {
        Ok(self
            .fetch_since("1970-01-01T00:00:00+00:00")
            .await?
            .into_iter()
            .find(|i| i.external_id.as_deref() == Some(external_id)))
    }

    async fn relate_items(
        &self,
        from: &ExternalId,
        blocked_by: &[ExternalId],
    ) -> Result<(), TrackerError> {
        let _ = (from, blocked_by);
        Err(TrackerError::Unsupported(
            "blocking links are not supported by this provider".into(),
        ))
    }

    /// Whether the configured destination actually EXISTS, and enough about it
    /// to say so out loud.
    ///
    /// The gap this closes is a real defect, not a convenience. Adapters
    /// validate the destination's SHAPE in their constructor — `LinearTracker`
    /// checks the string looks like a team key — and discover its EXISTENCE only
    /// on the first real call. So `tracker on --into ZZZ` succeeded and died
    /// later at push, having already written a config that named somewhere that
    /// was never there.
    ///
    /// A default is provided so no existing adapter changed when this verb
    /// arrived, the courtesy `relate_items` and `fetch_one` both got. A provider
    /// that cannot answer says so, and a caller treats `Unsupported` as "cannot
    /// check" rather than as "missing" — refusing to set up because a provider
    /// declines to introspect would be worse than the bug.
    async fn probe_target(&self) -> Result<TargetInfo, TrackerError> {
        Err(TrackerError::Unsupported(
            "this provider cannot describe its destination".into(),
        ))
    }

    /// Create the configured destination, returning what was made.
    ///
    /// The heaviest verb on this trait and the only one that writes STRUCTURE
    /// rather than content: an issue can be closed or deleted, a team that
    /// should not exist is somebody's afternoon. Two consequences the default
    /// encodes deliberately.
    ///
    /// It REFUSES unless an adapter opts in, so provisioning is never something
    /// a provider acquires by accident. And callers must confirm with a human
    /// first — this trait cannot enforce that, so it is stated here and enforced
    /// at the one call site (`cli::run_setup`), which never calls this without
    /// an explicit answer.
    ///
    /// `display_name` is the human label; the key/slug comes from the adapter's
    /// already-configured target, because that is the thing every existing link
    /// and `ext:` cross-ref is written against.
    async fn create_target(&self, display_name: &str) -> Result<TargetInfo, TrackerError> {
        let _ = display_name;
        Err(TrackerError::Unsupported(
            "this provider cannot create its destination — make it by hand first".into(),
        ))
    }

    /// Ensure a CONTAINER exists for `group`, creating it if absent, and
    /// return its identity so the caller can REMEMBER it.
    ///
    /// Idempotent by contract, like [`Self::upsert_item`]: called on every push
    /// for every group, so a second call for an existing container must be a
    /// no-op rather than a duplicate. Resolution prefers
    /// [`WorkGroup::external_id`] when the caller remembered one — a container
    /// whose name a human edited upstream is still OUR container, and
    /// resolving by name after a rename mints a duplicate. A remembered id
    /// that no longer resolves is an ERROR, not a fall-back-to-create:
    /// distinguishing "deleted" from a transport hiccup is not reliably
    /// possible here, and guessing wrong duplicates.
    ///
    /// OWNERSHIP. The NAME is ours at creation only
    /// — after that it is the human's, and an upstream rename is left
    /// standing. `state` is derived from the member chunks and is the only
    /// field a caller may set, and even it only within the values the
    /// projector authors: a current state outside our vocabulary (Linear's
    /// `paused`/`canceled`) records a human's judgement about the work, which
    /// the plan does not have, and is never patched over. Lead, dates,
    /// descriptions and health are never written at all.
    ///
    /// The returned [`UpsertOutcome`] carries the container's id and whether
    /// this call CREATED it (`version` is `None` — containers carry no
    /// concurrency token here). `created` is what lets the caller record who
    /// minted the container, the fact the empty-container question turns on.
    ///
    /// The default REFUSES, the idiom `relate_items`, `fetch_one` and
    /// `probe_target` all follow, so a provider with no container concept needs
    /// no code and the projector treats `Unsupported` as "file the items flat"
    /// rather than as a failure.
    async fn upsert_group(&self, group: &WorkGroup) -> Result<UpsertOutcome, TrackerError> {
        let _ = group;
        Err(TrackerError::Unsupported(
            "this provider has no container to file items in".into(),
        ))
    }

    /// Ensure the ROOF exists — the one container above every group that
    /// represents the roadmap itself (a Linear *initiative*), creating it if
    /// absent, and return its identity so the caller can remember it. The
    /// adapter keeps whatever id it needs and is expected to file subsequent
    /// [`Self::upsert_group`] containers under it.
    ///
    /// Reuses [`WorkGroup`] deliberately: the roof is a name, a derived state
    /// and a remembered id, exactly what a group is, and a second struct with
    /// the same fields would only invite drift. `state` here is derived from
    /// EVERY chunk in the push, not one group's members — how the closed
    /// vocabulary a provider uses for its roof differs from its containers' is
    /// the adapter's translation to make. The ownership rules on
    /// [`Self::upsert_group`] apply unchanged, including the vocabulary guard:
    /// a roof status outside the values we author is a human's judgement and
    /// is left standing.
    ///
    /// The projector calls this BEFORE any `upsert_group`, and a failure must
    /// degrade to groups-without-a-roof rather than aborting the push — the
    /// issues still land. The default REFUSES like `upsert_group`, so a
    /// provider with no such concept needs no code.
    async fn upsert_initiative(
        &self,
        initiative: &WorkGroup,
    ) -> Result<UpsertOutcome, TrackerError> {
        let _ = initiative;
        Err(TrackerError::Unsupported(
            "this provider has no roof to put its containers under".into(),
        ))
    }
}

/// What a destination is, as much as any provider can agree on: the key a human
/// types and the name they read.
///
/// Deliberately thin. Everything else about a target — Linear's workflow states,
/// GitHub's labels — is provider-specific and already resolved behind the
/// adapter's own boundary; hoisting any of it here would make the type a union
/// of things no caller can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    /// The key/slug as it appears in identifiers: `ENG`, `acme/widgets`.
    pub key: String,
    /// The human-readable name, when the provider has a separate one.
    pub name: String,
    /// One line a setup flow can print to prove it found the right place.
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_matches_the_outbox_contract() {
        assert!(TrackerError::Transport("reset".into()).retryable());
        assert!(
            TrackerError::Status {
                status: 503,
                body: String::new()
            }
            .retryable()
        );
        assert!(
            TrackerError::RateLimited {
                retry_after_secs: Some(30)
            }
            .retryable()
        );

        // 4xx would fail forever — never queue it.
        assert!(
            !TrackerError::Status {
                status: 422,
                body: "required field missing".into()
            }
            .retryable()
        );
        assert!(!TrackerError::NotFound("42".into()).retryable());
        assert!(!TrackerError::Unsupported("labels".into()).retryable());
    }

    /// The port must stay usable as a trait object; if this stops compiling the
    /// provider registry collapses back into a closed enum in the core.
    #[test]
    fn port_is_dyn_compatible() {
        fn assert_object_safe(_: &dyn TrackerPort) {}
        let fake = crate::tracker::fake::FakeTracker::new("fake");
        assert_object_safe(&fake);
        let boxed: Box<dyn TrackerPort> = Box::new(crate::tracker::fake::FakeTracker::new("fake"));
        assert_eq!(boxed.provider(), "fake");
    }

    /// The claim the 4th verb rests on: a provider written before `relate_items`
    /// existed still compiles and still behaves sanely, refusing rather than
    /// silently succeeding. If this needed an `impl` block the verb would be a
    /// breaking change to every adapter instead of an extension.
    #[tokio::test]
    async fn relate_items_defaults_to_a_refusal_no_implementor_needed() {
        struct PreExisting;

        #[async_trait]
        impl TrackerPort for PreExisting {
            fn provider(&self) -> &str {
                "pre-existing"
            }
            fn capabilities(&self) -> TrackerCapabilities {
                TrackerCapabilities::full()
            }
            async fn upsert_item(&self, _: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
                unreachable!("not exercised")
            }
            async fn fetch_since(&self, _: &str) -> Result<Vec<WorkItem>, TrackerError> {
                unreachable!("not exercised")
            }
        }

        let err = PreExisting
            .relate_items(&"1".to_string(), &["2".to_string()])
            .await
            .expect_err("the default must refuse");
        assert!(matches!(err, TrackerError::Unsupported(_)));
        // A refusal is a degradation signal, not something to queue and retry.
        assert!(!err.retryable());
    }
}
