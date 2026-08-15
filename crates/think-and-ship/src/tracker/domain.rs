//! The canonical work-item model — pure data, no engine, no IO.
//!
//! # Why this model is deliberately small
//!
//! [`WorkItem`] is a canonical model scoped to ONE bounded context: projecting a
//! roadmap chunk onto an external tracker and reconciling it back. It is *not*
//! an attempt to describe everything Jira, Linear and GitHub can express, and it
//! must not grow into one — an enterprise-wide canonical model is the classic
//! integration failure Fowler named in `MultipleCanonicalModels`, where the
//! shared schema accretes every producer's quirks until no consumer can trust
//! any field.
//!
//! The rule for this file: a field earns its place only if the projector needs
//! it to keep a chunk and its tracker twin in agreement. Everything else — story
//! points, sprints, custom fields, watchers, comment threads — stays on the
//! provider side of its Anti-Corruption Layer, reachable through
//! [`TrackerCapabilities`] rather than modelled here.

use serde::{Deserialize, Serialize};

use crate::infra::{ExternalId, ProviderId};

/// The coarse lifecycle every tracker can express, and the most any of them
/// agree on.
///
/// Providers do NOT agree on state *names* — Linear's workflow states are
/// per-team, Jira's are per-workflow-scheme, a GitHub Projects v2 board's
/// "Status" is a user-defined single-select that need not exist at all. So the
/// canonical model carries the coarse category and each adapter discovers its
/// own mapping at runtime. Never hardcode a provider's state name against this
/// enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemState {
    /// Known, not started.
    Todo,
    /// Being worked.
    InProgress,
    /// Finished.
    Done,
    /// Abandoned — closed without being finished.
    Cancelled,
}

impl WorkItemState {
    /// Stable string form, used for hashing and adapter mapping tables.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One work item as this system understands it: the canonical form a roadmap
/// chunk is projected into and a tracker payload is translated back out of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    /// The tracker's own id. `None` means "not yet created there" — an item the
    /// projector must create rather than patch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<ExternalId>,
    pub title: String,
    /// The rendered body. Markdown here; an adapter that needs another format
    /// (Jira's ADF) converts at its own boundary, never in this struct.
    #[serde(default)]
    pub body: String,
    pub state: WorkItemState,
    #[serde(default)]
    pub labels: Vec<String>,
    /// Provider-native user identity, opaque to us.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// The provider's optimistic-concurrency token (etag, `updatedAt`, version
    /// number) as an opaque string. Compared, never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The container this item belongs in — a Linear *project*, and whatever
    /// each other provider calls the same idea.
    ///
    /// Carries the group's NAME, not its upstream id, because the projector
    /// works from the roadmap and the roadmap knows names. Resolving a name to
    /// a provider id is the adapter's job, behind its own boundary, exactly as
    /// it already resolves label names and workflow states.
    ///
    /// `None` means unfiled, which stays the honest default: an item with no
    /// container behaves exactly as every item did before containers existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// A container of work items — a Linear *project*, a GitHub milestone, whatever
/// the provider calls the box that several issues sit in.
///
/// Deliberately thin, and thinner than the providers allow. A Linear project
/// carries a lead, a target date, health and a description; none of them appear
/// here because the roadmap has no basis for any of them. It has priorities,
/// not deadlines. Putting a `target_date` on this struct would invite the
/// projector to invent one, and a date nobody chose is worse than no date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkGroup {
    /// Human-readable name. The identity the projector works in — until the
    /// container has been pushed once and `external_id` takes over.
    pub name: String,
    /// Derived from the member chunks — see [`GroupState`].
    pub state: GroupState,
    /// The provider id remembered from an earlier push, when the caller has
    /// one. An adapter MUST prefer this over the name: the name is the one
    /// thing a human is likely to edit upstream, and resolving by it after a
    /// rename mints a duplicate container.
    pub external_id: Option<String>,
}

/// How far along a container is, derived from the chunks inside it.
///
/// Three states rather than the provider's five: we can honestly distinguish
/// "nothing started", "something is moving" and "everything is finished" from
/// the chunks. Paused and cancelled are human judgements about intent that the
/// roadmap does not record, so they are absent here and an adapter must leave
/// them alone rather than map something onto them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    NotStarted,
    Active,
    Complete,
}

impl WorkItem {
    /// A minimal item in [`WorkItemState::Todo`], not yet created upstream.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            external_id: None,
            title: title.into(),
            body: String::new(),
            state: WorkItemState::Todo,
            labels: Vec::new(),
            assignee: None,
            version: None,
            group: None,
        }
    }

    /// A stable digest of the AUTHORED content — title, body, state, labels,
    /// assignee, group.
    ///
    /// Identity (`external_id`) and the provider's concurrency token
    /// (`version`) are excluded on purpose. The question this hash answers is
    /// "has the content we author changed?", which must stay independent of
    /// where the item lives and how many times the provider has bumped its
    /// version. That independence is what lets the projector skip a no-op
    /// projection and, separately, recognize its own write echoing back.
    ///
    /// Fields are length-prefixed so no combination of values can collide by
    /// running together (`"ab" + "c"` must not hash as `"a" + "bc"`).
    #[must_use]
    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};

        fn feed(h: &mut Sha256, s: &str) {
            h.update((s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }

        let mut h = Sha256::new();
        feed(&mut h, &self.title);
        feed(&mut h, &self.body);
        feed(&mut h, self.state.as_str());
        h.update((self.labels.len() as u64).to_le_bytes());
        for l in &self.labels {
            feed(&mut h, l);
        }
        feed(&mut h, self.assignee.as_deref().unwrap_or(""));
        // The group is authored content: regrouping a chunk must dirty the item,
        // or the cheap skip in the projector eats the projectId attach and the
        // container stays empty for every already-mirrored issue.
        feed(&mut h, self.group.as_deref().unwrap_or(""));
        format!("{:x}", h.finalize())
    }
}

/// What a provider can actually do, discovered at runtime rather than assumed.
///
/// This is the degradation contract. Providers differ in ways that break naive
/// integrations: Jira projects impose required fields through per-project screen
/// schemes, Linear scopes workflow states to a team, a Projects v2 board may
/// have no "Status" field at all. An adapter reports what it supports and the
/// projector degrades — writes a textual fallback, or refuses with an
/// actionable message BEFORE calling the API — instead of discovering the
/// mismatch as an unreadable 400 at submit time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerCapabilities {
    /// Native blocking/dependency links between items. When false, a chunk's
    /// `deps` project as prose instead.
    pub blocking_links: bool,
    pub labels: bool,
    pub assignee: bool,
    /// Body length ceiling, if the provider enforces one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_len: Option<usize>,
    /// Fields the provider will reject a create without, and that we cannot
    /// derive from a [`WorkItem`]. Non-empty means "ask a human to configure a
    /// default before projecting here".
    #[serde(default)]
    pub required_fields: Vec<String>,
}

impl TrackerCapabilities {
    /// Everything supported, nothing required — the baseline a test double or a
    /// maximally capable provider reports.
    #[must_use]
    pub fn full() -> Self {
        Self {
            blocking_links: true,
            labels: true,
            assignee: true,
            max_body_len: None,
            required_fields: Vec::new(),
        }
    }

    /// Whether a [`WorkItem`] can be projected as-is. `Err` carries a message
    /// meant for a human, naming what to fix.
    pub fn admits(&self, item: &WorkItem) -> Result<(), String> {
        if !self.required_fields.is_empty() {
            return Err(format!(
                "provider requires field(s) this system cannot derive: {}",
                self.required_fields.join(", ")
            ));
        }
        if let Some(max) = self.max_body_len
            && item.body.len() > max
        {
            return Err(format!(
                "body is {} bytes, provider accepts at most {max}",
                item.body.len()
            ));
        }
        if !self.labels && !item.labels.is_empty() {
            return Err("provider does not support labels".to_string());
        }
        if !self.assignee && item.assignee.is_some() {
            return Err("provider does not support an assignee".to_string());
        }
        Ok(())
    }
}

/// A provider key plus the id it minted — the identity of one tracker twin.
///
/// Mirrors [`crate::infra::CrossRef::External`], which is the same pair in wire
/// form. This struct is the in-memory shape; the cross-ref is the graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRef {
    pub provider: ProviderId,
    pub external_id: ExternalId,
}

impl ExternalRef {
    #[must_use]
    pub fn new(provider: &str, external_id: &str) -> Self {
        Self {
            provider: provider.trim().to_ascii_lowercase(),
            external_id: external_id.trim().to_string(),
        }
    }

    /// The wire cross-ref this pair corresponds to.
    #[must_use]
    pub fn to_cross_ref(&self) -> crate::infra::CrossRef {
        crate::infra::CrossRef::external(&self.provider, &self.external_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable_and_order_sensitive() {
        let a = WorkItem::new("title");
        assert_eq!(a.content_hash(), a.clone().content_hash());

        let mut b = a.clone();
        b.title = "other".into();
        assert_ne!(a.content_hash(), b.content_hash());
    }

    /// Identity and the provider's version token must not move the hash, or the
    /// "did our content change?" question would answer yes every time the
    /// provider bumped a version — defeating both the no-op skip and the echo
    /// fence built on this.
    #[test]
    fn content_hash_ignores_identity_and_version() {
        let base = WorkItem::new("title");
        let mut identified = base.clone();
        identified.external_id = Some("12345".into());
        identified.version = Some("W/\"abc\"".into());
        assert_eq!(base.content_hash(), identified.content_hash());
    }

    /// Length-prefixing means no run-together collision: two items whose fields
    /// concatenate to the same bytes must still hash differently.
    #[test]
    fn content_hash_resists_field_run_together() {
        let mut a = WorkItem::new("ab");
        a.body = "c".into();
        let mut b = WorkItem::new("a");
        b.body = "bc".into();
        assert_ne!(a.content_hash(), b.content_hash());

        let mut labels_a = WorkItem::new("t");
        labels_a.labels = vec!["ab".into(), "c".into()];
        let mut labels_b = WorkItem::new("t");
        labels_b.labels = vec!["a".into(), "bc".into()];
        assert_ne!(labels_a.content_hash(), labels_b.content_hash());
    }

    #[test]
    fn capabilities_admit_a_plain_item() {
        assert!(
            TrackerCapabilities::full()
                .admits(&WorkItem::new("t"))
                .is_ok()
        );
    }

    #[test]
    fn capabilities_refuse_before_the_api_call() {
        let mut caps = TrackerCapabilities::full();
        caps.required_fields = vec!["customfield_10010".into()];
        let err = caps.admits(&WorkItem::new("t")).unwrap_err();
        assert!(
            err.contains("customfield_10010"),
            "message names the field: {err}"
        );

        let mut caps = TrackerCapabilities::full();
        caps.max_body_len = Some(4);
        let mut item = WorkItem::new("t");
        item.body = "too long".into();
        assert!(caps.admits(&item).is_err());

        let mut caps = TrackerCapabilities::full();
        caps.labels = false;
        let mut item = WorkItem::new("t");
        item.labels = vec!["bug".into()];
        assert!(caps.admits(&item).is_err());
    }

    #[test]
    fn external_ref_matches_the_cross_ref_wire_form() {
        let r = ExternalRef::new("GitHub", "1234");
        assert_eq!(r.provider, "github");
        assert_eq!(r.to_cross_ref().to_wire(), "ext:github/1234");
    }

    #[test]
    fn work_item_round_trips_through_json() {
        let mut item = WorkItem::new("t");
        item.state = WorkItemState::InProgress;
        item.labels = vec!["a".into()];
        let s = serde_json::to_string(&item).unwrap();
        let back: WorkItem = serde_json::from_str(&s).unwrap();
        assert_eq!(item, back);
    }
}
