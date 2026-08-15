//! Short chunk names — the label a canvas node wears.
//!
//! A chunk's `title` is a sentence stating a claim, and that is the house style:
//! measured across this project's own roadmap, zero of 519 titles are under 24
//! characters, the median is 90 and the longest is 160. A sentence is the right
//! thing to read once you have opened a chunk and the wrong thing to render 519
//! of on a canvas, so a chunk carries both — `title` keeps the claim, `name`
//! is what a node wears.
//!
//! # Where the budget comes from
//!
//! [`NAME_BUDGET`] is not a taste call. It is derived from the canvas map
//! level's mark size, and it is the number [`fits`] gates on.
//!
//! # Why the id is only a seed
//!
//! [`fn@derive`] exists so no chunk is ever nameless, not so names are computed.
//! The same argument the `group` field makes applies here: a derived-on-read
//! label can never be corrected, and names are one of the two things the
//! constraints document says need a machine *and* a human to maintain. So the
//! name is stored, seeded by this function, and overwritten the moment anyone
//! writes a better one.
//!
//! The derivation has to actively shorten rather than reformat. Reformatting the
//! id is the obvious approach and it does not work: 208 of this roadmap's 519
//! ids are themselves over budget (the longest is 47 characters), so humanizing
//! an id satisfies C8 for only 60% of the roadmap and silently fails the rest.

/// The most characters a node label may carry (constraint C8).
pub const NAME_BUDGET: usize = 24;

/// The most words a name may carry. A four-word fragment of a slug reads as a
/// truncated sentence, which is the thing being escaped; three is where a label
/// still reads as a label.
const MAX_WORDS: usize = 3;

/// Dropped from the front. An article carries no identity and spends characters
/// that the distinguishing words need.
const LEADING_NOISE: &[&str] = &["the", "a", "an"];

/// Dropped from the end. These are words a phrase leans on to reach the *next*
/// word, so when the next word is over budget they are left pointing at nothing
/// — "JWT secret is" says less than "JWT secret".
const TRAILING_NOISE: &[&str] = &[
    "is", "are", "was", "were", "be", "been", "the", "a", "an", "of", "on", "in", "into", "to",
    "and", "or", "for", "no", "not", "should", "shall", "can", "cannot", "its", "it", "that",
    "this", "these", "those", "from", "by", "with", "as", "at", "but", "so", "than", "then",
    "when", "while", "only", "just", "still", "does", "do", "did", "has", "have", "had", "must",
    "may", "might", "will", "would", "per", "via", "over", "under", "if", "unless",
];

/// Uppercased wholesale rather than title-cased. "Ai metering" and "Mcp entry"
/// read as typos; the acronym is the recognisable part of the label.
const ACRONYMS: &[&str] = &[
    "ai", "api", "cd", "ci", "cli", "cors", "cpu", "css", "csv", "db", "dns", "e2e", "env", "ftp",
    "gui", "html", "http", "https", "iac", "id", "imap", "io", "ipc", "json", "jwks", "jwt", "kv",
    "ldap", "llm", "md", "mcp", "mx", "npm", "oauth", "os", "otel", "pr", "rss", "saas", "sdk",
    "smtp", "spa", "sql", "sse", "ssh", "ssl", "tcp", "tls", "ttl", "tui", "udp", "ui", "url",
    "uuid", "ux", "wasm", "yaml",
];

/// Whether `name` is a usable node label: present, and within [`NAME_BUDGET`].
///
/// Counted in characters rather than bytes — the budget is about how much a
/// node can show, and a multi-byte character occupies one mark's worth of it.
#[must_use]
pub fn fits(name: &str) -> bool {
    let n = name.trim().chars().count();
    n > 0 && n <= NAME_BUDGET
}

/// How `name` fails [`fits`], as a phrase a diagnostic can print. `None` when it
/// passes.
#[must_use]
pub fn why_unfit(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("has no name".to_string());
    }
    let n = trimmed.chars().count();
    if n > NAME_BUDGET {
        return Some(format!(
            "name is {n} characters, over the {NAME_BUDGET}-character budget"
        ));
    }
    None
}

/// Seed a name from a chunk id. Total and deterministic: every id carrying any
/// content yields a name that satisfies [`fits`], including ids longer than the
/// whole budget. An empty id is the one exception and it derives to nothing,
/// because there is nothing to name — the engine rejects that id upstream.
///
/// Pure by design, so the gate can drive it with ids this roadmap has never
/// held. A derivation only ever exercised against live data proves nothing about
/// the next chunk anyone writes.
#[must_use]
pub fn derive(id: &str) -> String {
    let words: Vec<&str> = id
        .split(['-', '_', '/', '.', ' '])
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() {
        // No word characters at all. The id is still identity, so clamp it
        // rather than inventing a label that points at nothing.
        return clamp(id.trim());
    }

    // Leading articles go first, but never the last word standing: an id that is
    // literally "the" should name itself rather than name nothing.
    let mut start = 0;
    while start + 1 < words.len()
        && LEADING_NOISE.contains(&words[start].to_ascii_lowercase().as_str())
    {
        start += 1;
    }

    // Take words while both budgets hold. Checked against the *rendered* length
    // including separators, so the result needs no second truncation pass.
    let mut taken: Vec<&str> = Vec::new();
    let mut width = 0usize;
    for w in &words[start..] {
        if taken.len() == MAX_WORDS {
            break;
        }
        let sep = usize::from(!taken.is_empty());
        let next = width + sep + w.chars().count();
        if !taken.is_empty() && next > NAME_BUDGET {
            break;
        }
        taken.push(w);
        width = next;
    }

    // Trailing connectives are trimmed after the cut, not before it: a word is
    // only noise once the word it was reaching for has been dropped. Never trims
    // to nothing, for the same reason the leading pass does not.
    while taken.len() > 1
        && TRAILING_NOISE.contains(&taken[taken.len() - 1].to_ascii_lowercase().as_str())
    {
        taken.pop();
    }

    let rendered: Vec<String> = taken
        .iter()
        .enumerate()
        .map(|(i, w)| case_word(w, i == 0))
        .collect();

    clamp(&rendered.join(" "))
}

/// Uppercase a known acronym, capitalize the leading word, otherwise leave the
/// word lowercase. Sentence case rather than title case — a label is a name, not
/// a headline.
fn case_word(word: &str, leading: bool) -> String {
    let lower = word.to_ascii_lowercase();
    if ACRONYMS.contains(&lower.as_str()) {
        return lower.to_ascii_uppercase();
    }
    if !leading {
        return lower;
    }
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => lower,
    }
}

/// Cut to [`NAME_BUDGET`] characters. The last line of defence, reached only by
/// a single word longer than the whole budget — character-wise so it cannot
/// split a multi-byte character.
fn clamp(s: &str) -> String {
    s.chars().take(NAME_BUDGET).collect::<String>()
}

/// Separate seeds that collided. [`fn@derive`] keeps an id's HEAD words, so ids
/// that differ only in their tail land on the same label — measured across the
/// 58 real stores on this machine, 770 of 8089 chunks wore a label at least one
/// sibling in the same store also wore, 99% of them inside one region, and every
/// one of them was still the untouched seed. Two marks in one territory wearing
/// one word are two marks a reader cannot tell apart.
///
/// Takes the store's whole `(id, name)` table and returns only the rewrites, so
/// tests drive it with tables production has never held. Only a name still equal
/// to `derive(id)` is ever a candidate: that equality proves no human has
/// touched it, which is what keeps an authored label — and any label that does
/// not collide — out of reach by construction rather than by policy.
///
/// Per colliding class: the common word-prefix of the ids is computed; a member
/// with no words beyond it keeps the label the class tied at (at the top level
/// that is its own seed, so the family head does not move); every other member
/// wears the last shared word as context plus its own distinguishing tail,
/// through the same word-cap and budget machinery as the seed. Members still
/// tied recurse on their strictly-longer shared prefix; a proposal already worn
/// elsewhere in the store widens its context window until free. Deterministic
/// and order-independent (classes and members are processed in sorted order),
/// and idempotent: a rewritten name no longer equals its seed, so the next pass
/// leaves it alone.
#[must_use]
pub fn repair_collisions(table: &[(&str, &str)]) -> Vec<(String, String)> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut by_label: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, (_, name)) in table.iter().enumerate() {
        by_label.entry(name).or_default().push(i);
    }

    // Labels not ours to move: singletons, authored names (mixed classes keep
    // their authored members), and everything a repaired class settles on.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut classes: Vec<(&str, Vec<&str>)> = Vec::new();
    for (label, idxs) in &by_label {
        if label.trim().is_empty() {
            continue; // nameless rows are backfill's job, not a label class
        }
        let (seeds, authored): (Vec<usize>, Vec<usize>) = idxs
            .iter()
            .copied()
            .partition(|&i| table[i].1 == derive(table[i].0));
        if idxs.len() >= 2 && seeds.len() >= 2 {
            classes.push((label, seeds.iter().map(|&i| table[i].0).collect()));
            for i in authored {
                taken.insert(table[i].1.to_string());
            }
        } else {
            for &i in idxs {
                taken.insert(table[i].1.to_string());
            }
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for (tie, ids) in classes {
        let members: Vec<(&str, Vec<&str>)> = ids
            .iter()
            .map(|id| {
                (
                    *id,
                    id.split(['-', '_', '/', '.', ' '])
                        .filter(|w| !w.is_empty())
                        .collect(),
                )
            })
            .collect();
        repair_class(&members, tie, &mut taken, &mut out);
    }

    // Emit only real rewrites, in a stable order.
    let current: BTreeMap<&str, &str> = table.iter().map(|(id, name)| (*id, *name)).collect();
    out.retain(|(id, name)| current.get(id.as_str()).copied() != Some(name.as_str()));
    out.sort();
    out
}

/// Relabel one class of ids whose labels currently tie at `tie`. Members are
/// processed in sorted-id order so the result cannot depend on store order.
fn repair_class(
    members: &[(&str, Vec<&str>)],
    tie: &str,
    taken: &mut std::collections::BTreeSet<String>,
    out: &mut Vec<(String, String)>,
) {
    let mut common = members[0].1.len();
    for (_, words) in members {
        let mut i = 0;
        while i < common && i < words.len() && words[i] == members[0].1[i] {
            i += 1;
        }
        common = common.min(i);
    }

    let mut sorted: Vec<&(&str, Vec<&str>)> = members.iter().collect();
    sorted.sort_by_key(|(id, _)| *id);

    let mut proposals: std::collections::BTreeMap<String, Vec<&(&str, Vec<&str>)>> =
        std::collections::BTreeMap::new();
    for m in sorted {
        let label = if m.1.len() == common {
            // No distinguishing words left: keep the label the class tied at —
            // at the top level, its own seed, so the family head never moves.
            tie.to_string()
        } else {
            // Context + tail, widening the context window while the proposal is
            // already worn outside the class.
            let mut label = String::new();
            for ctx in 1..=common + 1 {
                let from = common.saturating_sub(ctx);
                let mut words: Vec<&str> = m.1[from..common].to_vec();
                let context_len = words.len();
                words.extend_from_slice(&m.1[common..]);
                label = select_from(&words, context_len);
                if !taken.contains(&label) {
                    break;
                }
            }
            label
        };
        proposals.entry(label).or_default().push(m);
    }

    for (label, ms) in proposals {
        if ms.len() == 1 && !taken.contains(&label) {
            taken.insert(label.clone());
            out.push((ms[0].0.to_string(), label));
        } else if ms.len() > 1 && ms.iter().all(|m| m.1.len() > common) {
            // Still tied: these ids share a strictly longer prefix, so the
            // recursion advances and terminates at word exhaustion.
            let subset: Vec<(&str, Vec<&str>)> = ms.iter().map(|m| (m.0, m.1.clone())).collect();
            repair_class(&subset, &label, taken, out);
        } else {
            // Unresolvable within the words the ids carry. Keep the tie rather
            // than invent content — the residual is measured, not hidden.
            for m in ms {
                taken.insert(label.clone());
                out.push((m.0.to_string(), label.clone()));
            }
        }
    }
}

/// [`fn@derive`]'s take-loop over an explicit word list. `must_reach` is the
/// index of the first word carrying distinguishing content; when the budget
/// cannot reach it the context words before it are dropped, because a label
/// that spends the whole budget on shared context has separated nothing.
fn select_from(words: &[&str], must_reach: usize) -> String {
    let mut list = words;
    let mut must = must_reach;
    loop {
        let mut taken: Vec<&str> = Vec::new();
        let mut width = 0usize;
        for w in list {
            if taken.len() == MAX_WORDS {
                break;
            }
            let sep = usize::from(!taken.is_empty());
            let next = width + sep + w.chars().count();
            if !taken.is_empty() && next > NAME_BUDGET {
                break;
            }
            taken.push(w);
            width = next;
        }
        if taken.len() > must || must == 0 || list.len() == 1 {
            while taken.len() > 1
                && TRAILING_NOISE.contains(&taken[taken.len() - 1].to_ascii_lowercase().as_str())
            {
                taken.pop();
            }
            let rendered: Vec<String> = taken
                .iter()
                .enumerate()
                .map(|(i, w)| case_word(w, i == 0))
                .collect();
            return clamp(&rendered.join(" "));
        }
        list = &list[must..];
        must = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids this roadmap has never held. The point of a table the live data has
    /// never seen is that passing it says something about the next chunk
    /// somebody writes, not about the 519 that already exist.
    const FOREIGN: &[(&str, &str)] = &[
        // Leading article dropped, trailing modal trimmed.
        ("the-widget-registry-should-flush", "Widget registry"),
        // Acronym uppercased where title case would read as a typo.
        ("ftp-upload-retries-forever", "FTP upload retries"),
        ("an-ldap-bind-leaks-a-handle", "LDAP bind leaks"),
        // Trailing copula trimmed rather than kept pointing at a dropped word.
        ("quota-ceiling-is-unenforced", "Quota ceiling"),
        // Three-word cap, comfortably inside the budget.
        (
            "shipping-labels-print-blank-on-thermal",
            "Shipping labels print",
        ),
        // Budget binds before the word cap does.
        ("reconciliation-statements-duplicate", "Reconciliation"),
        // Underscores and dots are separators too.
        ("payroll_export.breaks_on_leap_day", "Payroll export breaks"),
        // A single word, already a fine label.
        ("harbourmaster", "Harbourmaster"),
    ];

    #[test]
    fn derives_readable_names_for_ids_this_roadmap_has_never_held() {
        for (id, want) in FOREIGN {
            assert_eq!(&derive(id), want, "deriving a name for {id}");
        }
    }

    /// The cross-language contract. A chunk written before `name` existed keeps a
    /// cloud copy that is only rewritten when the chunk is mutated, so the canvas
    /// derives a label for records this engine will never get to seed — and the
    /// two derivations have to agree, or the same chunk wears one name locally
    /// and another on the web.
    ///
    /// The table lives outside both languages (`shared/name-derivation.cases.json`)
    /// so neither side owns it and neither can be quietly relaxed to match a
    /// regression. `frontend/src/tree/name.test.ts` drives the same file.
    #[test]
    fn agrees_with_the_typescript_derivation_on_the_shared_table() {
        const SHARED: &str = include_str!("../../../../shared/name-derivation.cases.json");
        let table: serde_json::Value =
            serde_json::from_str(SHARED).expect("the shared derivation table must parse");

        assert_eq!(
            table["budget"].as_u64().expect("budget"),
            NAME_BUDGET as u64,
            "the shared table's budget must be C8's budget, or the two sides are \
             agreeing about different things"
        );

        let cases = table["cases"].as_array().expect("cases");
        assert!(
            cases.len() >= 15,
            "a contract table this small stops being evidence: {} cases",
            cases.len()
        );

        for case in cases {
            let id = case["id"].as_str().expect("case id");
            let want = case["name"].as_str().expect("case name");
            assert_eq!(derive(id), want, "deriving a name for {id:?}");
        }
    }

    /// The gate the whole chunk exists to install: nothing this function can
    /// emit may exceed C8's budget, whatever it is handed.
    #[test]
    fn every_derived_name_fits_the_budget() {
        let hostile = [
            "a",
            "the",
            "-",
            "---",
            "",
            "supercalifragilisticexpialidocious-considerations",
            "declaring-identity-requires-configuring-clients",
            "ünïcödé-wörd-thät-rüns-lóng-ènough-to-clamp",
            "ONE-TWO-THREE",
            "a-b-c-d-e-f-g-h-i-j-k",
        ];
        for id in hostile {
            let got = derive(id);
            assert!(
                got.chars().count() <= NAME_BUDGET,
                "derive({id:?}) produced {} chars: {got:?}",
                got.chars().count()
            );
        }
        for (id, _) in FOREIGN {
            assert!(fits(&derive(id)), "derive({id:?}) must satisfy fits()");
        }
    }

    /// An id made only of noise words still names itself. Trimming to nothing
    /// would hand the canvas a blank node, which is worse than a weak label.
    #[test]
    fn never_trims_a_name_away_entirely() {
        for id in ["the", "a-the", "is", "of-the", "the-is-of"] {
            assert!(!derive(id).is_empty(), "derive({id:?}) must not be empty");
        }
    }

    /// Deterministic: positions on the canvas are learned, so a name that
    /// changed between runs would move the label under a node that did not move.
    #[test]
    fn derivation_is_deterministic() {
        for (id, _) in FOREIGN {
            assert_eq!(derive(id), derive(id));
        }
    }

    /// The one input with no name to give. Stated as a test so the exception is
    /// pinned rather than discovered.
    #[test]
    fn an_empty_id_derives_to_nothing() {
        assert_eq!(derive(""), "");
        assert!(!fits(&derive("")));
    }

    #[test]
    fn fits_rejects_the_empty_and_the_oversized() {
        assert!(!fits(""));
        assert!(!fits("   "));
        assert!(fits("Quota ceiling"));
        assert!(fits(&"x".repeat(NAME_BUDGET)));
        assert!(!fits(&"x".repeat(NAME_BUDGET + 1)));
    }

    #[test]
    fn why_unfit_names_the_actual_failure() {
        assert_eq!(why_unfit("").as_deref(), Some("has no name"));
        assert!(
            why_unfit(&"x".repeat(NAME_BUDGET + 1))
                .unwrap()
                .contains("over the")
        );
        assert_eq!(why_unfit("Quota ceiling"), None);
    }

    /// A sentence-shaped label is exactly what this chunk exists to prevent, so
    /// the budget has to reject one outright.
    #[test]
    fn a_sentence_cannot_pass_as_a_name() {
        let sentence = "Every chunk's label is a sentence, so nothing in the roadmap can be a node";
        assert!(!fits(sentence));
    }

    /// An id family this roadmap has never held, seeded the way the engine
    /// seeds: every tail past three words vanishes, so five distinct chunks
    /// wear one word. The repair must separate them without moving the head.
    fn orchard() -> Vec<(String, String)> {
        [
            "orchard-plot-water",
            "orchard-plot-water-pump",
            "orchard-plot-water-piping",
            "orchard-plot-water-meter",
            "orchard-plot-water-meter-dial",
        ]
        .iter()
        .map(|id| ((*id).to_string(), derive(id)))
        .collect()
    }

    fn as_refs(table: &[(String, String)]) -> Vec<(&str, &str)> {
        table
            .iter()
            .map(|(id, name)| (id.as_str(), name.as_str()))
            .collect()
    }

    #[test]
    fn colliding_seeds_are_separated_and_the_family_head_keeps_its_label() {
        let table = orchard();
        // The premise: the seed really does tie all five.
        for (_, name) in &table {
            assert_eq!(name, "Orchard plot water");
        }
        let rewrites = repair_collisions(&as_refs(&table));

        // The head — the id that IS the shared prefix — is not rewritten.
        assert!(rewrites.iter().all(|(id, _)| id != "orchard-plot-water"));

        // Every other member now wears context + its distinguishing tail.
        let get = |id: &str| {
            rewrites
                .iter()
                .find(|(i, _)| i == id)
                .map(|(_, n)| n.as_str())
                .unwrap_or_else(|| panic!("{id} must be rewritten"))
        };
        assert_eq!(get("orchard-plot-water-pump"), "Water pump");
        assert_eq!(get("orchard-plot-water-piping"), "Water piping");
        assert_eq!(get("orchard-plot-water-meter"), "Water meter");
        assert_eq!(get("orchard-plot-water-meter-dial"), "Water meter dial");

        // The repair actually repaired: no two labels tie afterwards.
        let mut labels: Vec<&str> = table
            .iter()
            .map(|(id, name)| get_or(&rewrites, id, name))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(
            labels.len(),
            table.len(),
            "all five labels must be distinct"
        );
        for l in labels {
            assert!(fits(l), "{l:?} must satisfy fits()");
        }
    }

    fn get_or<'a>(rewrites: &'a [(String, String)], id: &str, current: &'a str) -> &'a str {
        rewrites
            .iter()
            .find(|(i, _)| i == id)
            .map_or(current, |(_, n)| n.as_str())
    }

    /// Members still tied after one pass share a longer prefix and recurse:
    /// `…-north-latch-upper` and `…-north-latch-lower` both truncate to
    /// "Gate north latch" until the recursion reaches the words that differ.
    #[test]
    fn nested_families_recurse_until_the_labels_differ() {
        let ids = [
            "mill-race-gate",
            "mill-race-gate-north",
            "mill-race-gate-north-latch-upper",
            "mill-race-gate-north-latch-lower",
        ];
        let table: Vec<(String, String)> = ids
            .iter()
            .map(|id| ((*id).to_string(), derive(id)))
            .collect();
        let rewrites = repair_collisions(&as_refs(&table));

        let finals: Vec<&str> = table
            .iter()
            .map(|(id, name)| get_or(&rewrites, id, name))
            .collect();
        assert_eq!(finals[0], "Mill race gate", "the head keeps its seed");
        assert_eq!(finals[1], "Gate north");
        assert_eq!(finals[2], "Latch upper");
        assert_eq!(finals[3], "Latch lower");
    }

    /// The two prohibitions, driven together: an authored name is never moved
    /// even when it sits inside a colliding class, and a label nobody collides
    /// with is never touched. The authored name here also squats on the label
    /// a seed would want, forcing the context window to widen past it.
    #[test]
    fn authored_names_and_uncolliding_labels_are_never_touched() {
        let table: Vec<(String, String)> = vec![
            // Two seeds tied at "Orchard plot water".
            (
                "orchard-plot-water-pump".into(),
                derive("orchard-plot-water-pump"),
            ),
            (
                "orchard-plot-water-piping".into(),
                derive("orchard-plot-water-piping"),
            ),
            // An authored member of the same class: name != derive(id).
            ("rainbarrel-stand".into(), "Orchard plot water".into()),
            // An authored squatter on the label the pump seed would take.
            ("cistern-overflow".into(), "Water pump".into()),
            // A singleton seed: collides with nothing, must not move.
            ("harbourmaster".into(), derive("harbourmaster")),
        ];
        assert_ne!(derive("rainbarrel-stand"), "Orchard plot water");
        let rewrites = repair_collisions(&as_refs(&table));

        assert!(
            rewrites.iter().all(|(id, _)| id != "rainbarrel-stand"),
            "an authored name is never rewritten, colliding or not"
        );
        assert!(
            rewrites.iter().all(|(id, _)| id != "cistern-overflow"),
            "an authored label outside the class is never rewritten"
        );
        assert!(
            rewrites.iter().all(|(id, _)| id != "harbourmaster"),
            "a label nobody collides with is never touched"
        );
        // The pump seed could not take "Water pump" (authored squatter): the
        // context window widens until the label is free.
        let pump = rewrites
            .iter()
            .find(|(id, _)| id == "orchard-plot-water-pump")
            .map(|(_, n)| n.as_str())
            .expect("the pump seed must move");
        assert_ne!(pump, "Water pump");
        assert!(fits(pump));
    }

    /// Deterministic and order-independent: the store's serialization order is
    /// an accident, and a label that depended on it would move between loads.
    #[test]
    fn repair_is_deterministic_and_order_independent() {
        let mut table = orchard();
        let forward = repair_collisions(&as_refs(&table));
        table.reverse();
        let reversed = repair_collisions(&as_refs(&table));
        assert_eq!(forward, reversed);
        assert!(!forward.is_empty());
    }

    /// Idempotent by construction: a rewritten name no longer equals its seed,
    /// so the next pass sees an authored-looking label and leaves it alone.
    #[test]
    fn a_second_pass_finds_nothing_left_to_repair() {
        let mut table = orchard();
        let rewrites = repair_collisions(&as_refs(&table));
        assert!(!rewrites.is_empty());
        for (id, name) in &mut table {
            if let Some((_, n)) = rewrites.iter().find(|(i, _)| i == id) {
                *name = n.clone();
            }
        }
        assert_eq!(repair_collisions(&as_refs(&table)), vec![]);
    }

    /// Nameless rows belong to the backfill, not to a label class — an empty
    /// string tying with another empty string is not a collision.
    #[test]
    fn empty_names_are_not_a_collision_class() {
        let table: Vec<(String, String)> = vec![
            ("first-nameless".into(), String::new()),
            ("second-nameless".into(), String::new()),
        ];
        assert_eq!(repair_collisions(&as_refs(&table)), vec![]);
    }
}
