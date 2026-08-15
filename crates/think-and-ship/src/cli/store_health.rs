//! Reading the roadmap store, and — carefully — removing what doesn't belong.
//!
//! `doctor` used to check PATH and MCP config and never look at the data, so
//! the roadmap could be visibly wrong while doctor reported everything fine:
//! this project's store still holds another project's chunks, and `roadmap next`
//! answered with one of them.
//!
//! # Why pruning needs rules, not just a delete
//!
//! Until now the local store had no record-intrinsic origin. The cloud envelope
//! stamped `record.project_id` on the wire, but the local copy dropped it, so
//! the only attribution was the file a chunk happened to live in. That's why the
//! one-shot cleanup script had to guess from id prefixes.
//!
//! [`Chunk::project_id`] now carries the stamp, which splits every chunk into
//! three cases — and the middle one is the whole reason this module is careful:
//!
//! | Stamp | Meaning | Prune |
//! |---|---|---|
//! | `Some(ours)` | provably this project's | never |
//! | `None` | recorded before the stamp existed — **origin unprovable** | only when the operator names it explicitly |
//! | `Some(other)` | provably another project's | yes |
//!
//! Deleting on the strength of a `None` would mean deleting work whose origin we
//! cannot establish. The failure mode that matters here is not "residue
//! survives"; it's "someone's real work is gone".
//!
//! # The middle row is a one-way door without adoption
//!
//! Pruning can delete a record *out* of the unprovable row and nothing else
//! moves it, so a store that was bled into once keeps that row forever: every
//! future prune re-asks the same undecidable question, the operator re-types the
//! same `--matching` list, and the next bleed arrives among hundreds of records
//! nobody can attribute. [`adoptable_records`] is the missing write — a cleaned
//! store stamping its own unprovable records as its own, once, so the table can
//! answer for them from then on. It refuses while anything provably foreign is
//! still present, because adoption over a live bleed would stamp the intruder's
//! work as ours and erase the evidence.
//!
//! # One table, three families
//!
//! The table above is about a record's *origin*, not about roadmaps, so it is
//! implemented exactly once over [`Owned`] and shared by roadmap chunks, think
//! steps and signals. A per-family copy is how the middle row quietly goes
//! missing from one family — which is the only row whose absence deletes
//! someone's work.
//!
//! Each family differs only in how it answers "who owns this record", and that
//! difference is confined to its [`Owned::owner`] impl:
//!
//! | Family | Origin signal | Notes |
//! |---|---|---|
//! | roadmap | `Chunk::project_id` | stamped at record since roadmap-store-health |
//! | think | `ThinkStep::cwd` → [`crate::infra::project_id_for_path`] | no stamp needed; see [`ThinkOrigin`] and [`cwd_attribution_is_proof`] for the guard that makes this safe |
//! | signal | `Signal::project_id` | local-only stamp; every pre-existing signal is unstamped, hence unprovable, hence kept |

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::roadmap::domain::{Chunk, Roadmap};

/// A stored record that can say which project it came from.
///
/// The whole point of the abstraction is that `owner` returns an `Option`: the
/// `None` case is the unprovable-origin row of the table, and it is the row that
/// keeps real work alive. An impl that manufactures an owner it cannot prove
/// breaks the guarantee for its whole family.
pub trait Owned {
    /// Stable identifier, used to report and to match `--matching` prefixes.
    fn record_id(&self) -> String;
    /// The project this record provably came from, or `None` when that cannot
    /// be established. Never guess here.
    fn owner(&self) -> Option<String>;
}

impl Owned for Chunk {
    fn record_id(&self) -> String {
        self.id.clone()
    }
    fn owner(&self) -> Option<String> {
        self.project_id.clone()
    }
}

impl Owned for crate::signal::domain::Signal {
    fn record_id(&self) -> String {
        self.id.clone()
    }
    fn owner(&self) -> Option<String> {
        self.project_id.clone()
    }
}

/// Is a cwd-derived project id *proof* of origin in this process?
///
/// Only when our own id is itself cwd-derived. [`crate::infra::resolve_project_id`]
/// checks env overrides FIRST and only then hashes the cwd, so a project running
/// under a name override has an id that no step's cwd will ever reproduce —
/// naive attribution would call this project's entire trace foreign and offer to
/// delete it.
///
/// When this returns false, [`ThinkOrigin::owner`] yields `None`: unprovable,
/// therefore kept. Every uncertainty falls toward keeping the record.
pub fn cwd_attribution_is_proof(project_id: &str) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let path = cwd.canonicalize().unwrap_or(cwd);
    crate::infra::project_id_for_path(&path) == project_id
}

/// A think step viewed as an [`Owned`] record.
///
/// Think steps carry no origin stamp; they carry the `cwd` they were recorded
/// from, which [`crate::infra::project_id_for_path`] exists to attribute (its
/// own doc names think steps as the caller). That is why this family needs no
/// schema change — and why the historical steps a prune actually cares about
/// can be attributed at all, which a new stamp could never do.
pub struct ThinkOrigin<'a> {
    pub step: &'a crate::think::domain::ThinkStep,
    /// Whether cwd attribution counts as proof — see [`cwd_attribution_is_proof`].
    pub cwd_is_proof: bool,
}

impl Owned for ThinkOrigin<'_> {
    fn record_id(&self) -> String {
        self.step.step_number.to_string()
    }
    fn owner(&self) -> Option<String> {
        if !self.cwd_is_proof {
            return None;
        }
        let cwd = self
            .step
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        Some(crate::infra::project_id_for_path(Path::new(cwd)))
    }
}

/// What a read-only pass over the roadmap store found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StoreReport {
    /// Chunks stamped with a different project — provably not ours.
    pub foreign: Vec<String>,
    /// Chunks with no stamp at all: recorded before the stamp existed, or
    /// merged in when the stamp was being dropped. Origin unprovable.
    pub unstamped: usize,
    /// `(chunk, dep)` pairs where `dep` names a chunk that doesn't exist.
    pub dangling_deps: Vec<(String, String)>,
    /// Total chunks inspected.
    pub total: usize,
}

impl StoreReport {
    /// Everything a human needs to act on. `unstamped` alone isn't a problem —
    /// every chunk recorded before the stamp existed is unstamped, and they're
    /// almost all legitimately ours.
    pub fn has_findings(&self) -> bool {
        !self.foreign.is_empty() || !self.dangling_deps.is_empty()
    }
}

/// Inspect a roadmap for records that don't belong to `project_id` and for deps
/// pointing at chunks that aren't there. Pure — takes the loaded roadmap, reads
/// nothing, writes nothing.
pub fn inspect(roadmap: &Roadmap, project_id: &str) -> StoreReport {
    let mut report = inspect_records(&roadmap.chunks, project_id);

    // Dangling deps are a roadmap-only concern: only chunks reference each
    // other by id. Think steps and signals have no equivalent.
    let known: BTreeSet<&str> = roadmap.chunks.iter().map(|c| c.id.as_str()).collect();
    for chunk in &roadmap.chunks {
        for dep in &chunk.deps {
            if !known.contains(dep.as_str()) {
                report.dangling_deps.push((chunk.id.clone(), dep.clone()));
            }
        }
    }
    report
}

/// The origin half of [`inspect`], over any family of [`Owned`] records.
/// This is the one implementation of the ownership table.
pub fn inspect_records<T: Owned>(records: &[T], project_id: &str) -> StoreReport {
    let mut report = StoreReport {
        total: records.len(),
        ..Default::default()
    };
    for record in records {
        match record.owner().as_deref() {
            Some(owner) if owner != project_id => report.foreign.push(record.record_id()),
            Some(_) => {}
            None => report.unstamped += 1,
        }
    }
    report
}

/// Which chunks a prune would remove, given the operator's intent.
///
/// `id_prefixes` is the escape hatch for records the stamp can't speak for: the
/// operator states, explicitly, which unstamped ids are foreign. Nothing is
/// removed on a guess the code made by itself.
pub fn prunable(roadmap: &Roadmap, project_id: &str, id_prefixes: &[String]) -> Vec<String> {
    prunable_records(&roadmap.chunks, project_id, id_prefixes)
}

/// [`prunable`] over any family of [`Owned`] records — the shared decision.
pub fn prunable_records<T: Owned>(
    records: &[T],
    project_id: &str,
    id_prefixes: &[String],
) -> Vec<String> {
    records
        .iter()
        .filter(|r| {
            is_prunable(
                r.record_id().as_str(),
                r.owner().as_deref(),
                project_id,
                id_prefixes,
            )
        })
        .map(|r| r.record_id())
        .collect()
}

/// THE ownership table, in code, once. Every family routes through here.
fn is_prunable(
    record_id: &str,
    owner: Option<&str>,
    project_id: &str,
    id_prefixes: &[String],
) -> bool {
    match owner {
        // Provably ours. Never, regardless of what the operator typed — a
        // prefix match must not outrank proof of ownership.
        Some(owner) if owner == project_id => false,
        // Provably someone else's.
        Some(_) => true,
        // Unprovable: only when the operator named it.
        None => {
            !id_prefixes.is_empty()
                && id_prefixes
                    .iter()
                    .any(|p| record_id.starts_with(p.as_str()))
        }
    }
}

/// The refusal shared by every family's adoption. One builder, so a family
/// that grows its own door cannot grow its own wording with it.
fn adopt_refusal(project_id: &str, foreign: &[String]) -> String {
    let named: Vec<&str> = foreign.iter().take(3).map(String::as_str).collect();
    let more = foreign.len().saturating_sub(named.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!(
        "refusing to adopt: this store still holds {} record(s) that provably belong to another \
         project ({}{tail}). Adoption stamps unprovable records as {project_id}'s, so running it \
         over a store that is still polluted would convert someone else's work into ours and make \
         the mistake permanent. Prune first, then adopt.",
        foreign.len(),
        named.join(", "),
    )
}

/// Which records an adoption would stamp as `project_id`'s — the third
/// operation on the ownership table, and the only one that writes to the origin
/// column.
///
/// Without it the unprovable row is a one-way door: [`prunable`] can delete out
/// of it and nothing can leave it any other way, so a store that was once bled
/// into keeps hundreds of records no future prune can decide, and the operator
/// re-types the same `--matching` list forever. Adoption is how a store that
/// has been cleaned says so, once.
///
/// Two rules, and the asymmetry with `is_prunable` is deliberate:
///
/// - A record with ANY stamp is left alone. Ours needs nothing; another
///   project's must never be overwritten — that would manufacture the very
///   proof this module exists to keep honest.
/// - An unstamped record is adopted by default, where a prune would demand the
///   operator name it. The directions are not symmetric: guessing wrong on a
///   prune destroys someone's work, guessing wrong on an adoption relabels a
///   record the store already holds. `id_prefixes` narrows it when the operator
///   wants to claim only part of the store.
///
/// # Errors
///
/// Refuses outright while the store still holds a provably-foreign record.
/// Adopting over a live bleed is the one way this verb can do real damage: it
/// would stamp the bled records as ours and destroy the evidence that they
/// aren't. Prune first.
pub fn adoptable_records<T: Owned>(
    records: &[T],
    project_id: &str,
    id_prefixes: &[String],
) -> Result<Vec<String>> {
    let report = inspect_records(records, project_id);
    if !report.foreign.is_empty() {
        anyhow::bail!(adopt_refusal(project_id, &report.foreign));
    }
    Ok(records
        .iter()
        .filter(|r| is_adoptable(r.record_id().as_str(), r.owner().as_deref(), id_prefixes))
        .map(|r| r.record_id())
        .collect())
}

/// One record id held by more than one project's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossStoreDuplicate {
    /// The record id sitting in two or more stores.
    pub id: String,
    /// Every `(store's project, that copy's stamp)`, in store order.
    pub holders: Vec<(String, Option<String>)>,
}

impl CrossStoreDuplicate {
    /// The stores whose copy claims THEM as the owner. When more than one store
    /// says that about the same record, at least one of them is lying.
    #[must_use]
    pub fn self_claiming(&self) -> Vec<&str> {
        self.holders
            .iter()
            .filter(|(store, stamp)| stamp.as_deref() == Some(store.as_str()))
            .map(|(store, _)| store.as_str())
            .collect()
    }
}

/// Find records that exist in more than one project's store — the ONE signal
/// that can see a mis-stamp whose own record is perfectly self-consistent.
///
/// # Why this cannot be answered inside a single store
///
/// [`inspect_records`] sorts by the record's own stamp, and that is right for
/// every honest case. It is defeated by exactly one: a record that bled in from
/// another project and was later stamped with the HOST's id. It then reads as
/// `Some(ours)` — the row [`prunable`] must never touch — and every in-store
/// check agrees it belongs. That is not a flaw in the table; a store simply does
/// not contain the evidence. Ownership is contradicted only from OUTSIDE, by the
/// same id living somewhere else.
///
/// This is the shape the 2026-06 bleed left behind: 22 chunks in one project's
/// store carrying that project's stamp while the same ids, same `created_at`,
/// lived stamped correctly in the project that authored them.
///
/// # What it does NOT do
///
/// It does not decide who is right, and it must not. Two stores holding one id
/// is a CONTRADICTION, not a verdict — a shared id can also be an honest
/// collision (`phase-1`, `scaffold` and `design-tokens` genuinely recur across
/// unrelated projects here). Resolving it stays an operator's call with
/// `--matching`, exactly as [`prunable`] requires, because the failure that
/// matters is never "residue survived", it is "someone's real work is gone".
///
/// Takes the stores as a PARAMETER rather than reading the sessions directory,
/// so the rule can be driven by tables production has never seen.
pub fn cross_store_duplicates<T: Owned>(stores: &[(String, Vec<T>)]) -> Vec<CrossStoreDuplicate> {
    let mut seen: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    for (project, records) in stores {
        for record in records {
            seen.entry(record.record_id())
                .or_default()
                .push((project.clone(), record.owner()));
        }
    }
    seen.into_iter()
        .filter(|(_, holders)| holders.len() > 1)
        .map(|(id, holders)| CrossStoreDuplicate { id, holders })
        .collect()
}

/// Load every project's roadmap store out of a sessions directory.
///
/// The filesystem half of [`cross_store_duplicates`], kept separate so the rule
/// itself can be driven by tables that never existed on disk. A store that fails
/// to parse is SKIPPED rather than fatal: this runs inside `doctor`, and a
/// diagnostic that dies on one unreadable file tells you nothing about the other
/// fifty-seven.
pub fn load_all_roadmap_stores(sessions_dir: &Path) -> Vec<(String, Vec<Chunk>)> {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };
    let mut stores: Vec<(String, Vec<Chunk>)> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let project = path.file_stem()?.to_string_lossy().to_string();
            let text = std::fs::read_to_string(&path).ok()?;
            let roadmap: Roadmap = serde_json::from_str(&text).ok()?;
            Some((project, roadmap.chunks))
        })
        .collect();
    stores.sort_by(|a, b| a.0.cmp(&b.0));
    stores
}

/// Every project id with a store on this machine: the union of `<id>.json`
/// stems across the given sessions directories, sorted.
///
/// The filesystem half of `sync push --all-projects`, separated the way
/// [`load_all_roadmap_stores`] separates from [`cross_store_duplicates`] so it
/// can be driven against directories production has never seen. Entries that
/// are not project stores are excluded by *shape*, never by a name list:
/// lock and backup files fail the `.json` extension test, `_default` and the
/// `_legacy` directory start with `_`, and a think session file namespaced
/// `<project>__<session>.json` contributes its project half. A directory that
/// cannot be read contributes nothing — enumeration answers for what is
/// actually there.
pub fn project_ids_in(sessions_dirs: &[PathBuf]) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for dir in sessions_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.starts_with('_') {
                continue;
            }
            let project = stem
                .split(crate::infra::project_id::PROJECT_SEP)
                .next()
                .unwrap_or(stem);
            if !project.is_empty() {
                ids.insert(project.to_string());
            }
        }
    }
    ids.into_iter().collect()
}

/// The fourth row of the table, and the only one that can remove a record this
/// project claims: `Some(ours)` where ANOTHER store claims the same id.
///
/// Every other rule reads a record's own stamp, and a mis-stamped record passes
/// them all — it says it belongs where it sits. That is why the 22 chunks the
/// 2026-06 bleed left behind survived `prune roadmap`, which answered "Nothing
/// to remove" while looking straight at them.
///
/// Two conditions, and BOTH are required, because each guards a different way
/// this could destroy real work:
///
/// * the id must be **contested** — some other project's store claims it too, so
///   one of the two stamps is provably false. Without this, `--matching` would
///   become a way to delete a project's own chunks by name.
/// * the operator must **name it** with `--matching`. Contested says one stamp
///   is false; it never says which. Only a human knows which project authored
///   `saas-tenancy-billing`.
///
/// So this deletes only where there is proof of a contradiction AND a human has
/// resolved it. Uncertainty still falls toward keeping the record.
pub fn prunable_contested<T: Owned>(
    records: &[T],
    project_id: &str,
    id_prefixes: &[String],
    contested: &BTreeSet<String>,
) -> Vec<String> {
    if id_prefixes.is_empty() {
        return Vec::new();
    }
    records
        .iter()
        .filter(|r| {
            let id = r.record_id();
            r.owner().as_deref() == Some(project_id)
                && contested.contains(&id)
                && id_prefixes.iter().any(|p| id.starts_with(p.as_str()))
        })
        .map(|r| r.record_id())
        .collect()
}

/// The adoption half of the table, in code, once.
fn is_adoptable(record_id: &str, owner: Option<&str>, id_prefixes: &[String]) -> bool {
    match owner {
        // Already provable, either way. Ours is a no-op; another project's is
        // the row adoption must never touch.
        Some(_) => false,
        None => {
            id_prefixes.is_empty()
                || id_prefixes
                    .iter()
                    .any(|p| record_id.starts_with(p.as_str()))
        }
    }
}

/// Copy the store file next to itself with a timestamped suffix, so a prune is
/// always undoable by hand. Written OUTSIDE the sessions directory: the store's
/// own invariant is that the sessions dir contains store files and nothing else.
pub fn backup_path(store: &Path, stamp: &str) -> PathBuf {
    let name = store
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "roadmap.json".to_string());
    let dir = store
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("backups").join(format!("{name}.{stamp}.bak"))
}

pub fn write_backup(store: &Path, stamp: &str) -> Result<PathBuf> {
    let dest = backup_path(store, stamp);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating the backup directory {}", parent.display()))?;
    }
    std::fs::copy(store, &dest)
        .with_context(|| format!("backing up {} to {}", store.display(), dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadmap::domain::ChunkStatus;

    fn chunk(id: &str, owner: Option<&str>) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: id.into(),
            name: crate::roadmap::name::derive(id),
            status: ChunkStatus::Pending,
            priority: 100,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec![],
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: owner.map(str::to_owned),
            created_at: "2026-07-25T00:00:00Z".into(),
            updated_at: "2026-07-25T00:00:00Z".into(),
        }
    }

    fn roadmap(chunks: Vec<Chunk>) -> Roadmap {
        Roadmap {
            project_id: "ours".into(),
            preamble: String::new(),
            chunks,
            notes: vec![],
            refreshes: vec![],
            links: vec![],
            tracker_opt_ins: vec![],
            container_links: vec![],
            focuses: vec![],
        }
    }

    #[test]
    fn inspect_separates_ours_theirs_and_unprovable() {
        let rm = roadmap(vec![
            chunk("mine", Some("ours")),
            chunk("theirs", Some("other-project")),
            chunk("legacy", None),
        ]);

        let report = inspect(&rm, "ours");
        assert_eq!(report.foreign, vec!["theirs".to_string()]);
        assert_eq!(report.unstamped, 1);
        assert_eq!(report.total, 3);
        assert!(report.has_findings());
    }

    #[test]
    fn inspect_reports_deps_pointing_at_nothing() {
        let mut a = chunk("a", Some("ours"));
        a.deps = vec!["b".into(), "ghost".into()];
        let rm = roadmap(vec![a, chunk("b", Some("ours"))]);

        let report = inspect(&rm, "ours");
        assert_eq!(
            report.dangling_deps,
            vec![("a".to_string(), "ghost".to_string())]
        );
    }

    /// An all-ours store is silent. `unstamped` on its own is not a finding —
    /// every chunk predating the stamp is unstamped and almost all are ours.
    #[test]
    fn a_healthy_store_reports_nothing_to_act_on() {
        let rm = roadmap(vec![chunk("a", Some("ours")), chunk("b", None)]);
        let report = inspect(&rm, "ours");
        assert!(!report.has_findings());
        assert_eq!(report.unstamped, 1);
    }

    #[test]
    fn prune_takes_provably_foreign_records_by_default() {
        let rm = roadmap(vec![
            chunk("mine", Some("ours")),
            chunk("theirs", Some("other-project")),
            chunk("legacy", None),
        ]);
        assert_eq!(prunable(&rm, "ours", &[]), vec!["theirs".to_string()]);
    }

    /// THE test for this module. An unstamped chunk's origin cannot be
    /// established, so nothing may remove it unless a human names it — the
    /// failure that matters is deleting someone's real work, not leaving
    /// residue behind.
    #[test]
    fn prune_never_touches_a_record_whose_origin_is_unprovable() {
        let rm = roadmap(vec![chunk("legacy", None), chunk("drive-audio-idle", None)]);

        // No prefixes: both survive, however much they look foreign.
        assert!(prunable(&rm, "ours", &[]).is_empty());

        // Named explicitly: only the named one goes.
        assert_eq!(
            prunable(&rm, "ours", &["drive-".to_string()]),
            vec!["drive-audio-idle".to_string()]
        );
    }

    /// Proof of ownership outranks a prefix the operator typed: a chunk stamped
    /// as ours survives even if its id matches the pattern.
    #[test]
    fn an_explicit_prefix_cannot_delete_a_provably_ours_chunk() {
        let rm = roadmap(vec![
            chunk("drive-ours", Some("ours")),
            chunk("drive-theirs", Some("other-project")),
            chunk("drive-legacy", None),
        ]);
        let doomed = prunable(&rm, "ours", &["drive-".to_string()]);
        assert!(!doomed.contains(&"drive-ours".to_string()));
        assert_eq!(doomed.len(), 2, "the foreign and the named-unstamped one");
    }

    // ─── Adoption: the other direction out of the unprovable row ──────────

    /// THE test for adoption. Every stamp already present is left exactly as
    /// it was — ours because there is nothing to do, another project's because
    /// overwriting it is how a bleed becomes permanent — and only the
    /// unprovable rows are claimed.
    ///
    /// The foreign chunk here is stamped as ours-in-a-different-store on
    /// purpose: the refusal is proven separately below, so this case uses a
    /// store the refusal lets through.
    #[test]
    fn adoption_claims_only_the_unprovable_and_never_a_stamp_that_exists() {
        let rm = roadmap(vec![
            chunk("mine", Some("ours")),
            chunk("legacy-a", None),
            chunk("legacy-b", None),
        ]);
        let claimed = adoptable_records(&rm.chunks, "ours", &[]).expect("a clean store adopts");
        assert_eq!(
            claimed,
            vec!["legacy-a".to_string(), "legacy-b".to_string()],
            "adoption must claim the unprovable rows and only those"
        );
    }

    /// The guard that makes adopt-by-default safe. Adoption over a store that
    /// is still polluted would stamp the intruder's work as ours and delete
    /// the only evidence it wasn't — so it refuses, and says which records.
    #[test]
    fn adoption_refuses_while_anything_provably_foreign_is_still_here() {
        let rm = roadmap(vec![
            chunk("legacy", None),
            chunk("theirs", Some("other-project")),
        ]);

        // Load-bearing first: it refuses at all. Anything else about the
        // message is worth nothing if this passes.
        let refused = adoptable_records(&rm.chunks, "ours", &[]);
        let Err(err) = refused else {
            panic!("adoption ran over a store still holding another project's chunk");
        };
        let message = err.to_string();
        assert!(
            message.contains("theirs"),
            "the refusal must name what is in the way: {message}"
        );

        // And it is the FOREIGN row that blocks, not merely "something odd" —
        // remove it and the same store adopts.
        let cleaned = roadmap(vec![chunk("legacy", None)]);
        assert_eq!(
            adoptable_records(&cleaned.chunks, "ours", &[]).expect("the pruned store adopts"),
            vec!["legacy".to_string()]
        );
    }

    /// `--matching` narrows an adoption the same way it widens a prune, and a
    /// record outside the pattern stays unprovable rather than being claimed.
    #[test]
    fn adoption_can_be_narrowed_to_the_ids_the_operator_names() {
        let rm = roadmap(vec![chunk("drive-legacy", None), chunk("web-legacy", None)]);
        assert_eq!(
            adoptable_records(&rm.chunks, "ours", &["web-".to_string()])
                .expect("a clean store adopts"),
            vec!["web-legacy".to_string()]
        );
    }

    // ─── The same table, the other two families ───────────────────────────
    //
    // These exist because the guarantee is per-FAMILY, not per-module: a
    // generic implementation that silently lost the unprovable row for think
    // or signal would still pass every roadmap test above.

    fn signal(id: &str, owner: Option<&str>) -> crate::signal::domain::Signal {
        use crate::signal::domain::{SignalKind, SignalStatus};
        crate::signal::domain::Signal {
            id: id.into(),
            kind: SignalKind::Idea,
            from: "tester".into(),
            body: String::new(),
            content: None,
            created: "2026-07-27T00:00:00Z".into(),
            status: SignalStatus::New,
            enrichment: vec![],
            cross_refs: vec![],
            surfaced_at: None,
            snooze_until: None,
            project_id: owner.map(str::to_owned),
        }
    }

    fn step_with_cwd(n: u32, cwd: Option<&str>) -> crate::think::domain::ThinkStep {
        // Built via serde so this fixture doesn't restate ~20 fields, matching
        // the pattern in tests/repo_sync_think_e2e.rs.
        let mut s: crate::think::domain::ThinkStep =
            serde_json::from_value(serde_json::json!({})).expect("a bare step deserializes");
        s.step_number = n;
        s.cwd = cwd.map(str::to_owned);
        s
    }

    /// THE signal-family version of the module's central test.
    #[test]
    fn signal_prune_never_touches_a_record_whose_origin_is_unprovable() {
        let signals = vec![
            signal("ours", Some("ours")),
            signal("theirs", Some("other-project")),
            signal("legacy", None),
        ];

        let report = inspect_records(&signals, "ours");
        assert_eq!(report.foreign, vec!["theirs".to_string()]);
        assert_eq!(report.unstamped, 1, "the legacy signal is unprovable");

        // Unnamed: the legacy signal survives, only the provably foreign goes.
        assert_eq!(
            prunable_records(&signals, "ours", &[]),
            vec!["theirs".to_string()]
        );
        // Named: and only then.
        assert_eq!(
            prunable_records(&signals, "ours", &["leg".to_string()]),
            vec!["theirs".to_string(), "legacy".to_string()]
        );
    }

    /// The signal family's adoption, for the same reason its prune has a copy:
    /// a generic implementation that lost the never-overwrite-a-stamp rule for
    /// one family would still pass every roadmap adoption test above.
    #[test]
    fn signal_adoption_claims_only_the_unprovable_and_refuses_over_a_bleed() {
        let polluted = [signal("legacy", None), signal("theirs", Some("other"))];
        assert!(
            adoptable_records(&polluted, "ours", &[]).is_err(),
            "a polluted signal store must refuse adoption"
        );

        let clean = [signal("ours", Some("ours")), signal("legacy", None)];
        assert_eq!(
            adoptable_records(&clean, "ours", &[]).expect("a clean store adopts"),
            vec!["legacy".to_string()]
        );
    }

    /// THE think-family version. A step's cwd is the origin signal; a step
    /// with no cwd is unprovable and must survive an unnamed prune.
    #[test]
    fn think_prune_never_touches_a_step_whose_origin_is_unprovable() {
        let ours = crate::infra::project_id_for_path(Path::new("/w/ours"));
        let steps = [
            step_with_cwd(1, Some("/w/ours")),
            step_with_cwd(2, Some("/w/theirs")),
            step_with_cwd(3, None),
        ];
        let records: Vec<ThinkOrigin<'_>> = steps
            .iter()
            .map(|step| ThinkOrigin {
                step,
                cwd_is_proof: true,
            })
            .collect();

        let report = inspect_records(&records, &ours);
        assert_eq!(report.foreign, vec!["2".to_string()], "only the other root");
        assert_eq!(report.unstamped, 1, "the cwd-less step is unprovable");

        assert_eq!(
            prunable_records(&records, &ours, &[]),
            vec!["2".to_string()],
            "the cwd-less step must survive an unnamed prune"
        );
    }

    /// THE GUARD. When our own project id is NOT cwd-derived — a
    /// name override — no step's cwd can prove anything, so every step must
    /// read as unprovable and survive. Without this, a project running under
    /// an override would be offered its own entire trace for deletion.
    #[test]
    fn without_cwd_proof_every_step_is_unprovable_and_survives() {
        let steps = [
            step_with_cwd(1, Some("/w/ours")),
            step_with_cwd(2, Some("/w/theirs")),
        ];
        let records: Vec<ThinkOrigin<'_>> = steps
            .iter()
            .map(|step| ThinkOrigin {
                step,
                cwd_is_proof: false,
            })
            .collect();

        let report = inspect_records(&records, "name-override-id");
        assert!(
            report.foreign.is_empty(),
            "nothing is provably foreign when attribution isn't proof"
        );
        assert_eq!(report.unstamped, 2);
        assert!(
            prunable_records(&records, "name-override-id", &[]).is_empty(),
            "an override project must never be offered its own trace for deletion"
        );
    }

    #[test]
    fn backups_land_outside_the_sessions_directory() {
        let store = Path::new("/data/roadmap/sessions/proj.json");
        let backup = backup_path(store, "20260725-120000");
        assert_eq!(
            backup,
            PathBuf::from("/data/roadmap/backups/proj.json.20260725-120000.bak")
        );
        assert!(
            !backup.starts_with("/data/roadmap/sessions"),
            "the sessions dir must hold store files and nothing else"
        );
    }
}

#[cfg(test)]
mod cross_store_tests {
    use super::*;
    use crate::roadmap::domain::{Chunk, ChunkStatus};

    /// Enumeration is by shape, not by a list of known names — driven by a
    /// directory tree production has never seen. Lock files, backups, the
    /// `_default` store and the `_legacy` directory all fall out of the
    /// extension and underscore tests; a namespaced think session contributes
    /// its project half; the union across directories dedupes; an unreadable
    /// directory contributes nothing rather than failing the run.
    #[test]
    fn project_enumeration_is_by_shape_not_name_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let think = tmp.path().join("think/sessions");
        let ship = tmp.path().join("ship/sessions");
        std::fs::create_dir_all(&think).expect("mkdir think");
        std::fs::create_dir_all(&ship).expect("mkdir ship");
        for name in [
            "quayside-aa11bb.json",
            "quayside-aa11bb.json.lock",
            "gate-house-cc22dd.json.bak-20260101-000000",
            "night-shift-ee33ff__winter-plan.json",
            "_default.json",
        ] {
            std::fs::write(think.join(name), "{}").expect("write");
        }
        std::fs::write(ship.join("quayside-aa11bb.json"), "{}").expect("write");
        std::fs::write(ship.join("cold-store-9900aa.json"), "{}").expect("write");
        std::fs::create_dir_all(think.join("_legacy")).expect("mkdir legacy");

        let dirs = vec![think, ship, tmp.path().join("signal/sessions")];
        assert_eq!(
            project_ids_in(&dirs),
            vec![
                "cold-store-9900aa".to_string(),
                "night-shift-ee33ff".to_string(),
                "quayside-aa11bb".to_string(),
            ]
        );
    }

    fn chunk(id: &str, owner: Option<&str>) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: id.into(),
            name: crate::roadmap::name::derive(id),
            status: ChunkStatus::Pending,
            priority: 100,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec![],
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: owner.map(str::to_owned),
            created_at: "2026-07-25T00:00:00Z".into(),
            updated_at: "2026-07-25T00:00:00Z".into(),
        }
    }

    /// The corruption every in-store check is blind to, driven by a table that
    /// is nothing like this repo's: two stores, each holding one id and each
    /// stamping it as its own. Both copies are internally consistent, so
    /// `inspect_records` calls each one "ours" in its own store and `prunable`
    /// correctly refuses to touch either. Only the contradiction between them
    /// is evidence, and only from outside.
    #[test]
    fn a_mis_stamp_is_visible_only_as_a_contradiction_between_stores() {
        let stores = vec![
            (
                "harbour-lights-aa11".to_string(),
                vec![
                    chunk("tide-table-refit", Some("harbour-lights-aa11")),
                    chunk("only-ours", Some("harbour-lights-aa11")),
                ],
            ),
            (
                "lantern-works-bb22".to_string(),
                vec![
                    // The bled copy, wearing the host's stamp.
                    chunk("tide-table-refit", Some("lantern-works-bb22")),
                    chunk("only-theirs", Some("lantern-works-bb22")),
                ],
            ),
        ];

        // In-store, each copy looks impeccable — that is the whole problem.
        for (project, records) in &stores {
            let report = inspect_records(records, project);
            assert!(
                report.foreign.is_empty(),
                "{project}: an in-store check cannot see this, by construction"
            );
        }

        let dupes = cross_store_duplicates(&stores);
        let ids: Vec<&str> = dupes.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["tide-table-refit"],
            "only the id held by two stores is a contradiction"
        );
        assert_eq!(
            dupes[0].self_claiming(),
            vec!["harbour-lights-aa11", "lantern-works-bb22"],
            "both stores claim authorship, so at least one stamp is false"
        );
    }

    /// The restraint that keeps this from being a delete button. An id can
    /// honestly recur across unrelated projects — `phase-1`, `scaffold`,
    /// `design-tokens` all do here. When neither copy claims authorship there is
    /// no contradiction to report, and even when there is, the detector names it
    /// rather than resolving it.
    #[test]
    fn an_honest_shared_id_is_reported_without_accusing_anyone() {
        let stores = vec![
            (
                "kiln-road-cc33".to_string(),
                vec![chunk("phase-1", None), chunk("kiln-only", None)],
            ),
            ("saltmarsh-dd44".to_string(), vec![chunk("phase-1", None)]),
        ];

        let dupes = cross_store_duplicates(&stores);
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].id, "phase-1");
        assert!(
            dupes[0].self_claiming().is_empty(),
            "neither copy claims authorship, so nothing is contradicted — an \
             unprovable id in two stores is a coincidence until proven otherwise"
        );
    }

    /// A record in exactly one store is never a finding, however it is stamped.
    #[test]
    fn a_record_in_one_store_is_never_a_contradiction() {
        let stores = vec![
            (
                "solo-ee55".to_string(),
                vec![chunk("a", Some("solo-ee55")), chunk("b", None)],
            ),
            ("other-ff66".to_string(), vec![chunk("c", None)]),
        ];
        assert!(cross_store_duplicates(&stores).is_empty());
    }
}

#[cfg(test)]
mod contested_prune_tests {
    use super::*;
    use crate::roadmap::domain::{Chunk, ChunkStatus};

    fn chunk(id: &str, owner: Option<&str>) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: id.into(),
            name: crate::roadmap::name::derive(id),
            status: ChunkStatus::Pending,
            priority: 100,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: vec![],
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: owner.map(str::to_owned),
            created_at: "2026-07-25T00:00:00Z".into(),
            updated_at: "2026-07-25T00:00:00Z".into(),
        }
    }

    fn contested(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    /// Both conditions are required, and the test drives a table nothing in this
    /// repo resembles so the rule cannot be passing by accident.
    #[test]
    fn removes_only_what_is_both_contested_and_named() {
        let store = vec![
            chunk("ledger-rollforward", Some("quarry-bell-77")), // contested + named
            chunk("ledger-archive", Some("quarry-bell-77")),     // contested, NOT named
            chunk("kiln-firing-sched", Some("quarry-bell-77")),  // named, NOT contested
            chunk("ledger-orphan", None),                        // unprovable: other rule
        ];
        let evidence = contested(&["ledger-rollforward", "ledger-archive"]);

        let doomed = prunable_contested(
            &store,
            "quarry-bell-77",
            &["ledger-rollforward".to_string()],
            &evidence,
        );
        assert_eq!(
            doomed,
            vec!["ledger-rollforward".to_string()],
            "a claimed record may go only when contested AND named"
        );
    }

    /// The guard that stops this becoming a delete-by-name button: with no
    /// contradiction on record, naming a chunk does nothing at all.
    #[test]
    fn naming_a_chunk_with_no_contradiction_removes_nothing() {
        let store = vec![chunk("kiln-firing-sched", Some("quarry-bell-77"))];
        let doomed = prunable_contested(
            &store,
            "quarry-bell-77",
            &["kiln-firing-sched".to_string()],
            &contested(&[]),
        );
        assert!(
            doomed.is_empty(),
            "without cross-store evidence this would delete a project's own work by name"
        );
    }

    /// And a blanket run is refused outright — contested evidence never licenses
    /// a sweep, because it says one stamp is false and never which.
    #[test]
    fn contested_alone_never_licenses_a_blanket_prune() {
        let store = vec![
            chunk("ledger-rollforward", Some("quarry-bell-77")),
            chunk("ledger-archive", Some("quarry-bell-77")),
        ];
        let evidence = contested(&["ledger-rollforward", "ledger-archive"]);
        assert!(
            prunable_contested(&store, "quarry-bell-77", &[], &evidence).is_empty(),
            "no --matching means no removal, however contested the ids are"
        );
    }
}
