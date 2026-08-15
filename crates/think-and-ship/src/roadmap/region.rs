//! Regions — the places a map is navigated by.
//!
//! A chunk's `group` is its region, and a region is the unit of navigation on
//! the tech-tree canvas: at the map level a person sees regions, not chunks, and
//! picks one to enter. That only works if a region is a *place* — something a
//! person could point at and say the name of.
//!
//! # Where the budgets come from
//!
//! None of the three numbers here is a taste call. They are read off
//! the canvas constraints:
//!
//! - [`REGION_BUDGET`] is C5's map level: at most 20 marks on screen, and the
//!   marks at that level are regions.
//! - [`REGION_POPULATION_BUDGET`] is C5's region level: at most 60 nodes once
//!   you are inside one. A region holding more than that cannot be drawn at the
//!   level it exists for.
//! - [`MAX_POPULATION_RATIO`] is C7: a region more than five times the median
//!   gives the map no rhythm, whatever it is called.
//!
//! Together the last two pin both ends. A budget on the largest region alone
//! would be satisfied by one region per chunk; a cap on the region count alone
//! would be satisfied by one region holding everything.
//!
//! # Why a region name is checked against the id prefixes
//!
//! A region used to be guessed from the leading token of a chunk id, which is
//! how this roadmap ended up with `saas`, `line`, `signal` and `iac`. Those
//! satisfy every count-based clause and still name nothing: `line` is not
//! somewhere you can go. C7's test is written against that exact failure — a
//! region name that is a substring of a chunk id prefix is the slug wearing a
//! region's job.
//!
//! That guess is gone, because the test above rules out every name it could
//! make: a prefix is a substring of itself, so a prefix-derived region fails C7
//! always rather than sometimes. What survives is [`cluster_by_prefix`], which
//! says which chunks belong together and leaves the naming to a person.
//!
//! # Why this is an audit and not a derivation
//!
//! [`crate::roadmap::name`] can seed a name from an id because a shortened id is
//! a usable label. There is no equivalent move here: every derivation available
//! from a chunk's own fields is the slug this file exists to reject. So the
//! region map is authored, and what the code owns is the check — pure, total,
//! and driven by whatever table it is handed rather than by the live roadmap.

use std::collections::{BTreeMap, BTreeSet};

/// The most region marks the map level may draw (constraint C5, map level).
pub const REGION_BUDGET: usize = 20;

/// The most chunks one region may hold (constraint C5, region level).
pub const REGION_POPULATION_BUDGET: usize = 60;

/// The most times larger the largest region may be than the median one
/// (constraint C7).
pub const MAX_POPULATION_RATIO: usize = 5;

/// The one region for chunks nobody has placed.
///
/// Authored, like every other region name, and for the reason this module gives
/// under its own heading: there is nothing in a chunk's fields to derive a place
/// from. What makes this one different is that the code names it, so the code
/// has to answer for it — hence `tests::the_unplaced_region_names_a_place`,
/// which holds it to the same C7 test every authored name faces.
///
/// It passes that test structurally rather than by luck. [`id_prefix`] splits on
/// space, so no id prefix ever contains one; a region name containing a space
/// therefore cannot be a substring of any prefix, whatever the ids happen to be.
pub const UNPLACED: &str = "Uncharted ground";

/// The leading token of a chunk id — the part [`cluster_by_prefix`] groups by,
/// and therefore the part a region name must not merely repeat.
///
/// Split on the same separators the name derivation uses, so the two agree
/// about where a slug's first word ends.
#[must_use]
pub fn id_prefix(id: &str) -> &str {
    id.split(['-', '_', '/', '.', ' ']).next().unwrap_or(id)
}

/// A set of ungrouped chunks that share an id prefix, offered for a person to
/// name.
///
/// Deliberately carries no region name. See [`cluster_by_prefix`] for why the
/// one thing a caller wants from it is the one thing it cannot supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// The shared leading token. A slug, and named as one — it is evidence that
    /// these chunks belong together, not a proposed name for the container.
    pub prefix: String,
    /// The ungrouped chunks sharing it, in the order they were handed over.
    pub chunk_ids: Vec<String>,
    /// Why `prefix` cannot be the region's name, in the words [`why_unfit`]
    /// would print. Always populated: a prefix is a substring of itself.
    pub why_prefix_is_unfit: String,
}

/// Cluster UNGROUPED chunks by shared id prefix, where at least `min_shared`
/// chunks share one.
///
/// # Why this proposes and never assigns
///
/// A region guessed from an id prefix fails [`why_unfit`] every single time, and
/// not as a matter of unlucky slugs: [`why_unfit`] asks whether the name is
/// contained in some prefix, and a prefix contains itself. There is no id this
/// reasoning does not cover, so there is no version of prefix-seeding that
/// produces a name the map accepts. What the machine can honestly contribute is
/// the grouping — which chunks belong together — and it stops there. The naming
/// is the caller's, through `roadmap_set_group`.
///
/// The floor stays for the reason it was introduced: measured on this roadmap,
/// 17 prefixes covered 239 of 317 chunks while 78 sat in a tail of 56 prefixes
/// used once or twice, and a container per one-off slug is worse than none.
///
/// Takes `(chunk id, region)` pairs rather than reading the roadmap, for the
/// same reason [`audit`] does: a function only ever driven by today's chunks
/// proves nothing about the next roadmap anybody writes.
pub fn cluster_by_prefix<'a, I>(chunks: I, min_shared: usize) -> Vec<Cluster>
where
    I: IntoIterator<Item = (&'a str, Option<&'a str>)>,
{
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut prefixes: BTreeSet<String> = BTreeSet::new();

    for (id, group) in chunks {
        let prefix = id_prefix(id);
        if prefix.is_empty() {
            continue;
        }
        prefixes.insert(prefix.to_string());
        // A chunk a person already placed is not a candidate — the stored answer
        // beats the guess, which is the whole reason it is stored.
        if group.map(str::trim).is_some_and(|g| !g.is_empty()) {
            continue;
        }
        grouped
            .entry(prefix.to_string())
            .or_default()
            .push(id.to_string());
    }

    let prefix_refs: Vec<&str> = prefixes.iter().map(String::as_str).collect();
    grouped
        .into_iter()
        .filter(|(_, ids)| ids.len() >= min_shared.max(1))
        .map(|(prefix, chunk_ids)| {
            let why_prefix_is_unfit = why_unfit(&prefix, prefix_refs.iter().copied())
                .unwrap_or_else(|| unreachable!("a prefix is a substring of itself"));
            Cluster {
                prefix,
                chunk_ids,
                why_prefix_is_unfit,
            }
        })
        .collect()
}

/// Whether `name` names a place: present, and not a slug repeated back.
#[must_use]
pub fn names_a_place<'a>(name: &str, id_prefixes: impl IntoIterator<Item = &'a str>) -> bool {
    why_unfit(name, id_prefixes).is_none()
}

/// How `name` fails [`names_a_place`], as a phrase a diagnostic can print.
/// `None` when it passes.
///
/// Compared case-insensitively and as a substring rather than an equality,
/// because `Saas` and `saa` are the same failure as `saas` — the check is
/// whether the name is contained in a slug, not whether it was spelled the same
/// way the seeder spelled it.
#[must_use]
pub fn why_unfit<'a>(name: &str, id_prefixes: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("has no name".to_string());
    }
    let lower = trimmed.to_lowercase();
    let hit = id_prefixes
        .into_iter()
        .find(|p| p.to_lowercase().contains(&lower))?;
    Some(format!(
        "\"{trimmed}\" is inside the chunk id prefix \"{hit}\", so it repeats a slug \
         instead of naming a place"
    ))
}

/// What a set of chunks says about its region map, measured against the three
/// budgets above.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionAudit {
    /// Chunks considered, region or no region.
    pub total: usize,
    /// Ids of chunks with no region at all, in the order they were handed over.
    pub homeless: Vec<String>,
    /// Region name and population, largest first, ties broken by name so the
    /// output is stable across runs.
    pub populations: Vec<(String, usize)>,
    /// Regions whose name repeats a chunk id prefix, with the phrase saying so.
    pub slug_named: Vec<(String, String)>,
    /// The median region population — the reference [`MAX_POPULATION_RATIO`]
    /// multiplies. Zero when there are no regions.
    pub median: usize,
    /// Regions over [`REGION_POPULATION_BUDGET`].
    pub over_population_budget: Vec<(String, usize)>,
    /// Regions over [`MAX_POPULATION_RATIO`] times [`RegionAudit::median`].
    pub unbalanced: Vec<(String, usize)>,
}

impl RegionAudit {
    /// One phrase per clause the map breaks, empty when it breaks none.
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.homeless.is_empty() {
            out.push(format!(
                "{} chunk(s) have no region, so the map has nowhere to draw them",
                self.homeless.len()
            ));
        }
        for (name, why) in &self.slug_named {
            out.push(format!("region {name}: {why}"));
        }
        if self.populations.len() > REGION_BUDGET {
            out.push(format!(
                "{} regions, over the {REGION_BUDGET}-mark map budget",
                self.populations.len()
            ));
        }
        for (name, pop) in &self.over_population_budget {
            out.push(format!(
                "region {name} holds {pop} chunks, over the \
                 {REGION_POPULATION_BUDGET}-node region budget"
            ));
        }
        for (name, pop) in &self.unbalanced {
            out.push(format!(
                "region {name} holds {pop} chunks against a median of {}, over {MAX_POPULATION_RATIO}x",
                self.median
            ));
        }
        out
    }

    /// Whether the map satisfies every clause.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures().is_empty()
    }

    /// How many regions the map has.
    #[must_use]
    pub fn regions(&self) -> usize {
        self.populations.len()
    }
}

/// Audit a region map.
///
/// Takes `(chunk id, region)` pairs rather than reading the roadmap, so the
/// gate can be driven with a table this project has never held. A check only
/// ever run against live data proves something about today's 520 chunks and
/// nothing about the next one anybody writes.
///
/// A region that is present but blank counts as no region: a chunk carrying
/// `Some("")` is homeless in every way that matters to the canvas, and letting
/// it through would put an unnamed mark on the map.
pub fn audit<'a, I>(chunks: I) -> RegionAudit
where
    I: IntoIterator<Item = (&'a str, Option<&'a str>)>,
{
    let mut total = 0usize;
    let mut homeless = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut prefixes: BTreeSet<String> = BTreeSet::new();

    for (id, group) in chunks {
        total += 1;
        prefixes.insert(id_prefix(id).to_string());
        match group.map(str::trim).filter(|g| !g.is_empty()) {
            Some(g) => *counts.entry(g.to_string()).or_default() += 1,
            None => homeless.push(id.to_string()),
        }
    }

    let prefix_refs: Vec<&str> = prefixes.iter().map(String::as_str).collect();
    let slug_named: Vec<(String, String)> = counts
        .keys()
        .filter_map(|name| {
            why_unfit(name, prefix_refs.iter().copied()).map(|why| (name.clone(), why))
        })
        .collect();

    let mut populations: Vec<(String, usize)> = counts.into_iter().collect();
    populations.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let median = median_of(&populations);
    let ceiling = median.saturating_mul(MAX_POPULATION_RATIO);
    let unbalanced: Vec<(String, usize)> = populations
        .iter()
        .filter(|(_, pop)| median > 0 && *pop > ceiling)
        .cloned()
        .collect();
    let over_population_budget: Vec<(String, usize)> = populations
        .iter()
        .filter(|(_, pop)| *pop > REGION_POPULATION_BUDGET)
        .cloned()
        .collect();

    RegionAudit {
        total,
        homeless,
        populations,
        slug_named,
        median,
        over_population_budget,
        unbalanced,
    }
}

/// The median population, averaging the two middle values on an even count.
/// Averaging rather than taking a side keeps the reference from jumping when
/// one region is added, which would move the balance ceiling under regions that
/// did not change.
fn median_of(populations: &[(String, usize)]) -> usize {
    if populations.is_empty() {
        return 0;
    }
    let mut sorted: Vec<usize> = populations.iter().map(|(_, n)| *n).collect();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A roadmap for a harbour logistics tool, which this project is not. The
    /// point of a table the live data has never held is that passing it says
    /// something about the next region map anybody writes.
    const FOREIGN: &[(&str, Option<&str>)] = &[
        ("berth-allocation-races", Some("Quayside")),
        ("berth-draft-limits", Some("Quayside")),
        ("berth-pilot-handoff", Some("Quayside")),
        ("crane-idle-telemetry", Some("Container yard")),
        ("crane-spreader-calibration", Some("Container yard")),
        ("yard-block-rebalance", Some("Container yard")),
        ("customs-manifest-mismatch", Some("Paperwork")),
        ("customs-duty-rounding", Some("Paperwork")),
        ("bill-of-lading-amendments", Some("Paperwork")),
    ];

    fn table(rows: &[(&'static str, Option<&'static str>)]) -> RegionAudit {
        audit(rows.iter().map(|(id, g)| (*id, *g)))
    }

    fn clusters(rows: &[(&'static str, Option<&'static str>)], min_shared: usize) -> Vec<Cluster> {
        cluster_by_prefix(rows.iter().map(|(id, g)| (*id, *g)), min_shared)
    }

    /// The whole of the first chunk in one assertion: whatever the ids are, the
    /// prefix they share cannot be the name of the region holding them. Driven
    /// over the foreign table and every disguise the audit already knows about,
    /// so it is a statement about prefixes rather than about these nine rows.
    #[test]
    fn no_prefix_can_ever_name_the_region_it_groups() {
        let all_ungrouped: Vec<(&str, Option<&str>)> =
            FOREIGN.iter().map(|(id, _)| (*id, None)).collect();
        let found = cluster_by_prefix(all_ungrouped.iter().copied(), 1);
        assert!(!found.is_empty(), "the foreign ids do share prefixes");
        for c in &found {
            let prefixes: Vec<&str> = FOREIGN.iter().map(|(id, _)| id_prefix(id)).collect();
            assert!(
                !names_a_place(&c.prefix, prefixes.iter().copied()),
                "{:?} was accepted as a region name, so the seeder could still mint one",
                c.prefix
            );
            assert!(
                c.why_prefix_is_unfit.contains(&c.prefix),
                "the caller must be told which slug it was: {}",
                c.why_prefix_is_unfit
            );
        }
    }

    /// The clustering keeps the one judgement it can honestly make — which
    /// chunks belong together — and the floor that stops it minting a container
    /// per one-off slug.
    #[test]
    fn clustering_groups_the_ungrouped_and_respects_the_floor() {
        let rows = [
            ("berth-allocation-races", None),
            ("berth-draft-limits", None),
            ("berth-pilot-handoff", None),
            ("crane-idle-telemetry", None),
            ("customs-manifest-mismatch", Some("Paperwork")),
            ("customs-duty-rounding", Some("Paperwork")),
            ("customs-tariff-codes", Some("Paperwork")),
        ];
        let found = clusters(&rows, 3);
        assert_eq!(found.len(), 1, "only berth clears a floor of three");
        assert_eq!(found[0].prefix, "berth");
        assert_eq!(found[0].chunk_ids.len(), 3);

        // Lower the floor and crane appears; customs never does, because those
        // three are already placed and a stored answer beats the guess.
        let lower = clusters(&rows, 1);
        let named: Vec<&str> = lower.iter().map(|c| c.prefix.as_str()).collect();
        assert_eq!(named, ["berth", "crane"]);
    }

    /// The name the code itself chose has to survive the check every authored
    /// name faces — against the foreign table AND against this project's own
    /// ids, since those are the prefixes it will actually sit beside.
    #[test]
    fn the_unplaced_region_names_a_place() {
        let foreign: Vec<&str> = FOREIGN.iter().map(|(id, _)| id_prefix(id)).collect();
        assert!(names_a_place(UNPLACED, foreign.iter().copied()));

        let ours = [
            "seeding",
            "the",
            "uncharted",
            "ground",
            "unchartedground",
            "canvas",
            "region",
            "map",
        ];
        assert!(
            names_a_place(UNPLACED, ours.iter().copied()),
            "{UNPLACED:?} must survive prefixes that share its words"
        );
    }

    /// The name crosses a language boundary — the canvas draws it and this
    /// crate applies it — so it is owned by neither side. Both declare their own
    /// constant and both assert against the table, which is what makes renaming
    /// it on one side alone turn the other's suite red rather than producing two
    /// maps that disagree about where a chunk is.
    #[test]
    fn both_languages_agree_which_region_holds_the_unplaced() {
        const SHARED: &str = include_str!("../../../../shared/region-unplaced.json");
        let table: serde_json::Value =
            serde_json::from_str(SHARED).expect("the shared contract must parse");
        assert_eq!(
            table["region"].as_str(),
            Some(UNPLACED),
            "the shared table and roadmap::region::UNPLACED name different places"
        );
        // The reason, not just the name — a reader arriving at the constant with
        // no context should find the argument, and a rename to one word should
        // have to delete this sentence to pass.
        assert!(
            table["why_it_passes_the_check"]
                .as_str()
                .is_some_and(|w| w.contains("space")),
            "the contract must record WHY the name is safe, not only that it is"
        );
    }

    /// Why it survives is structural, not lucky: a prefix never contains a
    /// space, so a name that does can never be inside one. Stated as a test so
    /// that renaming the region to a single word turns this red rather than
    /// shipping.
    #[test]
    fn the_unplaced_region_is_safe_because_a_prefix_holds_no_space() {
        assert!(
            UNPLACED.contains(' '),
            "a single-word region name is only safe by luck"
        );
        for (id, _) in FOREIGN {
            assert!(!id_prefix(id).contains(' '));
        }
    }

    /// An unmapped roadmap is over budget in the unplaced region, and the audit
    /// has to say so. Concealing it would make an unnavigable map look fine.
    #[test]
    fn an_unplaced_region_over_budget_is_reported_not_hidden() {
        let ids: Vec<String> = (0..REGION_POPULATION_BUDGET + 1)
            .map(|n| format!("chunk-{n}"))
            .collect();
        let rows: Vec<(&str, Option<&str>)> =
            ids.iter().map(|id| (id.as_str(), Some(UNPLACED))).collect();
        let report = audit(rows);
        assert_eq!(report.over_population_budget.len(), 1);
        assert!(
            report
                .failures()
                .iter()
                .any(|f| f.contains(UNPLACED) && f.contains("over the")),
            "failures: {:?}",
            report.failures()
        );
        assert!(
            report.slug_named.is_empty(),
            "the unplaced region must not itself be a slug"
        );
    }

    #[test]
    fn a_map_of_named_balanced_places_passes_every_clause() {
        let report = table(FOREIGN);
        assert!(report.is_clean(), "failures: {:?}", report.failures());
        assert_eq!(report.total, 9);
        assert_eq!(report.regions(), 3);
        assert_eq!(report.median, 3);
    }

    #[test]
    fn a_region_named_after_a_slug_is_rejected_and_the_slug_is_named() {
        let rows = [
            ("berth-allocation-races", Some("berth")),
            ("berth-draft-limits", Some("berth")),
            ("crane-idle-telemetry", Some("Container yard")),
        ];
        let report = table(&rows);
        assert!(!report.is_clean());
        assert_eq!(report.slug_named.len(), 1);
        assert!(
            report.slug_named[0].1.contains("berth"),
            "the diagnostic must name the prefix: {}",
            report.slug_named[0].1
        );
    }

    /// Capitalizing a slug does not turn it into a place, and neither does
    /// using a piece of one.
    #[test]
    fn case_and_partial_slugs_do_not_escape_the_check() {
        for disguise in ["Berth", "BERTH", "bert", "h"] {
            let rows = [
                ("berth-allocation-races", Some(disguise)),
                ("crane-idle-telemetry", Some("Container yard")),
            ];
            let report = table(&rows);
            assert_eq!(
                report.slug_named.len(),
                1,
                "{disguise:?} should have been caught"
            );
        }
    }

    /// A name containing a slug word is fine — "Quayside berths" is somewhere
    /// you can stand. The clause is about the name being swallowed by a slug,
    /// not about which words it may use.
    #[test]
    fn a_place_name_may_contain_a_slug_word() {
        let rows = [
            ("berth-allocation-races", Some("Quayside berths")),
            ("crane-idle-telemetry", Some("Container yard")),
        ];
        assert!(table(&rows).slug_named.is_empty());
    }

    #[test]
    fn a_chunk_with_no_region_is_reported_by_id() {
        let rows = [
            ("berth-allocation-races", Some("Quayside")),
            ("crane-idle-telemetry", None),
            ("yard-block-rebalance", Some("")),
            ("customs-duty-rounding", Some("   ")),
        ];
        let report = table(&rows);
        assert_eq!(
            report.homeless,
            vec![
                "crane-idle-telemetry".to_string(),
                "yard-block-rebalance".to_string(),
                "customs-duty-rounding".to_string()
            ]
        );
        assert!(!report.is_clean());
    }

    #[test]
    fn more_regions_than_the_map_can_draw_is_a_failure() {
        let names: Vec<String> = (0..=REGION_BUDGET).map(|i| format!("Pier {i}")).collect();
        let ids: Vec<String> = (0..=REGION_BUDGET)
            .map(|i| format!("wharf-job-{i}"))
            .collect();
        let rows: Vec<(&str, Option<&str>)> = ids
            .iter()
            .zip(names.iter())
            .map(|(id, name)| (id.as_str(), Some(name.as_str())))
            .collect();
        let report = audit(rows);
        assert_eq!(report.regions(), REGION_BUDGET + 1);
        assert!(
            report.failures().iter().any(|f| f.contains("map budget")),
            "failures: {:?}",
            report.failures()
        );
    }

    /// The two population clauses catch different shapes, so each is proved
    /// against a map the other one is happy with.
    #[test]
    fn the_region_node_budget_binds_even_when_the_map_is_balanced() {
        let mut rows: Vec<(String, String)> = Vec::new();
        for region in ["Quayside", "Container yard", "Paperwork"] {
            for i in 0..(REGION_POPULATION_BUDGET + 1) {
                rows.push((format!("wharf-{region}-{i}"), region.to_string()));
            }
        }
        let report = audit(rows.iter().map(|(id, g)| (id.as_str(), Some(g.as_str()))));
        assert!(
            report.unbalanced.is_empty(),
            "every region is the same size"
        );
        assert_eq!(report.over_population_budget.len(), 3);
        assert!(!report.is_clean());
    }

    #[test]
    fn a_region_far_over_the_median_is_unbalanced_even_inside_the_node_budget() {
        let mut rows: Vec<(String, String)> = Vec::new();
        for i in 0..50 {
            rows.push((format!("wharf-big-{i}"), "Container yard".to_string()));
        }
        for i in 0..5 {
            rows.push((format!("berth-small-{i}"), "Quayside".to_string()));
        }
        for i in 0..5 {
            rows.push((format!("customs-small-{i}"), "Paperwork".to_string()));
        }
        let report = audit(rows.iter().map(|(id, g)| (id.as_str(), Some(g.as_str()))));
        assert_eq!(report.median, 5);
        assert!(report.over_population_budget.is_empty());
        assert_eq!(report.unbalanced, vec![("Container yard".to_string(), 50)]);
    }

    /// Exactly the ratio is allowed and one past it is not, stated as a test so
    /// the boundary is pinned rather than left to whoever reads the `>`.
    #[test]
    fn the_balance_ceiling_is_inclusive() {
        let build = |big: usize| {
            let mut rows: Vec<(String, String)> = Vec::new();
            for i in 0..big {
                rows.push((format!("wharf-big-{i}"), "Container yard".to_string()));
            }
            for i in 0..4 {
                rows.push((format!("berth-small-{i}"), "Quayside".to_string()));
            }
            for i in 0..4 {
                rows.push((format!("customs-small-{i}"), "Paperwork".to_string()));
            }
            rows
        };
        let at = build(4 * MAX_POPULATION_RATIO);
        let report = audit(at.iter().map(|(id, g)| (id.as_str(), Some(g.as_str()))));
        assert_eq!(report.median, 4);
        assert!(
            report.unbalanced.is_empty(),
            "exactly {MAX_POPULATION_RATIO}x must pass"
        );

        let over = build(4 * MAX_POPULATION_RATIO + 1);
        let report = audit(over.iter().map(|(id, g)| (id.as_str(), Some(g.as_str()))));
        assert_eq!(report.unbalanced.len(), 1);
    }

    /// An empty roadmap has no map to fail. Stated so the ratio arithmetic is
    /// pinned against a zero median rather than discovered by a division.
    #[test]
    fn an_empty_roadmap_is_clean() {
        let report = audit(std::iter::empty());
        assert!(report.is_clean());
        assert_eq!(report.median, 0);
        assert_eq!(report.regions(), 0);
    }

    #[test]
    fn populations_come_back_largest_first_and_stable() {
        let rows = [
            ("berth-a", Some("Quayside")),
            ("berth-b", Some("Quayside")),
            ("crane-a", Some("Container yard")),
            ("crane-b", Some("Container yard")),
            ("customs-a", Some("Paperwork")),
        ];
        let report = table(&rows);
        assert_eq!(
            report.populations,
            vec![
                ("Container yard".to_string(), 2),
                ("Quayside".to_string(), 2),
                ("Paperwork".to_string(), 1),
            ]
        );
    }

    #[test]
    fn a_prefix_stops_at_any_separator_the_ids_use() {
        assert_eq!(id_prefix("berth-allocation-races"), "berth");
        assert_eq!(id_prefix("berth_allocation"), "berth");
        assert_eq!(id_prefix("berth.allocation"), "berth");
        assert_eq!(id_prefix("berth/allocation"), "berth");
        assert_eq!(id_prefix("berth allocation"), "berth");
        assert_eq!(id_prefix("harbourmaster"), "harbourmaster");
        assert_eq!(id_prefix(""), "");
    }

    #[test]
    fn why_unfit_names_the_actual_failure() {
        assert_eq!(why_unfit("", ["berth"]).as_deref(), Some("has no name"));
        assert_eq!(why_unfit("   ", ["berth"]).as_deref(), Some("has no name"));
        assert_eq!(why_unfit("Quayside", ["berth", "crane"]), None);
        assert!(why_unfit("berth", ["berth"]).unwrap().contains("berth"));
    }

    #[test]
    fn names_a_place_agrees_with_why_unfit() {
        assert!(names_a_place("Quayside", ["berth"]));
        assert!(!names_a_place("berth", ["berth"]));
        assert!(!names_a_place("", ["berth"]));
    }
}
