//! Field ownership — who wins when both sides edited the same item.
//!
//! # The failure this exists to prevent
//!
//! Last-write-wins across a whole record is what everyone reaches for, and it
//! is how a two-way sync destroys a person's work. A PM retitles a ticket, the
//! next projection stamps the old title back, and nobody is told. The edit is
//! gone, no error was raised, and the only evidence is a diff nobody reads.
//!
//! # The policy, as data
//!
//! Ownership is a TABLE, not a set of branches, because "who wins" has to be
//! something a user can read and override before they are surprised by it. The
//! default:
//!
//! | Field    | Owner       | Why |
//! |----------|-------------|-----|
//! | Body     | [`Owner::Ours`]       | The description, acceptance checklist and provenance footer ARE the plan. That is the thing this system exists to publish. |
//! | Title    | [`Owner::Contested`]  | A remote retitle is a real statement about the plan, not noise — but so is ours. Nobody wins silently. |
//! | State    | [`Owner::Theirs`]     | Workflow columns belong to the team's process, not to our four canonical states. |
//! | Assignee | [`Owner::Theirs`]     | We never assign anyone. |
//! | Labels   | [`Owner::Theirs`]     | The team's taxonomy. Our own `roadmap:` namespace is carved out at the adapter, which removes only labels it authored. |
//!
//! # What "contested" actually does
//!
//! It does NOT mean "we win" or "they win". When the remote has MOVED, its
//! value stands for that projection and a human is told, because the edit
//! carries information our side does not have.
//!
//! The concession is DURABLE for the title: the projection records it as a
//! title proposal on the chunk, and while that proposal is open the caller
//! passes it back in as `concession`, so every later round keeps deferring
//! even though the remote is now unmoved (it holds the conceded value). A
//! human resolves the proposal — accept adopts the tracker's title into the
//! plan, reject clears it and lets the plan's title flow again. An unmoved
//! remote with NO open concession still does not win, or an ordinary local
//! rename could never be pushed at all.
//!
//! The honest remainder: a per-project override that marks a field OTHER than
//! the title as contested still has the one-round deferral — only the title
//! has a durable concession carrier today.
//!
//! # Why a newtype guards this
//!
//! A policy consulted by convention is a policy the next edit forgets. So the
//! patch path cannot take a bare [`WorkItem`]: it takes [`Reconciled`], whose
//! inner value is private and which only [`reconcile_fields`] can construct. Producing
//! one requires passing the policy and the remote item. Adding a new write path
//! that skips the policy is therefore not a review comment — it does not
//! compile.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tracker::domain::WorkItem;

/// The fields of a [`WorkItem`] that ownership applies to.
///
/// Deliberately not every struct field: `external_id` and `version` are
/// identity and concurrency tokens, not authored content, and nobody "owns"
/// them in the sense this module means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Body,
    State,
    Labels,
    Assignee,
}

impl Field {
    /// Every field ownership can speak about. Used by the table's `Default` and
    /// by the test that proves no field is silently unaddressed.
    pub const ALL: [Field; 5] = [
        Field::Title,
        Field::Body,
        Field::State,
        Field::Labels,
        Field::Assignee,
    ];

    /// The name a human sees in a concern signal.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Body => "body",
            Field::State => "state",
            Field::Labels => "labels",
            Field::Assignee => "assignee",
        }
    }
}

/// Who authors a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// We author it. Our value is sent. A remote edit is overridden — and
    /// REPORTED, because someone typed it and deserves to know it did not last.
    Ours,
    /// The team authors it. We never send it, and their value stands untouched.
    Theirs,
    /// Both have a claim. A MOVED remote's value stands for that projection and
    /// a human is told — not a tie-break, but an admission that the machine
    /// should not pick. An unmoved remote holds our own last write, so it does
    /// not win on its own; deferring to it would block ordinary local renames.
    /// A recorded concession (an open title proposal, passed back in by the
    /// caller) is what makes the deferral survive later rounds — see the
    /// module docs.
    Contested,
}

/// The ownership table for one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ownership {
    #[serde(default)]
    by_field: BTreeMap<Field, Owner>,
}

impl Default for Ownership {
    /// The documented default. Every field is listed explicitly — an absent
    /// field would be an unstated rule, which is the thing this type exists to
    /// prevent.
    fn default() -> Self {
        Self {
            by_field: [
                (Field::Body, Owner::Ours),
                (Field::Title, Owner::Contested),
                (Field::State, Owner::Theirs),
                (Field::Assignee, Owner::Theirs),
                (Field::Labels, Owner::Theirs),
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl Ownership {
    /// Who owns `field`.
    ///
    /// An unlisted field is [`Owner::Contested`], never `Ours`. A rule nobody
    /// wrote down must not license an overwrite — the safe direction for a gap
    /// in the table is to defer and tell someone.
    #[must_use]
    pub fn owner(&self, field: Field) -> Owner {
        self.by_field
            .get(&field)
            .copied()
            .unwrap_or(Owner::Contested)
    }

    /// Override one field for this project.
    #[must_use]
    pub fn with(mut self, field: Field, owner: Owner) -> Self {
        self.by_field.insert(field, owner);
        self
    }

    /// The whole table, for display. This is what makes the policy inspectable
    /// rather than merely documented.
    #[must_use]
    pub fn rows(&self) -> Vec<(Field, Owner)> {
        Field::ALL.iter().map(|f| (*f, self.owner(*f))).collect()
    }
}

/// A digest of ONLY the fields this policy lets us author.
///
/// # Why this exists, and what it is not
///
/// [`WorkItem::content_hash`] covers everything a provider stores, including
/// the fields the table hands to the team — `state`, `labels` and `assignee`
/// are all [`Owner::Theirs`] by default and all inside that digest. That is
/// correct for the echo fence, whose question is "is this inbound event the
/// bytes we last wrote?", and the answer has to include bytes we merely
/// carried.
///
/// It is the wrong digest for a different question: "if we projected now,
/// would we write anything?" A field the team owns can never make us write,
/// because [`reconcile_fields`] replaces our value with theirs before the
/// item is sent. Comparing the full hash to answer that question reports a
/// pending write for every chunk whose Linear state or label has moved on
/// without us — a preview that is wrong in the direction that manufactures
/// phantom work, which is exactly the bug this function was written for.
///
/// So: neutralize every `Theirs` field to a fixed value, then hash. What
/// survives is what we actually author, and two items that agree here agree
/// on everything we would ever send.
///
/// # Not a hypothesis about the initiative
///
/// An earlier diagnosis guessed that the roof initiative participated in the
/// hash and that the preview and the projector therefore hashed different
/// bodies. That guess is REFUTED and must not be re-chased: the initiative is
/// absent from `content_hash`, and `to_work_item` takes no initiative argument
/// on either path. The initiative only ever names a container in phase −1; it
/// never touches a [`WorkItem`].
///
/// # Derived from the table, not restated
///
/// The neutralization reads `policy.owner(..)` rather than hardcoding the
/// default table, so a project that reassigns a field gets a digest that
/// follows. A hardcoded list would silently answer for the wrong policy.
#[must_use]
pub fn authored_hash(policy: &Ownership, item: &WorkItem) -> String {
    use crate::tracker::domain::WorkItemState;

    let mut authored = item.clone();
    // Identity and the concurrency token are not authored content and are
    // already excluded by `content_hash`; nothing to do for them here.
    if policy.owner(Field::Title) == Owner::Theirs {
        authored.title.clear();
    }
    if policy.owner(Field::Body) == Owner::Theirs {
        authored.body.clear();
    }
    if policy.owner(Field::State) == Owner::Theirs {
        authored.state = WorkItemState::Todo;
    }
    if policy.owner(Field::Labels) == Owner::Theirs {
        authored.labels.clear();
    }
    if policy.owner(Field::Assignee) == Owner::Theirs {
        authored.assignee = None;
    }
    // `group` is deliberately NOT neutralized: the ownership table has nothing
    // to say about it and `reconcile_fields` never touches it, so the group we
    // compute is always the group we send. Leaving it in is what makes a
    // regrouped chunk show up as a real pending write rather than as noise.
    authored.content_hash()
}

/// A field where the two sides disagreed and the disagreement matters.
///
/// Not every difference is one of these: a field the tracker owns differing
/// from ours is normal and expected, and reporting it would be noise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub field: Field,
    pub owner: Owner,
    /// What we would have written.
    pub ours: String,
    /// What the tracker holds.
    pub theirs: String,
}

impl Divergence {
    /// A sentence for a human, which is the whole point of raising it.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.owner {
            Owner::Contested => format!(
                "The {} differs and neither side owns it. The tracker says {:?}; the plan says {:?}. \
                 The tracker's value was kept.",
                self.field.as_str(),
                self.theirs,
                self.ours
            ),
            Owner::Ours => format!(
                "The {} was edited in the tracker to {:?}, but the plan owns that field, so {:?} \
                 was written back over it.",
                self.field.as_str(),
                self.theirs,
                self.ours
            ),
            // Not raised today — a field they own diverging is normal — but the
            // arm exists so a future table change cannot produce an unworded
            // concern.
            Owner::Theirs => format!(
                "The {} differs, and the tracker owns it, so their value stands.",
                self.field.as_str()
            ),
        }
    }
}

/// An item that has been through the policy. The patch path takes this, not a
/// bare [`WorkItem`], so skipping the policy is a compile error rather than a
/// review comment.
#[derive(Debug, Clone)]
pub struct Reconciled {
    item: WorkItem,
    divergences: Vec<Divergence>,
}

impl Reconciled {
    /// The item to send.
    #[must_use]
    pub fn item(&self) -> &WorkItem {
        &self.item
    }

    /// Disagreements a human should hear about.
    #[must_use]
    pub fn divergences(&self) -> &[Divergence] {
        &self.divergences
    }
}

/// Merge what we would project with what the tracker currently holds.
///
/// THE ONLY constructor of [`Reconciled`], which is what makes the policy
/// unavoidable rather than advisory.
///
/// `theirs` is `None` on a create — there is nothing to conflict with, so our
/// item passes through whole and no divergence is possible.
///
/// `remote_moved` gates REPORTING only, never merging. When the provider says
/// its record has not moved since our write, the item we fetched is our own
/// last write, so a difference is our pending change rather than someone's
/// edit and is not news. The values are merged either way — a contested field
/// that stopped deferring the moment the remote went quiet would be re-asserted
/// on the very next projection.
///
/// `concession` is the open title proposal's suggested title, when the chunk
/// has one — the durable memory that we already conceded this exact contest.
/// While present, the contested title keeps deferring even though the remote
/// is unmoved (it holds the conceded value, which IS our last write). It also
/// covers the fetch-failure hole: with `theirs` absent but a concession open,
/// sending the plan's title would clobber the human's — so the concession is
/// applied even on the no-remote path.
#[must_use]
pub fn reconcile_fields(
    policy: &Ownership,
    ours: &WorkItem,
    theirs: Option<&WorkItem>,
    remote_moved: bool,
    concession: Option<&str>,
) -> Reconciled {
    let Some(theirs) = theirs else {
        let mut item = ours.clone();
        if policy.owner(Field::Title) == Owner::Contested
            && let Some(c) = concession
        {
            item.title = c.to_string();
        }
        return Reconciled {
            item,
            divergences: Vec::new(),
        };
    };

    let mut item = ours.clone();
    let mut divergences = Vec::new();

    // Identity and the concurrency token always come from the remote side: they
    // are not authored content and ownership has nothing to say about them.
    item.external_id = theirs.external_id.clone().or(item.external_id);
    item.version = theirs.version.clone();

    // MERGING and REPORTING are separate questions, and conflating them was a
    // bug. Values are ALWAYS merged: a contested field must keep deferring to
    // the remote on every projection, or we defer once and overwrite on the
    // next run — the plan wins after two rounds and "contested" means nothing.
    //
    // Reporting is gated on the remote having actually MOVED. Without that,
    // every ordinary plan edit reports itself as a human's edit being
    // overwritten, because the remote holds our own previous write, and the
    // concern channel fills with our own noise.
    let mut note = |field: Field, owner: Owner, a: String, b: String| {
        if a != b && remote_moved {
            divergences.push(Divergence {
                field,
                owner,
                ours: a,
                theirs: b,
            });
        }
    };

    match policy.owner(Field::Title) {
        Owner::Ours => note(
            Field::Title,
            Owner::Ours,
            ours.title.clone(),
            theirs.title.clone(),
        ),
        Owner::Theirs => {
            note(
                Field::Title,
                Owner::Theirs,
                ours.title.clone(),
                theirs.title.clone(),
            );
            item.title = theirs.title.clone();
        }
        // CONTESTED defers when the remote actually MOVED — an unmoved remote
        // holds our own previous write, so deferring to it on its own would be
        // deferring to ourselves, and a plain local rename, which nobody
        // contested, would never reach the tracker. The exception is a
        // recorded CONCESSION: an open title proposal means a human's retitle
        // was already deferred to, and it keeps winning until a human resolves
        // the proposal — that memory is what stops the plan re-asserting on
        // the round after the concession.
        Owner::Contested => {
            note(
                Field::Title,
                Owner::Contested,
                ours.title.clone(),
                theirs.title.clone(),
            );
            if remote_moved {
                item.title = theirs.title.clone();
            } else if let Some(c) = concession {
                item.title = c.to_string();
            }
        }
    }

    match policy.owner(Field::Body) {
        Owner::Ours => note(
            Field::Body,
            Owner::Ours,
            ours.body.clone(),
            theirs.body.clone(),
        ),
        owner @ (Owner::Theirs | Owner::Contested) => {
            note(Field::Body, owner, ours.body.clone(), theirs.body.clone());
            item.body = theirs.body.clone();
        }
    }

    match policy.owner(Field::State) {
        Owner::Ours => note(
            Field::State,
            Owner::Ours,
            format!("{:?}", ours.state),
            format!("{:?}", theirs.state),
        ),
        Owner::Theirs => item.state = theirs.state,
        Owner::Contested => {
            note(
                Field::State,
                Owner::Contested,
                format!("{:?}", ours.state),
                format!("{:?}", theirs.state),
            );
            item.state = theirs.state;
        }
    }

    match policy.owner(Field::Assignee) {
        Owner::Ours => note(
            Field::Assignee,
            Owner::Ours,
            ours.assignee.clone().unwrap_or_default(),
            theirs.assignee.clone().unwrap_or_default(),
        ),
        Owner::Theirs => item.assignee = theirs.assignee.clone(),
        Owner::Contested => {
            note(
                Field::Assignee,
                Owner::Contested,
                ours.assignee.clone().unwrap_or_default(),
                theirs.assignee.clone().unwrap_or_default(),
            );
            item.assignee = theirs.assignee.clone();
        }
    }

    // Labels are the one field where both sides legitimately hold values at
    // once: ours live under the `roadmap:` namespace and theirs outside it. So
    // "they own labels" means keep every label of theirs AND keep ours, rather
    // than replace one set with the other. The adapter removes only labels from
    // our own namespace, which is what makes this safe.
    match policy.owner(Field::Labels) {
        Owner::Ours => note(
            Field::Labels,
            Owner::Ours,
            ours.labels.join(","),
            theirs.labels.join(","),
        ),
        owner @ (Owner::Theirs | Owner::Contested) => {
            let mut merged = ours.labels.clone();
            for l in &theirs.labels {
                if !l.starts_with(BAND_PREFIX) && !merged.contains(l) {
                    merged.push(l.clone());
                }
            }
            if merged != ours.labels && owner == Owner::Contested {
                note(
                    Field::Labels,
                    owner,
                    ours.labels.join(","),
                    theirs.labels.join(","),
                );
            }
            item.labels = merged;
        }
    }

    Reconciled { item, divergences }
}

/// The namespace this system authors labels in. Mirrors the adapter's own
/// constant; a label outside it is the team's and is never ours to drop.
const BAND_PREFIX: &str = "roadmap:";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::domain::WorkItemState;

    fn ours() -> WorkItem {
        let mut i = WorkItem::new("our title");
        i.body = "our body".into();
        i.state = WorkItemState::Todo;
        i.labels = vec!["roadmap:high".into()];
        i
    }

    fn theirs() -> WorkItem {
        let mut i = WorkItem::new("their title");
        i.body = "their body".into();
        i.state = WorkItemState::InProgress;
        i.labels = vec!["roadmap:high".into(), "Bug".into()];
        i.assignee = Some("Ada".into());
        i.version = Some("v7".into());
        i
    }

    #[test]
    fn the_default_table_speaks_about_every_field() {
        let p = Ownership::default();
        assert_eq!(p.rows().len(), Field::ALL.len());
        assert!(
            p.rows().iter().all(|(_, o)| *o != Owner::Contested
                || matches!(p.owner(Field::Title), Owner::Contested)),
            "the table must be complete; an unlisted field is an unstated rule"
        );
    }

    /// A gap in the table must never license an overwrite.
    #[test]
    fn an_unlisted_field_is_contested_not_ours() {
        let empty = Ownership {
            by_field: BTreeMap::new(),
        };
        for f in Field::ALL {
            assert_eq!(empty.owner(f), Owner::Contested, "{f:?}");
        }
    }

    /// A create has nothing to conflict with.
    #[test]
    fn a_create_passes_through_whole() {
        let r = reconcile_fields(&Ownership::default(), &ours(), None, true, None);
        assert_eq!(r.item().title, "our title");
        assert!(r.divergences().is_empty());
    }

    /// The fields the tracker owns survive untouched — this is the promise.
    #[test]
    fn fields_the_tracker_owns_are_not_clobbered() {
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&theirs()), true, None);
        assert_eq!(r.item().state, WorkItemState::InProgress, "their column");
        assert_eq!(r.item().assignee.as_deref(), Some("Ada"), "their person");
        assert!(
            r.item().labels.contains(&"Bug".to_string()),
            "their taxonomy survives: {:?}",
            r.item().labels
        );
    }

    /// The body is ours, so ours is written — AND the discarded edit is
    /// reported, because somebody typed it.
    #[test]
    fn a_field_we_own_is_overwritten_but_the_edit_is_reported() {
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&theirs()), true, None);
        assert_eq!(r.item().body, "our body", "we own the plan");
        let d = r
            .divergences()
            .iter()
            .find(|d| d.field == Field::Body)
            .expect("the overwritten edit must be reported, not silently dropped");
        assert_eq!(d.owner, Owner::Ours);
        assert!(d.summary().contains("written back over it"));
    }

    /// Contested does not mean we win. The remote value STANDS and a human is
    /// told, because the edit carries information we do not have.
    #[test]
    fn a_contested_field_defers_to_the_tracker_and_raises_a_concern() {
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&theirs()), true, None);
        assert_eq!(
            r.item().title,
            "their title",
            "a remote retitle is a statement about the plan; overwriting it destroys the statement"
        );
        let d = r
            .divergences()
            .iter()
            .find(|d| d.field == Field::Title)
            .expect("a contested difference must reach a human");
        assert_eq!(d.owner, Owner::Contested);
        assert!(d.summary().contains("neither side owns it"));
    }

    /// Agreement is not a conflict. Identical values raise nothing.
    #[test]
    fn matching_values_raise_no_concern() {
        let same = ours();
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&same), true, None);
        assert!(r.divergences().is_empty(), "got {:?}", r.divergences());
    }

    /// The table is overridable per project, which is what "inspectable and
    /// overridable" has to mean in practice.
    #[test]
    fn a_project_can_take_the_title_back() {
        let policy = Ownership::default().with(Field::Title, Owner::Ours);
        let r = reconcile_fields(&policy, &ours(), Some(&theirs()), true, None);
        assert_eq!(r.item().title, "our title");
        assert_eq!(
            r.divergences()
                .iter()
                .find(|d| d.field == Field::Title)
                .map(|d| d.owner),
            Some(Owner::Ours)
        );
    }

    /// The concurrency token always comes from the remote side — it is the
    /// fence the echo classifier reads, and ours is stale by definition.
    #[test]
    fn the_version_always_comes_from_the_tracker() {
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&theirs()), true, None);
        assert_eq!(r.item().version.as_deref(), Some("v7"));
    }

    /// The durable half of contested: an UNMOVED remote (it holds our own
    /// round-1 concession) keeps winning while the concession is open, and no
    /// divergence is re-raised — the same disagreement must not re-notify.
    #[test]
    fn an_open_concession_keeps_deferring_when_the_remote_is_unmoved() {
        let mut remote = ours();
        remote.title = "their title".into();
        let r = reconcile_fields(
            &Ownership::default(),
            &ours(),
            Some(&remote),
            false,
            Some("their title"),
        );
        assert_eq!(
            r.item().title,
            "their title",
            "the concession is the memory; without it the plan re-asserts here"
        );
        assert!(
            r.divergences().is_empty(),
            "an already-conceded contest is not news: {:?}",
            r.divergences()
        );
    }

    /// No concession, unmoved remote: an ordinary local rename flows. This is
    /// the regression 6b caught — deferring to an unmoved remote is deferring
    /// to ourselves.
    #[test]
    fn without_a_concession_an_unmoved_remote_does_not_win() {
        let mut remote = ours();
        remote.title = "the old title we wrote".into();
        let r = reconcile_fields(&Ownership::default(), &ours(), Some(&remote), false, None);
        assert_eq!(r.item().title, "our title");
    }

    /// A remote that moved AGAIN outranks the recorded concession: the fresh
    /// edit is a different disagreement and the caller replaces the proposal.
    #[test]
    fn a_freshly_moved_remote_outranks_the_stored_concession() {
        let mut remote = ours();
        remote.title = "an even newer title".into();
        let r = reconcile_fields(
            &Ownership::default(),
            &ours(),
            Some(&remote),
            true,
            Some("their title"),
        );
        assert_eq!(r.item().title, "an even newer title");
    }

    /// The fetch-failure hole: no remote snapshot but an open concession must
    /// not send the plan's title — that would clobber the human's retitle the
    /// moment the provider hiccuped.
    #[test]
    fn a_concession_survives_a_missing_remote_snapshot() {
        let r = reconcile_fields(
            &Ownership::default(),
            &ours(),
            None,
            false,
            Some("their title"),
        );
        assert_eq!(r.item().title, "their title");
    }
}
