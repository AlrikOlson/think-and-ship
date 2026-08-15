//! Divergence reaches a human — the last step of the conflict story.
//!
//! # Why this is not inside the projector
//!
//! [`project_all`](super::project::project_all) DETECTS divergence and returns it. It does not raise
//! anything, and that separation is deliberate: a detector that also writes is
//! a detector nobody can reason about, and the same line was held for the
//! reconcile sweep in `tracker::sweep`. So the projector reports, the CALLER
//! decides whether those reports become signals, and this module is what the
//! caller uses when it decides yes.
//!
//! The practical consequence: `project_all` still takes no [`SignalEngine`],
//! and a caller that only wants to know what diverged — a dry run, a test, a
//! read-only audit — gets that without anything being written anywhere.
//!
//! # Why a signal rather than a log line
//!
//! Before this, a divergence reached `tracing::warn`. That means it reached a
//! human only if one happened to be watching stderr during a push, which is
//! nobody, most of the time. The signal family already solves this problem:
//! capture, research, surface under earned-interruption discipline, promote to
//! a roadmap chunk. Reusing it means a conflict arrives through machinery this
//! project already built and already trusts, instead of a bespoke channel that
//! would need its own inbox, its own triage and its own reasons to be believed.
//!
//! # Both directions of provenance
//!
//! Each concern carries two cross-refs: `chunk:<id>` and
//! `ext:<provider>/<external_id>`. From the chunk you can find the conflict;
//! from the ticket you can find the chunk. A one-directional link would make
//! the concern findable only by someone who already knew where to look.

use crate::roadmap::engine::RoadmapEngine;
use crate::signal::SignalEngine;
use crate::signal::domain::SignalKind;
use crate::tracker::project::ProjectionReport;

/// Who a divergence concern is "from". Not a person — the projection itself
/// noticed it, and pretending otherwise would put a human's name on a machine's
/// observation.
const AUTHOR: &str = "tracker-projection";

/// Turn a projection's divergences into concern signals.
///
/// Returns the ids of the signals actually captured, which is fewer than the
/// divergences whenever one was already raised.
///
/// IDEMPOTENT by body and refs: re-projecting an unchanged conflict does not
/// mint a second signal. That matters more than it looks — a projection runs on
/// every push, so a duplicating version would turn one disagreement into a
/// stream of identical concerns and the inbox would be abandoned within a day.
/// A conflict whose VALUES changed produces a different body and therefore a
/// new signal, which is correct: it is a different disagreement.
pub fn emit_divergence_concerns(
    signal: &mut SignalEngine,
    roadmap: &RoadmapEngine,
    provider: &str,
    report: &ProjectionReport,
) -> Vec<String> {
    let mut captured = Vec::new();

    for (chunk_id, divergence) in &report.divergences {
        // A field the tracker owns diverging is normal, not news. Raising it
        // would be the noise that gets the whole channel ignored.
        if divergence.owner == crate::tracker::ownership::Owner::Theirs {
            continue;
        }

        let Some(link) = roadmap.tracker_link(chunk_id, provider) else {
            // No link means no external ref to point at, and a concern that
            // cannot name the ticket it is about is not worth raising.
            continue;
        };

        let chunk_ref = format!("chunk:{chunk_id}");
        let external_ref = format!("ext:{provider}/{}", link.external_id);
        let body = divergence.summary();

        if already_raised(signal, &body, &chunk_ref, &external_ref) {
            continue;
        }

        let id = signal
            .capture(SignalKind::Concern, AUTHOR.to_string(), body)
            .id
            .clone();

        // Both directions, so the conflict is findable from either end.
        for r in [&chunk_ref, &external_ref] {
            if let Err(e) = signal.link(&id, r) {
                tracing::warn!(
                    target: "think_and_ship::tracker",
                    "could not link concern {id} to {r}: {e}"
                );
            }
        }
        captured.push(id);
    }

    captured
}

/// Whether this exact disagreement is already on someone's plate.
///
/// Matches on the body AND both refs rather than the body alone: the same
/// sentence about two different chunks is two different problems.
fn already_raised(signal: &SignalEngine, body: &str, chunk_ref: &str, external_ref: &str) -> bool {
    signal.signals().signals.iter().any(|s| {
        s.body == body
            && s.cross_refs.iter().any(|r| r == chunk_ref)
            && s.cross_refs.iter().any(|r| r == external_ref)
    })
}

/// Turn a sweep's remote changes into STATUS proposals on the chunks they
/// belong to.
///
/// The counterpart of [`emit_divergence_concerns`], and caller-invoked for the
/// same reason: `reconcile` detects, this decides, and a caller that only wants
/// to know what moved gets that without anything being written.
///
/// A proposal is NEVER a transition. A ticket closed in the tracker looks
/// exactly like a chunk that should go done, but a close means the ticket is
/// finished — not that the acceptance criteria were met. Transitioning silently
/// would remove the one moment a human was going to look at the evidence, which
/// is the moment the roadmap exists to create.
///
/// Returns the chunk ids that received a proposal.
pub fn propose_status_from_sweep(
    roadmap: &mut RoadmapEngine,
    provider: &str,
    report: &crate::tracker::sweep::SweepReport,
) -> Vec<String> {
    let mut proposed = Vec::new();

    for item in &report.remote {
        let Some(external_id) = item.external_id.as_deref() else {
            continue;
        };
        let Some((chunk_id, current)) = roadmap
            .tracker_link_by_external_id(provider, external_id)
            .map(|l| l.chunk_id.clone())
            .and_then(|id| {
                roadmap
                    .roadmap()
                    .chunks
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| (c.id.clone(), c.status))
            })
        else {
            // A ticket we never projected. The sweep reports it; proposing a
            // status for a chunk that does not exist would be nonsense.
            continue;
        };

        let suggested = status_for(item.state);
        if suggested == current {
            continue;
        }

        let reason = format!(
            "The tracker moved {external_id} to {:?}. The plan still says {current:?}. \
             A tracker close means the ticket is finished, not that the acceptance \
             criteria were met — so this is a suggestion, not a transition.",
            item.state
        );
        let source = format!("ext:{provider}/{external_id}");

        if let Err(e) = roadmap.propose_status(&chunk_id, suggested, reason, source) {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "could not propose a status for '{chunk_id}': {e}"
            );
            continue;
        }
        proposed.push(chunk_id);
    }

    proposed
}

/// Turn a sweep's remote retitles into TITLE proposals on the chunks they
/// belong to — the pull-side door into the durable-concession machinery.
///
/// The sibling of [`propose_status_from_sweep`], and the reason it exists is
/// the shape that sibling cannot see: a human retitles a ticket and the plan
/// never edits that chunk locally. Push cheap-skips before any I/O (nothing
/// local changed), so reconcile never runs and no Contested divergence is
/// produced. The sweep DOES fetch the retitle as a genuine remote change —
/// this is where that fetch becomes a proposal.
///
/// Two fences, both inherited rather than reimplemented:
///
/// - **Echo**: only `report.remote` is read. The sweep's `reconcile` already
///   ran every fetched item through the echo fence; our own writes coming
///   back land in `echoes`/`drifted` and never reach this loop, so a title
///   proposal cannot be minted from our own projection returning.
/// - **Ownership**: only an [`Owner::Contested`](crate::tracker::ownership::Owner::Contested) title may propose. `Ours`
///   means a remote title divergence is push's business to re-assert, and
///   proposing it here would launder a remote overwrite of a field the plan
///   owns into a polite suggestion. `Theirs` (a table override) means the
///   team decided title divergence is not a conversation at all.
///
/// A proposal is NEVER an edit: `propose_title` fills `title_proposal` and
/// the chunk's real `title` does not move. Idempotent across repeated sweeps
/// because `propose_title` is — an unchanged suggestion does not restamp
/// `proposed_at`.
///
/// Returns the chunk ids that received a proposal.
pub fn propose_titles_from_sweep(
    roadmap: &mut RoadmapEngine,
    provider: &str,
    report: &crate::tracker::sweep::SweepReport,
    ownership: &crate::tracker::ownership::Ownership,
) -> Vec<String> {
    use crate::tracker::ownership::{Field, Owner};

    if ownership.owner(Field::Title) != Owner::Contested {
        return Vec::new();
    }

    let mut proposed = Vec::new();

    for item in &report.remote {
        let Some(external_id) = item.external_id.as_deref() else {
            continue;
        };
        let Some((chunk_id, current_title)) = roadmap
            .tracker_link_by_external_id(provider, external_id)
            .map(|l| l.chunk_id.clone())
            .and_then(|id| {
                roadmap
                    .roadmap()
                    .chunks
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| (c.id.clone(), c.title.clone()))
            })
        else {
            // A ticket we never projected. The sweep reports it; proposing a
            // title for a chunk that does not exist would be nonsense.
            continue;
        };

        if item.title == current_title {
            continue;
        }

        let reason = format!(
            "The tracker retitled {external_id} to \"{}\". The plan still says \
             \"{current_title}\". A remote retitle is a real statement about the \
             plan — but so is ours, so nobody wins silently: this is a suggestion, \
             not an edit.",
            item.title
        );
        let source = format!("ext:{provider}/{external_id}");

        if let Err(e) = roadmap.propose_title(&chunk_id, item.title.clone(), reason, source) {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "could not propose a title for '{chunk_id}': {e}"
            );
            continue;
        }
        proposed.push(chunk_id);
    }

    proposed
}

/// The tracker's canonical state as a roadmap status.
///
/// Deliberately NOT the inverse of the projector's `state_of`: that maps five
/// chunk statuses onto four tracker states, so the round trip cannot be a
/// bijection and pretending otherwise would invent transitions nobody asked
/// for. `Todo` maps to `Pending` rather than `Backlog` because a ticket that
/// exists upstream is work someone has already decided to do.
fn status_for(state: crate::tracker::domain::WorkItemState) -> crate::roadmap::domain::ChunkStatus {
    use crate::roadmap::domain::ChunkStatus;
    use crate::tracker::domain::WorkItemState;
    match state {
        WorkItemState::Todo => ChunkStatus::Pending,
        WorkItemState::InProgress => ChunkStatus::InProgress,
        WorkItemState::Done => ChunkStatus::Done,
        WorkItemState::Cancelled => ChunkStatus::Obsoleted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap::domain::ChunkStatus;
    use crate::tracker::ownership::{Divergence, Field, Owner};

    fn roadmap_with_link() -> RoadmapEngine {
        let mut e = RoadmapEngine::new("proj".into());
        e.add_chunk(
            "c1".into(),
            "Chunk c1".into(),
            ChunkStatus::Pending,
            10,
            "why".into(),
            vec!["works".into()],
            vec![],
            false,
        )
        .expect("add");
        e.record_tracker_link("c1", "linear", "THI-1", "hash", Some("v1"))
            .expect("link");
        e
    }

    fn report_with(owner: Owner) -> ProjectionReport {
        ProjectionReport {
            divergences: vec![(
                "c1".to_string(),
                Divergence {
                    field: Field::Title,
                    owner,
                    ours: "our title".into(),
                    theirs: "their title".into(),
                },
            )],
            ..ProjectionReport::default()
        }
    }

    #[test]
    fn a_contested_divergence_becomes_a_concern_linked_both_ways() {
        let mut signal = SignalEngine::new("proj".into());
        let roadmap = roadmap_with_link();

        let ids = emit_divergence_concerns(
            &mut signal,
            &roadmap,
            "linear",
            &report_with(Owner::Contested),
        );

        assert_eq!(ids.len(), 1);
        let s = signal.get(&ids[0]).expect("signal");
        assert_eq!(s.kind, SignalKind::Concern);
        assert!(
            s.cross_refs.contains(&"chunk:c1".to_string()),
            "findable from the chunk: {:?}",
            s.cross_refs
        );
        assert!(
            s.cross_refs.contains(&"ext:linear/THI-1".to_string()),
            "findable from the ticket: {:?}",
            s.cross_refs
        );
        assert!(
            s.body.contains("neither side owns it"),
            "the signal body is the divergence's own sentence, not a second wording"
        );
    }

    /// A projection runs on every push. A duplicating emitter would turn one
    /// disagreement into a stream of identical concerns, and the inbox would be
    /// abandoned within a day.
    #[test]
    fn re_raising_an_unchanged_conflict_mints_nothing() {
        let mut signal = SignalEngine::new("proj".into());
        let roadmap = roadmap_with_link();
        let report = report_with(Owner::Contested);

        let first = emit_divergence_concerns(&mut signal, &roadmap, "linear", &report);
        let second = emit_divergence_concerns(&mut signal, &roadmap, "linear", &report);

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "the same conflict was raised twice");
        assert_eq!(signal.signals().signals.len(), 1);
    }

    /// A conflict whose VALUES moved is a different disagreement and deserves
    /// its own concern — dedup must not swallow a genuine change.
    #[test]
    fn a_changed_conflict_raises_again() {
        let mut signal = SignalEngine::new("proj".into());
        let roadmap = roadmap_with_link();

        emit_divergence_concerns(
            &mut signal,
            &roadmap,
            "linear",
            &report_with(Owner::Contested),
        );

        let mut moved = report_with(Owner::Contested);
        moved.divergences[0].1.theirs = "a different retitle".into();
        let again = emit_divergence_concerns(&mut signal, &roadmap, "linear", &moved);

        assert_eq!(again.len(), 1, "a new disagreement must reach someone");
        assert_eq!(signal.signals().signals.len(), 2);
    }

    /// A field the tracker owns diverging is normal. Raising it is the noise
    /// that gets the whole channel ignored.
    #[test]
    fn a_field_they_own_raises_nothing() {
        let mut signal = SignalEngine::new("proj".into());
        let roadmap = roadmap_with_link();

        let ids =
            emit_divergence_concerns(&mut signal, &roadmap, "linear", &report_with(Owner::Theirs));

        assert!(ids.is_empty());
        assert!(signal.signals().signals.is_empty());
    }

    /// Without a link there is no ticket to name, and a concern that cannot say
    /// which ticket it is about is not worth raising.
    #[test]
    fn a_chunk_with_no_link_raises_nothing() {
        let mut signal = SignalEngine::new("proj".into());
        let roadmap = RoadmapEngine::new("proj".into());

        let ids = emit_divergence_concerns(
            &mut signal,
            &roadmap,
            "linear",
            &report_with(Owner::Contested),
        );
        assert!(ids.is_empty());
    }

    // ── propose_titles_from_sweep (tracker-sweep-title-proposal) ──────────

    use crate::tracker::domain::{WorkItem, WorkItemState};
    use crate::tracker::ownership::Ownership;
    use crate::tracker::sweep::SweepReport;

    fn item_titled(title: &str) -> WorkItem {
        WorkItem {
            external_id: Some("THI-1".to_string()),
            title: title.into(),
            body: String::new(),
            state: WorkItemState::Todo,
            labels: vec![],
            assignee: None,
            version: Some("v2".into()),
            group: None,
        }
    }

    fn sweep_finding(item: WorkItem) -> SweepReport {
        SweepReport {
            fetched: 1,
            remote: vec![item],
            ..SweepReport::default()
        }
    }

    #[test]
    fn a_remote_retitle_becomes_a_title_proposal_and_the_title_does_not_move() {
        let mut roadmap = roadmap_with_link();

        let proposed = propose_titles_from_sweep(
            &mut roadmap,
            "linear",
            &sweep_finding(item_titled("A human's better name")),
            &Ownership::default(),
        );

        assert_eq!(proposed, vec!["c1".to_string()]);
        let chunk = roadmap
            .roadmap()
            .chunks
            .iter()
            .find(|c| c.id == "c1")
            .expect("chunk");
        let p = chunk
            .title_proposal
            .as_ref()
            .expect("the proposal must be RECORDED, not merely returned");
        assert_eq!(p.suggested_title, "A human's better name");
        assert_eq!(
            p.source, "ext:linear/THI-1",
            "the proposal must say which ticket caused it"
        );
        assert_eq!(
            chunk.title, "Chunk c1",
            "a proposal is never a rename — the plan's title must not move"
        );
    }

    /// The ownership gate. A project that sets Title to Ours has said the plan
    /// wins: re-asserting it is push's business, and proposing it here would
    /// launder a remote overwrite into a polite suggestion. Theirs means the
    /// team decided title divergence is not a conversation at all.
    #[test]
    fn a_title_the_plan_owns_gets_no_proposal_from_the_sweep() {
        for owner in [Owner::Ours, Owner::Theirs] {
            let mut roadmap = roadmap_with_link();
            let proposed = propose_titles_from_sweep(
                &mut roadmap,
                "linear",
                &sweep_finding(item_titled("A human's better name")),
                &Ownership::default().with(Field::Title, owner),
            );
            assert!(proposed.is_empty(), "{owner:?} must not propose");
            assert!(
                roadmap.roadmap().chunks[0].title_proposal.is_none(),
                "{owner:?}: a proposal landed on the chunk anyway"
            );
        }
    }

    /// The echo fence, inherited: only `report.remote` may mint. A drifted item
    /// is the trap this guards — it is OUR OWN write coming back with its
    /// content mangled by a lossy adapter round trip, so its title genuinely
    /// differs from the plan's, and treating it as a human's retitle would turn
    /// adapter lossage into a stream of phantom proposals.
    #[test]
    fn an_echo_classified_item_cannot_mint_a_title_proposal() {
        let mut roadmap = roadmap_with_link();
        let report = SweepReport {
            fetched: 2,
            echoes: 2,
            drifted: vec![item_titled("Chunk c1 (mangled by the round trip)")],
            ..SweepReport::default()
        };

        let proposed =
            propose_titles_from_sweep(&mut roadmap, "linear", &report, &Ownership::default());

        assert!(proposed.is_empty());
        assert!(
            roadmap.roadmap().chunks[0].title_proposal.is_none(),
            "an echo minted a title proposal — the sweep is proposing our own \
             write back at us"
        );
    }

    #[test]
    fn an_unchanged_remote_title_proposes_nothing() {
        let mut roadmap = roadmap_with_link();

        let proposed = propose_titles_from_sweep(
            &mut roadmap,
            "linear",
            &sweep_finding(item_titled("Chunk c1")),
            &Ownership::default(),
        );

        assert!(proposed.is_empty());
        assert!(roadmap.roadmap().chunks[0].title_proposal.is_none());
    }

    /// A sweep runs every few minutes forever, so the criterion is not "it
    /// proposes" but "proposing twice changes nothing": `proposed_at` must not
    /// advance for an unchanged suggestion, or every old disagreement looks
    /// perpetually new.
    #[test]
    fn re_proposing_the_same_retitle_does_not_restamp() {
        let mut roadmap = roadmap_with_link();
        let report = sweep_finding(item_titled("A human's better name"));

        propose_titles_from_sweep(&mut roadmap, "linear", &report, &Ownership::default());
        let first = roadmap.roadmap().chunks[0]
            .title_proposal
            .as_ref()
            .expect("recorded")
            .proposed_at
            .clone();

        propose_titles_from_sweep(&mut roadmap, "linear", &report, &Ownership::default());
        let second = roadmap.roadmap().chunks[0]
            .title_proposal
            .as_ref()
            .expect("still recorded")
            .proposed_at
            .clone();

        assert_eq!(first, second, "an unchanged suggestion was restamped");
    }
}
