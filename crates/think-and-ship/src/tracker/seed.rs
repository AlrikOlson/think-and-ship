//! Giving a patch-only lane its first identities, without teaching anything
//! here the name of a provider.
//!
//! # The problem this solves
//!
//! [`crate::tracker::project::to_work_item`] resolves an item's `external_id`
//! from `engine.tracker_link(chunk_id, provider)`: present means patch, absent
//! means create. A provider that CAN create needs nothing from this module —
//! its first push mints the identity and records the link.
//!
//! A GitHub Projects v2 board cannot. A board item wraps content that already
//! exists, so `projects_v2` refuses any item whose `external_id` is `None`, and
//! it has no link records of its own until something writes one. Left alone,
//! every first push against a correctly-configured board refuses every chunk,
//! loudly and forever (projects-v2-board-link-seeding).
//!
//! # Why the copy lives here rather than in either adapter
//!
//! The identity the board needs is already recorded — under the ISSUES
//! provider's key, for the same chunk. Reading it from inside the board adapter
//! would be the cross-provider coupling the port seam exists to prevent, and
//! filing both lanes under one provider key would put two link records in a
//! fight over one `external_id`.
//!
//! So the copy happens outside both adapters, in a function that takes BOTH
//! provider keys as PARAMETERS and names neither — the same discipline
//! `registry::build_in` pays. Nothing in this file can learn a provider key it
//! was not told, which is what makes that guarantee structural rather than
//! asserted.

use crate::roadmap::engine::RoadmapEngine;

/// The content hash a seeded link carries, and the reason it is this value.
///
/// [`crate::tracker::project::project_all_with_policy`] skips a chunk whose
/// link's `our_last_write_hash` equals the hash of what it is about to send —
/// "nothing local changed, so do not touch anyone's tracker". A seed that
/// copied the source lane's hash would therefore make the companion's FIRST
/// push skip every chunk: the loud refusal this module removes would become a
/// silent no-op, which is strictly worse, because a refusal at least tells the
/// human what to fix.
///
/// `WorkItem::content_hash` is a SHA-256 rendered as hex and is never empty, so
/// the empty string is a value no real payload can produce. A seeded link
/// therefore always looks like a pending write, exactly once.
pub const NEVER_WRITTEN: &str = "";

/// What one seeding pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Chunks that gained their first link on the destination lane.
    pub seeded: Vec<String>,
    /// Chunks the destination lane already knew — a seed is a FIRST identity
    /// only, so an existing link is never overwritten and never re-stamped.
    pub already_linked: usize,
    /// Chunks opted into the destination lane that the source lane has never
    /// written. Nothing can be seeded from nothing; these are still refused by
    /// the destination adapter, and counting them is how a human learns the
    /// two lanes disagree about scope.
    pub not_yet_upstream: usize,
}

impl SeedReport {
    /// Whether this pass changed anything, i.e. whether the engine needs saving.
    #[must_use]
    pub fn changed(&self) -> bool {
        !self.seeded.is_empty()
    }
}

/// Copy `from`'s item identities onto `to`, for every chunk opted into `to`
/// that does not have one yet.
///
/// Idempotent: a chunk already linked on `to` is left exactly as it is, so
/// running this before every push costs nothing after the first.
///
/// # Errors
///
/// When `from` and `to` are the same lane. The config refuses that at the door
/// too; it is re-checked here because this function is public and a caller that
/// passed the same key twice would silently overwrite the source lane's own
/// links with an unwritten hash, forcing a needless rewrite of every item.
pub fn seed_links_from(
    engine: &mut RoadmapEngine,
    from: &str,
    to: &str,
) -> Result<SeedReport, String> {
    let from = from.trim().to_ascii_lowercase();
    let to = to.trim().to_ascii_lowercase();
    if from.is_empty() || to.is_empty() {
        return Err("seeding needs a source lane and a destination lane".to_string());
    }
    if from == to {
        return Err(format!(
            "'{to}' cannot be seeded from itself — a lane's own links are the ones a seed would \
             overwrite"
        ));
    }

    // Snapshot the work list before mutating: the borrow on the engine ends
    // here, and the set of chunks in the pass must not shift underneath it.
    let planned: Vec<String> = engine
        .chunks_opted_in(&to)
        .into_iter()
        .map(|c| c.id.clone())
        .collect();

    let mut report = SeedReport::default();
    for chunk_id in planned {
        if engine.tracker_link(&chunk_id, &to).is_some() {
            report.already_linked += 1;
            continue;
        }
        let Some(external_id) = engine
            .tracker_link(&chunk_id, &from)
            .map(|l| l.external_id.clone())
        else {
            report.not_yet_upstream += 1;
            continue;
        };
        // The version is deliberately NOT carried across. It is the source
        // lane's record of what IT last saw, and handing it to another provider
        // would make the echo fence there compare against a version that
        // provider never issued.
        engine.record_tracker_link(&chunk_id, &to, &external_id, NEVER_WRITTEN, None)?;
        report.seeded.push(chunk_id);
    }
    Ok(report)
}
