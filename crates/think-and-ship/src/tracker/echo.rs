//! Echo suppression — the one question the inbound path must answer before it
//! does anything else: *is this our own write coming back?*
//!
//! # The failure mode
//!
//! We write to the tracker. The tracker fires a webhook at us. We read it as a
//! remote change and write again. Forever. Every practitioner guide on two-way
//! sync names this first, and it is the reason this module exists before any
//! webhook handler does.
//!
//! # Why the obvious fix is wrong
//!
//! The common defence is to ignore events attributed to our own bot user. It
//! works, and it silently destroys the case that matters most: a **human
//! editing through our integration's token**. Their edit carries our actor, so
//! it is discarded, and nobody is ever told. A support engineer fixing a title
//! through our OAuth app is indistinguishable from our own echo.
//!
//! That is why [`classify`] does not take an actor. Not "does not use one" —
//! does not *accept* one. A function that cannot see who made the change
//! cannot be tempted to decide based on it, and no future edit can quietly
//! reintroduce the heuristic without changing the signature.
//!
//! It takes no clock either. "It arrived within N seconds of our write, so it
//! must be our echo" is a race dressed as a rule, and it fails the moment a
//! provider delivers a webhook late.
//!
//! # The two fences, which are not peers
//!
//! [`TrackerLink`] stores what we last wrote: `our_last_write_hash` and the
//! `last_seen_version` the provider handed back. Both fence an echo, but they
//! are not equally trustworthy, and treating them as interchangeable is a bug:
//!
//! - **The version is the provider's own assertion** that its record has not
//!   moved since our write. It assumes nothing about our code. Strongest
//!   signal available.
//! - **The hash is our opinion** about content, and it quietly assumes the
//!   adapter's inbound parse exactly inverts its outbound build. That
//!   assumption is doing real work and is easy to get wrong: a provider that
//!   returns an assignee's display name where we sent an identifier, or labels
//!   in its own order, or a workflow state collapsed to a coarser type, all
//!   produce a different hash for content nobody changed.
//!
//! Requiring BOTH — the shape this started as — inverts into the very loop it
//! was meant to prevent: if round-trip fidelity is imperfect the hash never
//! matches, nothing is ever an echo, and every event triggers another write.
//! So the fences are consulted in order of strength, not conjoined.
//!
//! # Drift, and why it is worth an enum variant
//!
//! When the provider says its record has not moved and our hash disagrees
//! anyway, that is not ambiguity — it is *proof* that the adapter's round trip
//! is lossy, because nothing changed remotely and yet our two views differ.
//! [`Verdict::EchoWithDrift`] carries that evidence. The event is still
//! suppressed (the version is authoritative), but the fence's one hidden
//! assumption stops being an article of faith and becomes something a test, or
//! a live smoke run, can falsify per provider.

use crate::roadmap::domain::TrackerLink;
use crate::tracker::domain::WorkItem;

/// What an inbound item turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A genuine change from the other side. Process it.
    ///
    /// This is the safe default in every uncertain case: re-reading a change
    /// we already have costs one comparison, while discarding a human's edit
    /// costs their work and tells them nothing.
    Remote,
    /// Our own write coming back. Suppress it.
    Echo,
    /// Our own write coming back, but the adapter's round trip lost something.
    ///
    /// The provider reports the record has not moved since our write, so this
    /// IS an echo and must be suppressed. But the content we read back does not
    /// hash to what we wrote, and since nothing changed remotely the difference
    /// can only have come from the adapter. Suppress, and tell someone.
    EchoWithDrift,
}

impl Verdict {
    /// Whether the inbound item should be suppressed rather than processed.
    #[must_use]
    pub fn is_echo(self) -> bool {
        matches!(self, Self::Echo | Self::EchoWithDrift)
    }
}

/// Decide whether `inbound` is our own write returning, given what we recorded
/// when we wrote it.
///
/// Pure: no I/O, no clock, no actor. A function of exactly two values, so the
/// whole truth table is reachable from a unit test without a tracker.
///
/// `link` is `None` when we have never written to this item. Such an item can
/// never be our echo — we have no write for it to be an echo *of* — so it is
/// always [`Verdict::Remote`].
#[must_use]
pub fn classify(inbound: &WorkItem, link: Option<&TrackerLink>) -> Verdict {
    let Some(link) = link else {
        return Verdict::Remote;
    };

    let content_matches = inbound.content_hash() == link.our_last_write_hash;

    // Versions are OPAQUE tokens — GitHub returns an ETag, Linear an RFC-3339
    // instant, another provider an integer. Compared by EQUALITY and never by
    // ordering: `>` on strings from three different vocabularies is not a
    // comparison, it is a coin flip that looks like one.
    match (&link.last_seen_version, &inbound.version) {
        // The provider says its record is exactly where we left it. Whatever
        // our hash thinks, nothing over there has changed, so this is ours.
        (Some(ours), Some(theirs)) if ours == theirs => {
            if content_matches {
                Verdict::Echo
            } else {
                Verdict::EchoWithDrift
            }
        }
        // The version moved, or one side has none to compare. Fall back to the
        // weaker fence: content identical to our last write is an echo, and
        // anything else is a change we did not make.
        _ => {
            if content_matches {
                Verdict::Echo
            } else {
                Verdict::Remote
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(hash: &str, version: Option<&str>) -> TrackerLink {
        TrackerLink {
            chunk_id: "c1".into(),
            provider: "linear".into(),
            external_id: "ENG-1".into(),
            our_last_write_hash: hash.into(),
            last_seen_version: version.map(str::to_string),
            our_last_relations_hash: None,
            our_last_authored_hash: None,
            created_at: "2026-07-26T10:00:00+00:00".into(),
            updated_at: "2026-07-26T10:00:00+00:00".into(),
        }
    }

    fn item(title: &str, version: Option<&str>) -> WorkItem {
        WorkItem {
            version: version.map(str::to_string),
            ..WorkItem::new(title)
        }
    }

    /// An item we have never written to cannot be an echo of anything.
    #[test]
    fn an_unknown_item_is_always_remote() {
        assert_eq!(
            classify(&item("anything", Some("7")), None),
            Verdict::Remote
        );
    }

    /// The base case: we wrote it, it came straight back untouched.
    #[test]
    fn our_own_write_bouncing_back_is_an_echo() {
        let ours = item("ship the thing", Some("7"));
        let l = link(&ours.content_hash(), Some("7"));
        assert_eq!(classify(&ours, Some(&l)), Verdict::Echo);
    }

    /// THE case the actor heuristic gets wrong, and the reason this module
    /// takes no actor. A human edits the title through OUR OWN OAuth token.
    /// Every actor-based filter sees our bot and discards their work silently.
    #[test]
    fn a_human_editing_through_our_own_token_is_remote() {
        let ours = item("ship the thing", Some("7"));
        let l = link(&ours.content_hash(), Some("7"));

        let theirs = item("ship the thing (Q3)", Some("8"));
        assert_eq!(
            classify(&theirs, Some(&l)),
            Verdict::Remote,
            "a human's edit must survive even when it wears our identity"
        );
    }

    /// The provider's assertion outranks our opinion. If it says the record has
    /// not moved, it has not moved — and a hash that disagrees is evidence
    /// about OUR adapter, not about the record.
    #[test]
    fn an_unchanged_version_with_a_mismatched_hash_is_drift_not_a_remote_change() {
        let l = link("hash-of-what-we-wrote", Some("7"));
        // Same version — the provider swears nothing changed — but the content
        // we read back does not hash to what we wrote. Only the adapter can
        // have done that.
        let readback = item("assignee came back as a display name", Some("7"));

        let verdict = classify(&readback, Some(&l));
        assert_eq!(verdict, Verdict::EchoWithDrift);
        assert!(
            verdict.is_echo(),
            "drift is still an echo — suppressing it is what stops the loop"
        );
    }

    /// Had this been a conjunction (`hash AND version`), the drift case above
    /// would classify as Remote, we would re-project, the version would move,
    /// and the loop this module exists to prevent would run forever. This test
    /// exists to make that regression loud if anyone rewrites the rule.
    #[test]
    fn the_rule_is_not_a_conjunction() {
        let l = link("hash-of-what-we-wrote", Some("7"));
        let drifted = item("not what we wrote", Some("7"));
        assert_ne!(
            classify(&drifted, Some(&l)),
            Verdict::Remote,
            "requiring hash AND version turns imperfect adapter fidelity into an infinite loop"
        );
    }

    /// A provider that bumps its version on a no-op write must not turn our own
    /// content into a remote change — that is the other way to build the loop.
    #[test]
    fn a_version_bump_over_identical_content_is_still_an_echo() {
        let ours = item("ship the thing", Some("7"));
        let l = link(&ours.content_hash(), Some("7"));

        let bumped = item("ship the thing", Some("9"));
        assert_eq!(classify(&bumped, Some(&l)), Verdict::Echo);
    }

    /// Not every provider returns a concurrency token. With no version to
    /// consult, the hash is all there is — and it must still work.
    #[test]
    fn with_no_version_on_either_side_the_hash_decides() {
        let ours = item("ship the thing", None);
        let l = link(&ours.content_hash(), None);
        assert_eq!(classify(&ours, Some(&l)), Verdict::Echo);

        let changed = item("someone else's title", None);
        assert_eq!(classify(&changed, Some(&l)), Verdict::Remote);
    }

    /// One side having a version and the other not is not a match. Treating
    /// `None` as equal to anything would suppress genuine changes.
    #[test]
    fn a_missing_version_never_counts_as_an_unchanged_one() {
        let l = link("hash-of-what-we-wrote", Some("7"));
        let no_version = item("a different title entirely", None);
        assert_eq!(classify(&no_version, Some(&l)), Verdict::Remote);
    }

    /// Identity and the provider's token are excluded from `content_hash`, so
    /// an item that moved id but kept its content is still recognisably ours.
    #[test]
    fn the_verdict_does_not_depend_on_identity() {
        let mut ours = item("ship the thing", Some("7"));
        let l = link(&ours.content_hash(), Some("7"));
        ours.external_id = Some("ENG-999".into());
        assert_eq!(classify(&ours, Some(&l)), Verdict::Echo);
    }
}
