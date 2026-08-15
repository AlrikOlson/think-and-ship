//! An in-memory [`TrackerPort`] — the only implementation this seam ships.
//!
//! It exists to answer one question honestly: does the abstraction hold without
//! a provider? If the projector, the link record and the round trip all work
//! against a double that makes no network call, the seam is real; if making them
//! work needs a new verb on the port, it is not, and the right move is to say so
//! rather than widen the trait.
//!
//! Not `#[cfg(test)]`: integration tests and external harnesses live outside
//! this crate's unit-test build, and every one of them wants a tracker that
//! cannot fail unpredictably.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::infra::ExternalId;
use crate::tracker::domain::{TrackerCapabilities, WorkItem};
use crate::tracker::port::{TargetInfo, TrackerError, TrackerPort, UpsertOutcome};

#[derive(Default)]
struct FakeState {
    /// `(changed_at, item)`, newest write last.
    items: Vec<(String, WorkItem)>,
    next_id: u64,
    /// Every successful `upsert_item`, so a test can assert a no-op projection
    /// performed ZERO writes rather than merely landing on the same value.
    writes: usize,
    /// Returned (and consumed) by the next call, for failure-path tests.
    fail_next: Option<TrackerError>,
    clock: String,
    /// `(from, blocked_by)` — the last declared set per item, since
    /// `relate_items` converges rather than appends.
    relations: Vec<(ExternalId, Vec<ExternalId>)>,
    /// Every successful `relate_items`, counted separately from `writes` so a
    /// test can prove an unchanged dep set costs nothing.
    relation_writes: usize,
    /// Whether the destination exists yet. Starts `true` — the overwhelmingly
    /// common case, and a double that defaulted to "missing" would make every
    /// pre-existing test opt out of a state it does not care about.
    target_exists: bool,
    /// The display names passed to `create_target`, in order. A COUNTED LOG
    /// rather than a flag, so a test can prove provisioning happened exactly
    /// once and never behind a `--dry-run`.
    created_targets: Vec<String>,
    /// `name -> state`, the containers ensured so far.
    groups: Vec<(String, crate::tracker::domain::GroupState)>,
    /// Every upsert_group WRITE, excluding no-ops. A counter rather than a flag,
    /// so a test can prove a second push did not re-write an unchanged container.
    group_writes: usize,
    /// The roof, if one was ever ensured. One slot, not a list: a push carries
    /// at most one initiative, and holding a Vec would let a duplicate-creation
    /// bug hide as "the newest entry looks right".
    initiative: Option<(String, crate::tracker::domain::GroupState)>,
    /// Every upsert_initiative WRITE, excluding no-ops — same contract as
    /// `group_writes`, so idempotence stays provable one level up.
    initiative_writes: usize,
    /// `(name, remembered external_id)` per upsert_group CALL, in order — so a
    /// test can prove the projector passed the identity it recorded last push,
    /// which is the whole of rename survival at the projector's level.
    group_calls: Vec<(String, Option<String>)>,
    /// The same log for the roof.
    initiative_calls: Vec<(String, Option<String>)>,
}

/// A tracker that lives entirely in memory.
pub struct FakeTracker {
    provider: String,
    capabilities: TrackerCapabilities,
    state: Mutex<FakeState>,
}

impl FakeTracker {
    #[must_use]
    pub fn new(provider: &str) -> Self {
        Self {
            provider: provider.trim().to_ascii_lowercase(),
            capabilities: TrackerCapabilities::full(),
            state: Mutex::new(FakeState {
                clock: "1970-01-01T00:00:00+00:00".to_string(),
                // Explicit: `Default` would make this false, and a double whose
                // destination is missing until asked otherwise would break every
                // existing test for a reason none of them are about.
                target_exists: true,
                ..FakeState::default()
            }),
        }
    }

    /// Constrain the double, to exercise the degradation paths a real provider
    /// forces (Jira's required fields, a board with no assignee).
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: TrackerCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set the stamp subsequent writes are recorded at.
    pub fn set_clock(&self, stamp: &str) {
        self.lock().clock = stamp.to_string();
    }

    /// Pretend the destination has not been created yet, so a setup flow has to
    /// deal with a missing target.
    pub fn set_target_missing(&self) {
        self.lock().target_exists = false;
    }

    /// The display names `create_target` was called with, in order. Empty means
    /// nothing was provisioned — which is what `--dry-run` must produce.
    #[must_use]
    pub fn created_targets(&self) -> Vec<String> {
        self.lock().created_targets.clone()
    }

    /// The containers this double holds, name-sorted.
    #[must_use]
    pub fn groups(&self) -> Vec<(String, crate::tracker::domain::GroupState)> {
        let mut g = self.lock().groups.clone();
        g.sort_by(|a, b| a.0.cmp(&b.0));
        g
    }

    /// How many times a container was WRITTEN (created or re-stated). The number
    /// an idempotent re-push must not increase.
    #[must_use]
    pub fn group_writes(&self) -> usize {
        self.lock().group_writes
    }

    /// The roof this double holds, if a push ever ensured one.
    #[must_use]
    pub fn initiative(&self) -> Option<(String, crate::tracker::domain::GroupState)> {
        self.lock().initiative.clone()
    }

    /// How many times the roof was WRITTEN — the `group_writes` contract one
    /// level up.
    #[must_use]
    pub fn initiative_writes(&self) -> usize {
        self.lock().initiative_writes
    }

    /// `(name, remembered external_id)` per `upsert_group` call, in order.
    #[must_use]
    pub fn group_calls(&self) -> Vec<(String, Option<String>)> {
        self.lock().group_calls.clone()
    }

    /// `(name, remembered external_id)` per `upsert_initiative` call, in order.
    #[must_use]
    pub fn initiative_calls(&self) -> Vec<(String, Option<String>)> {
        self.lock().initiative_calls.clone()
    }

    /// Make the next call fail. Consumed on use.
    pub fn fail_next(&self, err: TrackerError) {
        self.lock().fail_next = Some(err);
    }

    /// Successful writes so far.
    #[must_use]
    pub fn writes(&self) -> usize {
        self.lock().writes
    }

    /// Successful `relate_items` calls so far. Separate from [`Self::writes`]
    /// because a projection that changed no content may still need to converge
    /// relations, and vice versa.
    #[must_use]
    pub fn relation_writes(&self) -> usize {
        self.lock().relation_writes
    }

    /// What `from` is currently declared to be blocked by. `None` when no
    /// relation has ever been declared for it — distinct from a declared-empty
    /// set, which is how "the deps were removed" is represented.
    #[must_use]
    pub fn blocked_by(&self, from: &str) -> Option<Vec<ExternalId>> {
        self.lock()
            .relations
            .iter()
            .find(|(f, _)| f == from)
            .map(|(_, b)| b.clone())
    }

    /// Current items, oldest write first.
    #[must_use]
    pub fn items(&self) -> Vec<WorkItem> {
        self.lock().items.iter().map(|(_, i)| i.clone()).collect()
    }

    /// One item by id.
    #[must_use]
    pub fn item(&self, external_id: &str) -> Option<WorkItem> {
        self.lock()
            .items
            .iter()
            .find(|(_, i)| i.external_id.as_deref() == Some(external_id))
            .map(|(_, i)| i.clone())
    }

    /// Simulate someone else editing upstream — a human in the tracker's UI.
    /// Bypasses the write counter precisely because it is NOT our write.
    pub fn remote_edit(&self, external_id: &str, mutate: impl FnOnce(&mut WorkItem)) {
        let mut st = self.lock();
        let stamp = st.clock.clone();
        if let Some(slot) = st
            .items
            .iter_mut()
            .find(|(_, i)| i.external_id.as_deref() == Some(external_id))
        {
            mutate(&mut slot.1);
            slot.1.version = Some(bump(slot.1.version.as_deref()));
            slot.0 = stamp;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Whether `at` is at or after `since`, comparing the instants they denote.
///
/// Unparseable stamps fall back to byte comparison rather than silently
/// dropping an item: a watermark that cannot be read is a reason to over-deliver
/// (the caller dedupes on identity) not to lose data.
fn at_or_after(at: &str, since: &str) -> bool {
    use chrono::DateTime;
    match (
        DateTime::parse_from_rfc3339(at),
        DateTime::parse_from_rfc3339(since),
    ) {
        (Ok(a), Ok(s)) => a >= s,
        _ => at >= since,
    }
}

/// Opaque version tokens, monotone so a fence can compare them.
fn bump(current: Option<&str>) -> String {
    let n: u64 = current.and_then(|v| v.parse().ok()).unwrap_or(0);
    (n + 1).to_string()
}

#[async_trait]
impl TrackerPort for FakeTracker {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn capabilities(&self) -> TrackerCapabilities {
        self.capabilities.clone()
    }

    async fn upsert_item(&self, item: &WorkItem) -> Result<UpsertOutcome, TrackerError> {
        self.capabilities
            .admits(item)
            .map_err(TrackerError::Unsupported)?;

        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        let stamp = st.clock.clone();

        // Identity decides create-vs-patch. Never the title (see TrackerPort).
        if let Some(id) = item.external_id.clone() {
            let Some(slot) = st
                .items
                .iter_mut()
                .find(|(_, i)| i.external_id.as_deref() == Some(id.as_str()))
            else {
                return Err(TrackerError::NotFound(id));
            };
            let version = bump(slot.1.version.as_deref());
            slot.1 = WorkItem {
                version: Some(version.clone()),
                ..item.clone()
            };
            slot.0 = stamp;
            st.writes += 1;
            return Ok(UpsertOutcome {
                external_id: id,
                version: Some(version),
                created: false,
            });
        }

        st.next_id += 1;
        let external_id: ExternalId = st.next_id.to_string();
        let version = "1".to_string();
        st.items.push((
            stamp,
            WorkItem {
                external_id: Some(external_id.clone()),
                version: Some(version.clone()),
                ..item.clone()
            },
        ));
        st.writes += 1;
        Ok(UpsertOutcome {
            external_id,
            version: Some(version),
            created: true,
        })
    }

    async fn fetch_since(&self, since: &str) -> Result<Vec<WorkItem>, TrackerError> {
        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        // Compare INSTANTS, not bytes. Byte order was sound only while every
        // stamp came from this engine, which mints a `+00:00` offset — but a
        // provider does not cooperate. Linear returns the `Z` form, and
        // `"…T10:00:00Z" >= "…T10:00:00+00:00"` is TRUE by byte order for the
        // SAME instant, while a non-UTC offset sorts wrong outright.
        Ok(st
            .items
            .iter()
            .filter(|(at, _)| at_or_after(at, since))
            .map(|(_, i)| i.clone())
            .collect())
    }

    async fn relate_items(
        &self,
        from: &ExternalId,
        blocked_by: &[ExternalId],
    ) -> Result<(), TrackerError> {
        // Refuse before touching state, exactly as a capability-constrained
        // provider would — the projector must degrade, not retry.
        if !self.capabilities.blocking_links {
            return Err(TrackerError::Unsupported(
                "provider does not support blocking links".into(),
            ));
        }

        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        // Every referenced item must already exist. This is the fake's way of
        // enforcing the ordering constraint a real provider enforces with a 422:
        // you cannot be blocked by an issue that has not been created yet.
        for id in std::iter::once(from).chain(blocked_by.iter()) {
            if !st
                .items
                .iter()
                .any(|(_, i)| i.external_id.as_deref() == Some(id.as_str()))
            {
                return Err(TrackerError::NotFound(id.clone()));
            }
        }

        let set = blocked_by.to_vec();
        match st.relations.iter_mut().find(|(f, _)| f == from) {
            Some(slot) => slot.1 = set,
            None => st.relations.push((from.clone(), set)),
        }
        st.relation_writes += 1;
        Ok(())
    }

    async fn upsert_group(
        &self,
        group: &crate::tracker::domain::WorkGroup,
    ) -> Result<UpsertOutcome, TrackerError> {
        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        st.group_calls
            .push((group.name.clone(), group.external_id.clone()));
        // Deterministic synthetic id — the double has no rename to survive, so
        // the name suffices as the id's seed.
        let id = format!("grp-{}", group.name);
        let created = match st.groups.iter_mut().find(|(n, _)| n == &group.name) {
            // Unchanged is a NO-OP, not a re-write. The contract says this verb
            // is called on every push for every group, so counting an unchanged
            // container as a write would make idempotence untestable.
            Some(slot) if slot.1 == group.state => false,
            Some(slot) => {
                slot.1 = group.state;
                st.group_writes += 1;
                false
            }
            None => {
                st.groups.push((group.name.clone(), group.state));
                st.group_writes += 1;
                true
            }
        };
        Ok(UpsertOutcome {
            external_id: id,
            version: None,
            created,
        })
    }

    async fn upsert_initiative(
        &self,
        initiative: &crate::tracker::domain::WorkGroup,
    ) -> Result<UpsertOutcome, TrackerError> {
        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        st.initiative_calls
            .push((initiative.name.clone(), initiative.external_id.clone()));
        let id = format!("init-{}", initiative.name);
        // Same no-op contract as upsert_group: an unchanged roof on a re-push
        // must cost zero writes or idempotence is untestable one level up.
        let created = match &mut st.initiative {
            Some(slot) if slot.0 == initiative.name && slot.1 == initiative.state => false,
            Some(slot) => {
                *slot = (initiative.name.clone(), initiative.state);
                st.initiative_writes += 1;
                false
            }
            slot @ None => {
                *slot = Some((initiative.name.clone(), initiative.state));
                st.initiative_writes += 1;
                true
            }
        };
        Ok(UpsertOutcome {
            external_id: id,
            version: None,
            created,
        })
    }

    async fn probe_target(&self) -> Result<TargetInfo, TrackerError> {
        let mut st = self.lock();
        if let Some(err) = st.fail_next.take() {
            return Err(err);
        }
        if !st.target_exists {
            // The SAME variant a real adapter reports for an absent destination,
            // so a caller's missing-target branch is exercised by the shape it
            // will actually meet rather than one invented here.
            return Err(TrackerError::NotFound(format!(
                "{} destination does not exist",
                self.provider
            )));
        }
        Ok(TargetInfo {
            key: "fake-target".into(),
            name: "Fake Target".into(),
            detail: format!("{} item(s) held", st.items.len()),
        })
    }

    async fn create_target(&self, display_name: &str) -> Result<TargetInfo, TrackerError> {
        {
            let mut st = self.lock();
            if let Some(err) = st.fail_next.take() {
                return Err(err);
            }
            st.created_targets.push(display_name.to_string());
            st.target_exists = true;
        }
        // Re-probe rather than synthesising a return value, matching the real
        // adapter's contract that an `Ok` means the destination is USABLE now.
        self.probe_target().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::domain::WorkItemState;

    #[tokio::test]
    async fn create_then_patch_by_identity() {
        let t = FakeTracker::new("fake");

        let created = t.upsert_item(&WorkItem::new("first")).await.unwrap();
        assert!(created.created);
        assert_eq!(t.writes(), 1);

        let mut patch = WorkItem::new("first, renamed");
        patch.external_id = Some(created.external_id.clone());
        let patched = t.upsert_item(&patch).await.unwrap();
        assert!(!patched.created, "same identity must patch, not create");
        assert_eq!(t.items().len(), 1, "no duplicate minted");
        assert_ne!(
            created.version, patched.version,
            "the version token advances on write"
        );
    }

    /// A rename must not mint a second ticket. This is the failure a
    /// title-matching implementation produces, so the double has to be immune to
    /// it or it would not catch the bug in a real adapter.
    #[test]
    fn identity_not_title_decides() {
        let t = FakeTracker::new("fake");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let a = rt
            .block_on(t.upsert_item(&WorkItem::new("same title")))
            .unwrap();
        let b = rt
            .block_on(t.upsert_item(&WorkItem::new("same title")))
            .unwrap();
        assert_ne!(a.external_id, b.external_id);
        assert_eq!(t.items().len(), 2, "two creates, because neither had an id");
    }

    #[tokio::test]
    async fn patching_an_unknown_id_is_not_found_and_not_retryable() {
        let t = FakeTracker::new("fake");
        let mut item = WorkItem::new("ghost");
        item.external_id = Some("nope".into());
        let err = t.upsert_item(&item).await.unwrap_err();
        assert!(matches!(err, TrackerError::NotFound(_)));
        assert!(!err.retryable());
    }

    #[tokio::test]
    async fn capabilities_refuse_before_any_write_lands() {
        let mut caps = TrackerCapabilities::full();
        caps.required_fields = vec!["customfield_1".into()];
        let t = FakeTracker::new("fake").with_capabilities(caps);
        assert!(t.upsert_item(&WorkItem::new("t")).await.is_err());
        assert_eq!(t.writes(), 0, "refusal must not count as a write");
        assert!(t.items().is_empty());
    }

    #[tokio::test]
    async fn fetch_since_honours_the_watermark() {
        let t = FakeTracker::new("fake");
        t.set_clock("2026-07-01T00:00:00+00:00");
        t.upsert_item(&WorkItem::new("old")).await.unwrap();
        t.set_clock("2026-07-20T00:00:00+00:00");
        t.upsert_item(&WorkItem::new("new")).await.unwrap();

        let all = t.fetch_since("2026-01-01T00:00:00+00:00").await.unwrap();
        assert_eq!(all.len(), 2);
        let recent = t.fetch_since("2026-07-10T00:00:00+00:00").await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].title, "new");
    }

    #[tokio::test]
    async fn remote_edit_is_visible_but_is_not_our_write() {
        let t = FakeTracker::new("fake");
        let created = t.upsert_item(&WorkItem::new("ours")).await.unwrap();
        let writes_before = t.writes();

        t.remote_edit(&created.external_id, |i| {
            i.state = WorkItemState::Done;
        });

        assert_eq!(t.writes(), writes_before, "a human's edit is not our write");
        assert_eq!(
            t.item(&created.external_id).unwrap().state,
            WorkItemState::Done
        );
    }

    #[tokio::test]
    async fn injected_failure_is_returned_once() {
        let t = FakeTracker::new("fake");
        t.fail_next(TrackerError::Transport("reset".into()));
        assert!(t.upsert_item(&WorkItem::new("t")).await.is_err());
        assert!(t.upsert_item(&WorkItem::new("t")).await.is_ok(), "consumed");
    }
}

#[cfg(test)]
mod watermark_tests {
    use super::*;

    /// The defect: byte order and time order agree only while every stamp comes
    /// from this engine. A provider's `Z` form breaks that.
    #[test]
    fn the_z_form_and_the_offset_form_are_the_same_instant() {
        assert!(at_or_after(
            "2026-07-26T10:00:00Z",
            "2026-07-26T10:00:00+00:00"
        ));
        assert!(at_or_after(
            "2026-07-26T10:00:00+00:00",
            "2026-07-26T10:00:00Z"
        ));
        // Byte order says only one of those is true, which is the bug.
        assert!("2026-07-26T10:00:00Z" >= "2026-07-26T10:00:00+00:00");
        assert!("2026-07-26T10:00:00+00:00" < "2026-07-26T10:00:00Z");
    }

    /// A non-UTC offset that is EARLIER in time must sort earlier, whatever its
    /// bytes say. 09:00+02:00 is 07:00Z — before 08:00Z — but sorts after it.
    #[test]
    fn a_non_utc_offset_sorts_by_time_not_by_bytes() {
        assert!(!at_or_after(
            "2026-07-26T09:00:00+02:00",
            "2026-07-26T08:00:00Z"
        ));
        // The byte comparison gets this backwards, which is what we replaced.
        assert!("2026-07-26T09:00:00+02:00" >= "2026-07-26T08:00:00Z");
    }

    #[test]
    fn ordinary_comparisons_still_behave() {
        assert!(at_or_after("2026-07-26T11:00:00Z", "2026-07-26T10:00:00Z"));
        assert!(!at_or_after("2026-07-26T09:00:00Z", "2026-07-26T10:00:00Z"));
        // Inclusive lower bound, as before.
        assert!(at_or_after("2026-07-26T10:00:00Z", "2026-07-26T10:00:00Z"));
    }

    /// An unreadable stamp must not silently drop an item — over-delivering is
    /// recoverable by identity dedup, losing data is not.
    #[test]
    fn an_unparseable_stamp_falls_back_rather_than_dropping() {
        assert!(at_or_after("zzz", "2026-07-26T10:00:00Z"));
    }
}
