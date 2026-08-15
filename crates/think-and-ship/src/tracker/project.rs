//! The projector: a roadmap chunk becomes a work item, idempotently.
//!
//! # What makes this different from a sync tool
//!
//! Unito and Exalate move fields between trackers. They cannot move the reason a
//! ticket exists, because they never see it — by the time a task reaches a
//! tracker, the deliberation that produced it is gone. This system holds the
//! reasoning and the plan in the same graph as the work, so a projected item can
//! carry acceptance criteria as a checklist, dependencies as real blocking
//! links, and the `think:`/`task:` refs that produced the decision, in a footer
//! a machine can read back. That payload is the product; the transport is
//! commodity.
//!
//! # Two phases, and why it cannot be one
//!
//! A blocking relation cannot be written before its target exists — GitHub's
//! dependency endpoint takes the database id of an already-created issue. So the
//! projector upserts every opted-in chunk first, and only then wires relations,
//! by which point every chunk in the run has an external id. Sorting by
//! dependency order instead would work only while the graph is acyclic and
//! wholly inside one project, and neither is guaranteed.
//!
//! # Two fences, not one
//!
//! [`WorkItem::content_hash`] covers the content we author, and a dependency's
//! external id is identity we resolved rather than content we wrote. Keeping
//! relations out of the content hash is what lets an unchanged chunk skip its
//! write entirely; it also means a dep-only change hashes equal, so relations
//! carry their own fence (`our_last_relations_hash`). A projection can change
//! content without touching deps, and the reverse, and each is detected.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::roadmap::domain::{Chunk, ChunkStatus, ContainerKind};
use crate::roadmap::engine::RoadmapEngine;
use crate::tracker::domain::{TrackerCapabilities, WorkItem, WorkItemState};
use crate::tracker::outbox::TrackerOutbox;
use crate::tracker::ownership::{Ownership, Reconciled, authored_hash, reconcile_fields};
use crate::tracker::port::{TrackerError, TrackerPort};

/// Marks the machine-readable provenance block in a projected body.
///
/// An HTML comment, so it renders as nothing in every tracker that speaks
/// Markdown while staying exactly parseable on the way back in — which the
/// inbound reconcile needs in order to tell our own writing from a human's.
const FOOTER_OPEN: &str = "<!-- think-and-ship:";
const FOOTER_CLOSE: &str = "-->";

/// What one chunk's projection did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOutcome {
    /// The item was created upstream for the first time.
    Created { external_id: String },
    /// The item existed and its content changed.
    Patched { external_id: String },
    /// Content was byte-identical to our last write — no call was made.
    Skipped { external_id: String },
    /// The provider refused this item (a capability it cannot express). Not a
    /// failure: the run continues and the chunk is reported.
    Refused { reason: String },
    /// A retryable failure (transport or 5xx) put this projection on the outbox
    /// for replay. Nothing is lost.
    Queued { reason: String },
    /// A contract rejection (4xx). Logged loudly and deliberately NOT queued —
    /// it would fail identically forever.
    Rejected { reason: String },
}

impl ProjectionOutcome {
    /// The item was minted this run.
    #[must_use]
    pub fn is_created(&self) -> bool {
        matches!(self, ProjectionOutcome::Created { .. })
    }

    /// The item existed and we wrote to it.
    #[must_use]
    pub fn is_patched(&self) -> bool {
        matches!(self, ProjectionOutcome::Patched { .. })
    }

    /// A fence matched and no call was made.
    #[must_use]
    pub fn is_skipped(&self) -> bool {
        matches!(self, ProjectionOutcome::Skipped { .. })
    }
}

/// The result of a whole projection run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionReport {
    /// `(chunk_id, outcome)` in roadmap order.
    pub outcomes: Vec<(String, ProjectionOutcome)>,
    /// Chunks whose relations were converged this run.
    pub relations_written: Vec<String>,
    /// Chunks whose deps were rendered as prose because the provider cannot
    /// express blocking links.
    pub relations_degraded: Vec<String>,
    /// `(chunk_id, divergence)` — fields where the two sides disagreed and the
    /// disagreement mattered. Reported rather than resolved silently; the
    /// concern module turns these into concern signals.
    pub divergences: Vec<(String, crate::tracker::ownership::Divergence)>,
    /// Containers ensured this run, in name order.
    pub groups_ensured: Vec<String>,
    /// `(group, why)` — a container that could not be ensured. Recorded rather
    /// than fatal: a container problem must not cost the issues.
    pub group_failures: Vec<(String, String)>,
    /// The provider has no container concept, so grouping degraded to filing
    /// items flat. Not a failure — the same shape `relations_degraded` reports.
    pub groups_unsupported: bool,
    /// The roof ensured this run, when the caller named one and the provider
    /// could raise it.
    pub initiative_ensured: Option<String>,
    /// Why the roof could not be ensured. Recorded rather than fatal, exactly
    /// like `group_failures`: a roof problem must not cost the projects or the
    /// issues. `Unsupported` never lands here — that is a provider without the
    /// concept, not a failure.
    pub initiative_failure: Option<String>,
}

impl ProjectionReport {
    /// How many upstream writes this run performed. The number a no-op run must
    /// keep at zero.
    #[must_use]
    pub fn writes(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|(_, o)| {
                matches!(
                    o,
                    ProjectionOutcome::Created { .. } | ProjectionOutcome::Patched { .. }
                )
            })
            .count()
    }

    /// How many outcomes satisfy `pred` — the counting the receipt and the CLI
    /// summary both need, written once so they cannot disagree about what
    /// "updated" means.
    #[must_use]
    pub fn counts_of(&self, pred: impl Fn(&ProjectionOutcome) -> bool) -> usize {
        self.outcomes.iter().filter(|(_, o)| pred(o)).count()
    }
}

/// Render a chunk as a work item for `tracker`.
///
/// Pure: it reads engine state and returns a value, so the body — the part that
/// carries the product's actual differentiator — is testable without a provider.
///
/// `capabilities` decides two things. Dependencies become prose here only when
/// the provider cannot express blocking links, so a provider that can gets them
/// as real relations in phase two rather than as duplicated text. And an
/// over-long body is truncated to `max_body_len` with a visible marker rather
/// than being silently cut or rejected at submit time.
#[must_use]
pub fn to_work_item(
    engine: &RoadmapEngine,
    chunk: &Chunk,
    provider: &str,
    capabilities: &TrackerCapabilities,
) -> WorkItem {
    let link = engine.tracker_link(&chunk.id, provider);

    let mut body = String::new();
    if !chunk.description.is_empty() {
        body.push_str(&chunk.description);
        body.push_str("\n\n");
    }

    if !chunk.acceptance.is_empty() {
        body.push_str("## Acceptance\n\n");
        for a in &chunk.acceptance {
            body.push_str(&format!("- [ ] {a}\n"));
        }
        body.push('\n');
    }

    // Prose deps are the documented fallback, not a supplement: a provider that
    // supports blocking links gets them in phase two, and duplicating them here
    // would leave two sources of truth that drift the moment one is edited.
    if !capabilities.blocking_links && !chunk.deps.is_empty() {
        body.push_str("## Blocked by\n\n");
        for dep in &chunk.deps {
            let title = engine
                .roadmap()
                .chunks
                .iter()
                .find(|c| &c.id == dep)
                .map(|c| c.title.as_str());
            match title {
                Some(t) => body.push_str(&format!("- [ ] `{dep}` — {t}\n")),
                None => body.push_str(&format!("- [ ] `{dep}`\n")),
            }
        }
        body.push_str(
            "\n_This provider cannot express blocking links, so dependencies are listed here._\n\n",
        );
    }

    body.push_str(&provenance_footer(chunk, engine.project_id()));

    // Trailing whitespace is normalized away HERE, at the point the body is
    // authored, because providers do it for us and then disagree with our hash.
    // Linear strips the trailing newline from a description on save; we sent
    // one, read it back without, and `content_hash` differed by that single
    // byte on every projected chunk — enough to make the echo fence misjudge
    // every inbound event. A canonical body has no trailing whitespace, which
    // is a rule every provider already agrees with, so normalizing once here
    // beats teaching each adapter to trim.
    let mut body = body.trim_end().to_string();

    if let Some(max) = capabilities.max_body_len {
        body = truncate_body(body, max);
    }

    WorkItem {
        // Identity resolved BEFORE the item is built: present means patch,
        // absent means create. Never the title — a human renaming the ticket
        // would otherwise mint a duplicate.
        external_id: link.map(|l| l.external_id.clone()),
        title: chunk.title.clone(),
        body,
        state: state_of(chunk.status),
        labels: if capabilities.labels {
            vec![format!("roadmap:{}", band_of(chunk.priority))]
        } else {
            Vec::new()
        },
        assignee: None,
        version: link.and_then(|l| l.last_seen_version.clone()),
        // The chunk's workstream, carried by NAME. The adapter resolves it to a
        // container id; the projector never learns one.
        group: chunk.group.clone(),
    }
}

/// What a projection would do to one chunk, decided WITHOUT touching the
/// network.
///
/// Five answers rather than three, because three was a lie. The old preview
/// had only "created", "updated" and "unchanged", so every chunk it could not
/// prove unchanged was announced as a pending update — and 7 of 46 items in a
/// live workspace were announced that way for days while the real push
/// correctly skipped all 46.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewVerdict {
    /// No link: the item does not exist upstream yet.
    WouldCreate,
    /// The full content fence matches, so the projector's own cheap skip fires
    /// before it does any I/O. Certain, and it is certain because this is
    /// literally the projector's first gate.
    Unchanged,
    /// A field WE author changed. The projector's second gate cannot rescue
    /// this, so a write really is pending.
    WouldUpdate,
    /// The only differences are in fields the ownership table gives the team.
    /// [`reconcile_fields`] replaces those with the tracker's values before
    /// anything is sent, so the projector skips at its second gate — unless the
    /// tracker's own record has moved since our write, which no preview can
    /// know without reading it.
    OnlyTrackerOwned,
    /// The link predates the authored fence, so there is no evidence either
    /// way. Says so, instead of picking the flattering answer. Self-heals on
    /// the next write.
    Unknown,
}

impl PreviewVerdict {
    /// The line a human reads.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewVerdict::WouldCreate => "would be created",
            PreviewVerdict::Unchanged => "unchanged, would not be sent",
            PreviewVerdict::WouldUpdate => "would be updated",
            PreviewVerdict::OnlyTrackerOwned => {
                "unchanged by us — differs only where the tracker owns the field"
            }
            PreviewVerdict::Unknown => "cannot tell without reading the tracker (pre-fence link)",
        }
    }

    /// Whether this verdict promises a write.
    ///
    /// The invariant the agreement test asserts: when the projector SKIPS a
    /// chunk, this must be false. A preview that promises writes the projector
    /// will not perform is the defect this whole type exists to prevent.
    #[must_use]
    pub fn promises_a_write(self) -> bool {
        matches!(
            self,
            PreviewVerdict::WouldCreate | PreviewVerdict::WouldUpdate
        )
    }
}

/// Predict what [`project_all_with_policy`] would do to one chunk, using only
/// local state.
///
/// # Why this is a function and not four lines in the CLI
///
/// It used to be four lines in the CLI, and they reimplemented the projector's
/// FIRST skip gate only. The projector has two: the cheap one on the raw item,
/// and — after `fetch_one` plus [`reconcile_fields`] — one on the reconciled
/// item, which is also the hash it stores. Those two gates disagree on every
/// chunk whose `state`, `labels` or `assignee` diverged from the plan, because
/// the table gives those to the team while `content_hash` still covers them.
/// One copy that both paths are tested against is the only shape in which the
/// preview cannot silently drift from the write again.
///
/// # What it deliberately does NOT do
///
/// It does not fetch. A preview that opens a connection per item is not a
/// preview. The price of that is honest: a remote-only edit since our last
/// write makes the projector write, and no local predicate can see it — which
/// is precisely what [`PreviewVerdict::OnlyTrackerOwned`] says out loud
/// instead of guessing.
#[must_use]
pub fn preview_verdict(
    link: Option<&crate::roadmap::domain::TrackerLink>,
    item: &WorkItem,
    policy: &Ownership,
) -> PreviewVerdict {
    let Some(link) = link else {
        return PreviewVerdict::WouldCreate;
    };
    // Gate 1, character for character the projector's cheap skip.
    if link.our_last_write_hash == item.content_hash() {
        return PreviewVerdict::Unchanged;
    }
    match link.our_last_authored_hash.as_deref() {
        Some(recorded) if recorded == authored_hash(policy, item) => {
            PreviewVerdict::OnlyTrackerOwned
        }
        Some(_) => PreviewVerdict::WouldUpdate,
        None => PreviewVerdict::Unknown,
    }
}

/// The machine-readable provenance block — the thing no field-sync tool can
/// emit, because it never had the reasoning to begin with.
///
/// Keys are emitted in a stable order so the same chunk always produces the same
/// bytes; an unstable footer would change `content_hash` on every run and defeat
/// the no-op skip.
fn provenance_footer(chunk: &Chunk, project_id: &str) -> String {
    let mut think: Vec<&str> = Vec::new();
    let mut ship: Vec<&str> = Vec::new();
    for r in &chunk.cross_refs {
        if r.starts_with("think:") {
            think.push(r);
        } else if r.starts_with("task:") || r.starts_with("action:") || r.starts_with("check:") {
            ship.push(r);
        }
    }
    let payload = serde_json::json!({
        "chunk": chunk.id,
        "project": project_id,
        // The node label rides here rather than in the item's title: upstream,
        // a human reads the sentence, and replacing it with a 24-character
        // label would lose the claim the ticket exists to state. In the footer
        // it survives the round-trip without changing what anyone reads.
        "name": chunk.name,
        "status": chunk.status,
        "deps": chunk.deps,
        "think": think,
        "ship": ship,
    });
    format!("{FOOTER_OPEN} {payload} {FOOTER_CLOSE}\n")
}

/// Trim a body to `max` BYTES without splitting a UTF-8 character, leaving a
/// visible marker. Truncation is announced rather than silent: a body that just
/// stops looks like data loss, because it is.
fn truncate_body(body: String, max: usize) -> String {
    if body.len() <= max {
        return body;
    }
    const MARKER: &str = "\n\n_[truncated to fit this provider's body limit]_\n";
    let budget = max.saturating_sub(MARKER.len());
    let mut cut = budget.min(body.len());
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = body[..cut].to_string();
    out.push_str(MARKER);
    out
}

/// Roadmap status to the canonical work-item state. Backlog, Pending and Blocked
/// all read as "not started" upstream: no tracker has a state that means "we
/// have not decided to start this yet" and inventing one per provider would put
/// provider vocabulary back in the core.
fn state_of(status: ChunkStatus) -> WorkItemState {
    match status {
        ChunkStatus::InProgress => WorkItemState::InProgress,
        ChunkStatus::Done => WorkItemState::Done,
        ChunkStatus::Obsoleted => WorkItemState::Cancelled,
        ChunkStatus::Backlog | ChunkStatus::Pending | ChunkStatus::Blocked => WorkItemState::Todo,
    }
}

/// The priority band, matching the roadmap's own named bands so a label reads
/// the same in the tracker as it does in `roadmap status`.
fn band_of(priority: u32) -> &'static str {
    match priority {
        0..=99 => "critical",
        100..=199 => "high",
        200..=299 => "medium",
        300..=399 => "low",
        _ => "later",
    }
}

/// Fingerprint a dependency set. Sorted, so reordering `deps` — which says
/// nothing — does not look like a change and cost a write.
fn relations_hash(external_ids: &[String]) -> String {
    let mut sorted: Vec<&str> = external_ids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut h = Sha256::new();
    for id in sorted {
        h.update((id.len() as u64).to_le_bytes());
        h.update(id.as_bytes());
    }
    format!("{:x}", h.finalize())
}

/// Project every opted-in chunk into `tracker`.
///
/// # Failure handling
///
/// With an `outbox`, this follows the delivery contract the cloud sync already
/// uses: a retryable failure (transport or 5xx) queues for replay, a contract
/// rejection (4xx) is logged loudly and never queued, and either way the run
/// continues to the next chunk — one chunk's failure is not a reason to abandon
/// the others. Without an outbox there is nowhere durable to put a retryable
/// failure, so it propagates as `Err` rather than being quietly dropped.
///
/// A capability refusal is never a failure: it is reported and the run
/// continues.
///
/// The caller is responsible for the per-project consent gate
/// ([`crate::tracker::config::should_project`]); this function honours the
/// per-chunk gate itself by projecting only [`RoadmapEngine::chunks_opted_in`].
pub async fn project_all(
    engine: &mut RoadmapEngine,
    tracker: &dyn TrackerPort,
    outbox: Option<&TrackerOutbox>,
) -> Result<ProjectionReport, TrackerError> {
    project_all_with_policy(engine, tracker, outbox, &Ownership::default(), None).await
}

/// The three-valued honesty both container levels share: "nothing started",
/// "something is moving", "all finished" — and nothing richer, because anything
/// richer would be the projector inventing an opinion.
///
/// Obsoleted chunks are IGNORED rather than counted as finished — a workstream
/// whose every chunk was abandoned has not been completed, and calling it so
/// upstream would be a lie a human then has to notice and undo.
fn derived_state<'a>(
    chunks: impl Iterator<Item = &'a Chunk>,
) -> crate::tracker::domain::GroupState {
    use crate::roadmap::domain::ChunkStatus;
    use crate::tracker::domain::GroupState;

    let mine: Vec<&Chunk> = chunks
        .filter(|c| c.status != ChunkStatus::Obsoleted)
        .collect();
    if mine.is_empty() {
        return GroupState::NotStarted;
    }
    if mine.iter().all(|c| c.status == ChunkStatus::Done) {
        return GroupState::Complete;
    }
    if mine
        .iter()
        .any(|c| matches!(c.status, ChunkStatus::InProgress | ChunkStatus::Done))
    {
        return GroupState::Active;
    }
    GroupState::NotStarted
}

/// A container's state, derived from the chunks inside it — see
/// [`derived_state`] for the vocabulary and the obsoleted rule.
fn group_state_of(planned: &[Chunk], name: &str) -> crate::tracker::domain::GroupState {
    derived_state(planned.iter().filter(|c| c.group.as_deref() == Some(name)))
}

/// Project under an explicit ownership policy.
///
/// [`project_all`] is this with the documented defaults. The policy is never
/// optional — a project may override the table, but no caller can decline to
/// have one, because the write path takes a [`Reconciled`] and only
/// `reconcile_fields` builds those.
///
/// `initiative` names the roof the whole push files under, when the caller has
/// one. `None` means no roof is attempted at all — which is also the documented
/// default, because only a caller with config in hand can name one honestly.
pub async fn project_all_with_policy(
    engine: &mut RoadmapEngine,
    tracker: &dyn TrackerPort,
    outbox: Option<&TrackerOutbox>,
    policy: &Ownership,
    initiative: Option<&str>,
) -> Result<ProjectionReport, TrackerError> {
    let provider = tracker.provider().to_string();
    let capabilities = tracker.capabilities();
    let mut report = ProjectionReport::default();

    // Stage 1 — upsert. Snapshot first: the borrow ends before any mutation, and
    // the set of chunks in the run must not shift underneath stage two.
    let planned: Vec<Chunk> = engine
        .chunks_opted_in(&provider)
        .into_iter()
        .cloned()
        .collect();

    // Stage −1 — THE ROOF, before any container. The adapter is expected to
    // remember the roof it raised and file every stage-0 container under it, so
    // ordering is the contract here exactly as containers-before-items is below.
    //
    // The remembered identity travels BOTH ways: the recorded uuid rides in on
    // `external_id` so a renamed roof still resolves, and the outcome's id is
    // recorded back so the NEXT push remembers it.
    //
    // Failure DEGRADES: a roadmap whose roof could not be raised still gets its
    // projects and its issues, and the report says why. `Unsupported` is a
    // provider without the concept — the flat-filing treatment, not a failure.
    if let Some(name) = initiative {
        let state = derived_state(planned.iter());
        let remembered = engine
            .container_link(ContainerKind::Initiative, name, &provider)
            .map(|l| l.external_id.clone());
        match tracker
            .upsert_initiative(&crate::tracker::domain::WorkGroup {
                name: name.to_string(),
                state,
                external_id: remembered,
            })
            .await
        {
            Ok(outcome) => {
                let _ = engine.record_container_link(
                    ContainerKind::Initiative,
                    name,
                    &provider,
                    &outcome.external_id,
                    outcome.created,
                );
                report.initiative_ensured = Some(name.to_string());
            }
            Err(TrackerError::Unsupported(_)) => {}
            Err(e) => report.initiative_failure = Some(e.to_string()),
        }
    }

    // Stage 0 — CONTAINERS FIRST. An item can only join a container that already
    // exists, so every group in this run is ensured before any item is written.
    // Ordering, not preference.
    //
    // `Unsupported` is the documented "this provider has no containers" answer
    // and degrades to filing items flat, the same treatment `relate_items` gets.
    // Any other failure is recorded and the run CONTINUES, because a container
    // problem must not cost the issues.
    let mut groups: Vec<String> = planned.iter().filter_map(|c| c.group.clone()).collect();
    groups.sort();
    groups.dedup();
    for name in &groups {
        let state = group_state_of(&planned, name);
        let remembered = engine
            .container_link(ContainerKind::Group, name, &provider)
            .map(|l| l.external_id.clone());
        match tracker
            .upsert_group(&crate::tracker::domain::WorkGroup {
                name: name.clone(),
                state,
                external_id: remembered,
            })
            .await
        {
            Ok(outcome) => {
                let _ = engine.record_container_link(
                    ContainerKind::Group,
                    name,
                    &provider,
                    &outcome.external_id,
                    outcome.created,
                );
                report.groups_ensured.push(name.clone());
            }
            Err(TrackerError::Unsupported(_)) => {
                report.groups_unsupported = true;
                break;
            }
            Err(e) => report.group_failures.push((name.clone(), e.to_string())),
        }
    }

    for chunk in &planned {
        let item = to_work_item(engine, chunk, &provider, &capabilities);

        if let Err(reason) = capabilities.admits(&item) {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "chunk '{}' not projected: {reason}", chunk.id
            );
            report
                .outcomes
                .push((chunk.id.clone(), ProjectionOutcome::Refused { reason }));
            continue;
        }

        // THE POLICY GATE. On a patch we read what the tracker currently holds
        // and merge under the ownership table, so a field the team owns is
        // never overwritten by a projection that simply did not know about it.
        // On a create there is nothing to conflict with.
        //
        // `send_reconciled` is the ONLY caller of `upsert_item` in this module,
        // and it takes a `Reconciled` — which only `reconcile_fields` can
        // construct. A future write path that skips the policy does not compile.
        // CHEAP SKIP, before any I/O. If what we plan to send is byte-identical
        // to what we last sent, nothing local changed and there is nothing to
        // reconcile — re-running a projection must not touch anyone's tracker,
        // and a read is touching it. A remote change is the sweep's job to
        // notice, not this path's.
        if let Some(link) = engine.tracker_link(&chunk.id, &provider)
            && link.our_last_write_hash == item.content_hash()
        {
            report.outcomes.push((
                chunk.id.clone(),
                ProjectionOutcome::Skipped {
                    external_id: link.external_id.clone(),
                },
            ));
            continue;
        }

        let (existing, remote_moved) = match engine.tracker_link(&chunk.id, &provider) {
            Some(link) => {
                let remote = tracker.fetch_one(&link.external_id).await.unwrap_or(None);
                // THE FENCE — and it gates REPORTING, not merging.
                // If the provider's record has not moved since our write, the
                // thing we fetched IS our own last write, so a difference is our
                // pending change rather than a human's edit and raising it would
                // fill the concern channel with our own noise.
                //
                // The values are merged either way. Skipping the merge when the
                // remote is unmoved is what made a contested field oscillate: we
                // deferred to their title once, then re-asserted ours on the
                // next run because we never looked.
                let moved = !matches!(
                    (
                        remote.as_ref().and_then(|r| r.version.as_deref()),
                        link.last_seen_version.as_deref()
                    ),
                    (Some(theirs), Some(ours)) if theirs == ours
                );
                (remote, moved)
            }
            None => (None, false),
        };
        // THE CONCESSION, read back in from the chunk. An open title proposal
        // is the durable record that a contested retitle was already deferred
        // to — without it the merge would re-assert the plan's title on the
        // round after the concession, because the remote is then unmoved.
        let concession = chunk
            .title_proposal
            .as_ref()
            .map(|p| p.suggested_title.as_str());
        let reconciled =
            reconcile_fields(policy, &item, existing.as_ref(), remote_moved, concession);

        // SECOND skip, on what the policy decided we would ACTUALLY send. The
        // merge can turn a locally-changed item back into exactly what we last
        // wrote — a contested field deferring to a moved remote does precisely
        // that — and writing it again would be a no-op push that looks like
        // activity to everyone watching the tracker.
        let hash = reconciled.item().content_hash();
        if let Some(link) = engine.tracker_link(&chunk.id, &provider)
            && link.our_last_write_hash == hash
        {
            report.outcomes.push((
                chunk.id.clone(),
                ProjectionOutcome::Skipped {
                    external_id: link.external_id.clone(),
                },
            ));
            continue;
        }

        for d in reconciled.divergences() {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "chunk '{}' diverged on {}: {}", chunk.id, d.field.as_str(), d.summary()
            );
        }
        report.divergences.extend(
            reconciled
                .divergences()
                .iter()
                .cloned()
                .map(|d| (chunk.id.clone(), d)),
        );

        // A contested title we just deferred to becomes a PROPOSAL on the
        // chunk — the durable memory the next round's `concession` reads.
        // Written here rather than left to a caller because durability is the
        // reconciliation's own invariant: a caller that forgot this call would
        // silently reintroduce the one-round concession. Idempotent in the
        // engine, so a re-projection of the same disagreement does not restamp.
        // The concern signal (caller-side, emit_divergence_concerns) stays the
        // NOTIFICATION; this is the state a human resolves.
        for d in reconciled.divergences() {
            if d.field == crate::tracker::ownership::Field::Title
                && d.owner == crate::tracker::ownership::Owner::Contested
            {
                let source = engine
                    .tracker_link(&chunk.id, &provider)
                    .map(|l| format!("ext:{provider}/{}", l.external_id))
                    .unwrap_or_else(|| format!("ext:{provider}"));
                let reason = format!(
                    "The tracker's title {:?} was kept over the plan's {:?}; accept to adopt it \
                     into the plan, reject to push the plan's title back.",
                    d.theirs, d.ours
                );
                if let Err(e) = engine.propose_title(&chunk.id, d.theirs.clone(), reason, source) {
                    tracing::warn!(
                        target: "think_and_ship::tracker",
                        "could not record the title concession for '{}': {e}", chunk.id
                    );
                }
            }
        }

        let outcome = match send_reconciled(tracker, &reconciled).await {
            Ok(o) => o,
            Err(e) => {
                let queued = crate::tracker::outbox::handle_failure(
                    outbox,
                    &provider,
                    &chunk.id,
                    reconciled.item(),
                    &e,
                );
                if queued {
                    report.outcomes.push((
                        chunk.id.clone(),
                        ProjectionOutcome::Queued {
                            reason: e.to_string(),
                        },
                    ));
                    continue;
                }
                // Nowhere durable to put a retryable failure means the caller
                // has to hear about it; a 4xx has already been logged loudly and
                // is recorded so the run's report is complete.
                if e.retryable() {
                    return Err(e);
                }
                report.outcomes.push((
                    chunk.id.clone(),
                    ProjectionOutcome::Rejected {
                        reason: e.to_string(),
                    },
                ));
                continue;
            }
        };
        // The hash of what we ACTUALLY sent, which after reconciliation may
        // differ from what we planned — recording the plan's hash would make
        // the echo fence compare against bytes that never reached the provider.
        let written = hash.clone();
        engine
            .record_tracker_link(
                &chunk.id,
                &provider,
                &outcome.external_id,
                &written,
                outcome.version.as_deref(),
            )
            .map_err(TrackerError::Unsupported)?;
        // The SECOND fence, stamped from the same sent item. `written` covers
        // fields the team owns and so cannot answer "would we write again?";
        // this one covers only what we author, which is what the preview needs
        // to predict this very code path without repeating its network read.
        // Recorded here, next to the write it describes, because a caller that
        // forgot it would silently return the preview to guessing.
        engine
            .record_tracker_authored(
                &chunk.id,
                &provider,
                &crate::tracker::ownership::authored_hash(policy, reconciled.item()),
            )
            .map_err(TrackerError::Unsupported)?;

        report.outcomes.push((
            chunk.id.clone(),
            if outcome.created {
                ProjectionOutcome::Created {
                    external_id: outcome.external_id,
                }
            } else {
                ProjectionOutcome::Patched {
                    external_id: outcome.external_id,
                }
            },
        ));
    }

    // Stage 2 — relate. Every chunk in the run now has a binding, so a dep can
    // finally be named by the id the provider understands.
    if !capabilities.blocking_links {
        report.relations_degraded = planned
            .iter()
            .filter(|c| !c.deps.is_empty())
            .map(|c| c.id.clone())
            .collect();
        return Ok(report);
    }

    // Resolve every chunk id in the run to its external id once.
    let resolved: BTreeMap<String, String> = planned
        .iter()
        .filter_map(|c| {
            engine
                .tracker_link(&c.id, &provider)
                .map(|l| (c.id.clone(), l.external_id.clone()))
        })
        .collect();

    for chunk in &planned {
        if chunk.deps.is_empty() {
            continue;
        }
        let Some(from) = resolved.get(&chunk.id) else {
            continue; // refused in phase one; nothing to relate.
        };

        let mut blocked_by = Vec::new();
        let mut unresolved = Vec::new();
        for dep in &chunk.deps {
            match resolved.get(dep) {
                Some(id) => blocked_by.push(id.clone()),
                None => unresolved.push(dep.as_str()),
            }
        }
        // A dep outside the opted-in set has no twin to point at. Say so rather
        // than quietly declaring a partial relation that reads as complete.
        if !unresolved.is_empty() {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "chunk '{}' has dependencies that are not projected, so they cannot be linked: {}",
                chunk.id,
                unresolved.join(", ")
            );
            report.relations_degraded.push(chunk.id.clone());
        }
        if blocked_by.is_empty() {
            continue;
        }

        let hash = relations_hash(&blocked_by);
        if let Some(link) = engine.tracker_link(&chunk.id, &provider)
            && link.our_last_relations_hash.as_deref() == Some(hash.as_str())
        {
            continue;
        }

        match tracker.relate_items(from, &blocked_by).await {
            Ok(()) => {
                engine
                    .record_tracker_relations(&chunk.id, &provider, &hash)
                    .map_err(TrackerError::Unsupported)?;
                report.relations_written.push(chunk.id.clone());
            }
            // The provider said it could and then refused this particular set.
            // A degradation, not a run-ending failure.
            Err(TrackerError::Unsupported(reason)) => {
                tracing::warn!(
                    target: "think_and_ship::tracker",
                    "chunk '{}' relations not written: {reason}", chunk.id
                );
                report.relations_degraded.push(chunk.id.clone());
            }
            Err(e) => return Err(e),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::fake::FakeTracker;
    use crate::tracker::outbox::TrackerOutbox;

    fn engine() -> RoadmapEngine {
        RoadmapEngine::new("proj".into())
    }

    fn add(e: &mut RoadmapEngine, id: &str, deps: Vec<String>) {
        e.add_chunk(
            id.into(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            10,
            format!("why {id} exists"),
            vec![format!("{id} works")],
            deps,
            false,
        )
        .expect("add chunk");
    }

    fn opted(e: &mut RoadmapEngine, id: &str) {
        e.set_tracker_opt_in(id, "fake", true).expect("opt in");
    }

    #[test]
    fn the_body_carries_acceptance_and_provenance() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        e.link_chunk("c1", "think:46").expect("link think");
        e.link_chunk("c1", "task:projector").expect("link task");

        let chunk = e.roadmap().chunks[0].clone();
        let item = to_work_item(&e, &chunk, "fake", &TrackerCapabilities::full());

        assert_eq!(item.title, "Chunk c1");
        assert!(item.body.contains("why c1 exists"));
        assert!(item.body.contains("- [ ] c1 works"), "acceptance checklist");

        let footer = item
            .body
            .split(FOOTER_OPEN)
            .nth(1)
            .expect("footer present")
            .trim_end()
            .trim_end_matches(FOOTER_CLOSE)
            .trim();
        let parsed: serde_json::Value = serde_json::from_str(footer).expect("footer is JSON");
        assert_eq!(parsed["chunk"], "c1");
        assert_eq!(parsed["project"], "proj");
        assert_eq!(parsed["think"][0], "think:46");
        assert_eq!(parsed["ship"][0], "task:projector");
    }

    /// The footer must be byte-stable across runs, or content_hash changes every
    /// time and the no-op skip never fires.
    #[test]
    fn the_footer_is_stable_across_renders() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        e.link_chunk("c1", "think:1").expect("ref");
        e.link_chunk("c1", "think:2").expect("ref");
        let chunk = e.roadmap().chunks[0].clone();
        let caps = TrackerCapabilities::full();
        let a = to_work_item(&e, &chunk, "fake", &caps);
        let b = to_work_item(&e, &chunk, "fake", &caps);
        assert_eq!(a.content_hash(), b.content_hash());
    }

    /// Deps become prose ONLY where blocking links are unavailable — otherwise
    /// the same fact would live in two places and drift.
    #[test]
    fn deps_are_prose_only_when_the_provider_cannot_link() {
        let mut e = engine();
        add(&mut e, "base", vec![]);
        add(&mut e, "c1", vec!["base".into()]);
        let chunk = e.roadmap().chunks[1].clone();

        let capable = to_work_item(&e, &chunk, "fake", &TrackerCapabilities::full());
        assert!(!capable.body.contains("## Blocked by"));

        let degraded = to_work_item(
            &e,
            &chunk,
            "fake",
            &TrackerCapabilities {
                blocking_links: false,
                ..TrackerCapabilities::full()
            },
        );
        assert!(degraded.body.contains("## Blocked by"));
        assert!(degraded.body.contains("`base` — Chunk base"));
    }

    #[test]
    fn an_over_long_body_is_truncated_visibly() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        let chunk = e.roadmap().chunks[0].clone();
        let item = to_work_item(
            &e,
            &chunk,
            "fake",
            &TrackerCapabilities {
                max_body_len: Some(120),
                ..TrackerCapabilities::full()
            },
        );
        assert!(item.body.len() <= 120);
        assert!(item.body.contains("truncated"));
    }

    /// Nothing is projected without an explicit per-chunk opt-in. The chunk's
    /// central promise: upgrading cannot fill anyone's tracker.
    #[tokio::test]
    async fn nothing_projects_without_an_opt_in() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        let tracker = FakeTracker::new("fake");

        let report = project_all(&mut e, &tracker, None).await.expect("run");
        assert_eq!(tracker.writes(), 0);
        assert!(report.outcomes.is_empty());
    }

    #[tokio::test]
    async fn an_unchanged_chunk_costs_no_write() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake");

        let first = project_all(&mut e, &tracker, None).await.expect("first");
        assert_eq!(first.writes(), 1);
        assert_eq!(tracker.writes(), 1);

        let second = project_all(&mut e, &tracker, None).await.expect("second");
        assert_eq!(second.writes(), 0, "an unchanged chunk must not be written");
        assert_eq!(tracker.writes(), 1, "no call reached the provider at all");
        assert!(matches!(
            second.outcomes[0].1,
            ProjectionOutcome::Skipped { .. }
        ));
    }

    #[tokio::test]
    async fn a_changed_chunk_patches_the_same_item() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake");
        project_all(&mut e, &tracker, None).await.expect("first");

        e.update_chunk("c1", Some("Renamed".into()), None, None, None, None, None)
            .expect("rename");

        let report = project_all(&mut e, &tracker, None).await.expect("second");
        assert_eq!(report.writes(), 1);
        assert!(matches!(
            report.outcomes[0].1,
            ProjectionOutcome::Patched { .. }
        ));
        assert_eq!(tracker.items().len(), 1, "a rename must not mint a twin");
    }

    /// The ordering property the 4th verb exists for: `c1` depends on `base`, and
    /// the relation can only be written after BOTH have been created.
    #[tokio::test]
    async fn deps_become_native_blocking_links_after_every_item_exists() {
        let mut e = engine();
        add(&mut e, "base", vec![]);
        add(&mut e, "c1", vec!["base".into()]);
        opted(&mut e, "base");
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake");

        let report = project_all(&mut e, &tracker, None).await.expect("run");
        assert_eq!(report.relations_written, vec!["c1".to_string()]);

        let base_id = match &report.outcomes[0].1 {
            ProjectionOutcome::Created { external_id } => external_id.clone(),
            other => panic!("expected base created, got {other:?}"),
        };
        let c1_id = match &report.outcomes[1].1 {
            ProjectionOutcome::Created { external_id } => external_id.clone(),
            other => panic!("expected c1 created, got {other:?}"),
        };
        assert_eq!(tracker.blocked_by(&c1_id), Some(vec![base_id]));
    }

    /// Relations are fenced separately, so an unchanged dep set costs nothing
    /// even though the content fence would also say "unchanged".
    #[tokio::test]
    async fn an_unchanged_dep_set_costs_no_relation_write() {
        let mut e = engine();
        add(&mut e, "base", vec![]);
        add(&mut e, "c1", vec!["base".into()]);
        opted(&mut e, "base");
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake");

        project_all(&mut e, &tracker, None).await.expect("first");
        assert_eq!(tracker.relation_writes(), 1);

        let second = project_all(&mut e, &tracker, None).await.expect("second");
        assert!(second.relations_written.is_empty());
        assert_eq!(tracker.relation_writes(), 1);
    }

    /// A provider that cannot express blocking links must not be asked to, and
    /// the degradation must be reported rather than inferred from silence.
    #[tokio::test]
    async fn a_provider_without_blocking_links_degrades_and_says_so() {
        let mut e = engine();
        add(&mut e, "base", vec![]);
        add(&mut e, "c1", vec!["base".into()]);
        opted(&mut e, "base");
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake").with_capabilities(TrackerCapabilities {
            blocking_links: false,
            ..TrackerCapabilities::full()
        });

        let report = project_all(&mut e, &tracker, None).await.expect("run");
        assert_eq!(tracker.relation_writes(), 0);
        assert_eq!(report.relations_degraded, vec!["c1".to_string()]);
        // …and the fallback actually reached the item.
        let c1 = tracker
            .items()
            .into_iter()
            .find(|i| i.title == "Chunk c1")
            .expect("c1 projected");
        assert!(c1.body.contains("## Blocked by"));
    }

    /// One chunk the provider cannot express must not abandon the others.
    #[tokio::test]
    async fn a_capability_refusal_is_reported_and_the_run_continues() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        add(&mut e, "c2", vec![]);
        opted(&mut e, "c1");
        opted(&mut e, "c2");
        // A provider demanding a field this system has no concept of refuses
        // everything — the Jira required-custom-field shape.
        let tracker = FakeTracker::new("fake").with_capabilities(TrackerCapabilities {
            required_fields: vec!["story_points".into()],
            ..TrackerCapabilities::full()
        });

        let report = project_all(&mut e, &tracker, None).await.expect("run");
        assert_eq!(report.outcomes.len(), 2, "both chunks were reported");
        assert_eq!(tracker.writes(), 0);
        assert!(
            report
                .outcomes
                .iter()
                .all(|(_, o)| matches!(o, ProjectionOutcome::Refused { .. }))
        );
    }

    /// A dep outside the opted-in set has no twin to point at. Reporting it is
    /// the difference between a partial relation and a silent lie.
    #[tokio::test]
    async fn an_unprojected_dep_is_reported_not_silently_dropped() {
        let mut e = engine();
        add(&mut e, "base", vec![]);
        add(&mut e, "c1", vec!["base".into()]);
        opted(&mut e, "c1"); // base deliberately NOT opted in
        let tracker = FakeTracker::new("fake");

        let report = project_all(&mut e, &tracker, None).await.expect("run");
        assert_eq!(report.relations_degraded, vec!["c1".to_string()]);
        assert_eq!(tracker.relation_writes(), 0);
    }

    /// Reordering deps says nothing, so it must not cost a write.
    #[test]
    fn the_relations_hash_ignores_order() {
        assert_eq!(
            relations_hash(&["a".into(), "b".into()]),
            relations_hash(&["b".into(), "a".into()])
        );
        assert_ne!(
            relations_hash(&["a".into()]),
            relations_hash(&["a".into(), "b".into()])
        );
    }

    /// The outbox contract, first half: transport and 5xx queue for replay, and
    /// the run continues to the next chunk rather than abandoning it.
    #[tokio::test]
    async fn a_retryable_failure_queues_and_the_run_continues() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        add(&mut e, "c2", vec![]);
        opted(&mut e, "c1");
        opted(&mut e, "c2");
        let tracker = FakeTracker::new("fake");
        let outbox = TrackerOutbox::new(None);
        tracker.fail_next(TrackerError::Status {
            status: 503,
            body: "unavailable".into(),
        });

        let report = project_all(&mut e, &tracker, Some(&outbox))
            .await
            .expect("a queued failure is not a run failure");

        assert!(matches!(
            report.outcomes[0].1,
            ProjectionOutcome::Queued { .. }
        ));
        assert_eq!(outbox.queued_chunks("fake"), vec!["c1".to_string()]);
        assert!(
            matches!(report.outcomes[1].1, ProjectionOutcome::Created { .. }),
            "one chunk's outage must not abandon the next"
        );
        assert!(
            e.tracker_link("c1", "fake").is_none(),
            "a queued write has not landed, so no binding may be recorded"
        );
    }

    /// The outbox contract, second half: a 4xx would fail identically forever,
    /// so it is logged loudly and NEVER queued. A queue that accumulates
    /// permanently-doomed entries is a queue nobody can drain.
    #[tokio::test]
    async fn a_contract_rejection_is_never_queued() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        add(&mut e, "c2", vec![]);
        opted(&mut e, "c1");
        opted(&mut e, "c2");
        let tracker = FakeTracker::new("fake");
        let outbox = TrackerOutbox::new(None);
        tracker.fail_next(TrackerError::Status {
            status: 422,
            body: "required field missing".into(),
        });

        let report = project_all(&mut e, &tracker, Some(&outbox))
            .await
            .expect("a rejection is not a run failure");

        assert!(matches!(
            report.outcomes[0].1,
            ProjectionOutcome::Rejected { .. }
        ));
        assert!(outbox.is_empty(), "a 4xx must never reach the queue");
        assert!(matches!(
            report.outcomes[1].1,
            ProjectionOutcome::Created { .. }
        ));
    }

    /// A retryable transport failure must reach the caller so it can be queued.
    #[tokio::test]
    async fn a_transport_failure_propagates_for_the_caller_to_queue() {
        let mut e = engine();
        add(&mut e, "c1", vec![]);
        opted(&mut e, "c1");
        let tracker = FakeTracker::new("fake");
        tracker.fail_next(TrackerError::Transport("connection reset".into()));

        let err = project_all(&mut e, &tracker, None)
            .await
            .expect_err("must fail");
        assert!(
            err.retryable(),
            "the caller needs to know this can be replayed"
        );
        assert!(
            e.tracker_link("c1", "fake").is_none(),
            "a failed write must not record a binding"
        );
    }
}

/// The single privileged write path.
///
/// Takes a [`Reconciled`] rather than a [`WorkItem`], which is what makes the
/// ownership policy structural instead of advisory: `Reconciled` has a private
/// field and only `reconcile_fields` constructs one, so reaching this function
/// requires having consulted the table. Grep for `upsert_item` in this module —
/// this is the only hit, and that is the property worth preserving.
async fn send_reconciled(
    tracker: &dyn TrackerPort,
    reconciled: &Reconciled,
) -> Result<crate::tracker::port::UpsertOutcome, TrackerError> {
    tracker.upsert_item(reconciled.item()).await
}

#[cfg(test)]
mod policy_gate_tests {
    /// THE structural criterion. `Reconciled` has a private field, so the only
    /// way to obtain one is `reconcile_fields`, which requires the policy. That
    /// makes `send_reconciled` — the module's single caller of `upsert_item` —
    /// unreachable without consulting the table.
    ///
    /// This test is source inspection rather than behaviour, deliberately: the
    /// property is "a future edit CANNOT do X", and no runtime assertion can
    /// check a path that does not exist. If someone adds a second `upsert_item`
    /// call here, this fails and tells them why.
    #[test]
    fn upsert_has_exactly_one_call_site_and_it_takes_a_reconciled() {
        // Scan only the production half — this test's own assertion strings
        // mention the call, and counting them would make it lie about itself.
        let whole = include_str!("project.rs");
        let src = whole
            .split_once("mod policy_gate_tests")
            .map_or(whole, |(before, _)| before);

        let calls = src.matches("tracker.upsert_item(").count();
        assert_eq!(
            calls, 1,
            "found {calls} calls to upsert_item in the projector. The ownership \
             policy is structural only while there is exactly ONE write path and \
             it takes a Reconciled — which only reconcile_fields can build. A \
             second call site is a path that can skip the table"
        );

        assert!(
            src.contains("reconciled: &Reconciled,"),
            "the single write path must take a Reconciled, not a WorkItem"
        );
        assert!(
            src.contains("tracker.upsert_item(reconciled.item())"),
            "the write must come from the reconciled item, not from the planned one"
        );
    }
}
