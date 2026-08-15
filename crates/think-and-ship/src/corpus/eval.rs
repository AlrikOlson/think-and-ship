//! Offline eval harness over the corpus: replay roadmap history
//! and score "which chunk completes next" predictors on top-1/top-3 accuracy.
//!
//! Replay model — reconstructed from what the stores actually persist
//! (current state + timestamps; there is NO transition log):
//! - completion events = chunk events with `status == done`, ordered by
//!   `closed` (≈ completion time; post-completion cross-ref touches add small
//!   uniform noise);
//! - at each completion instant `t`, the candidate set is every chunk already
//!   created (`created <= t`) and not yet closed (`closed` absent or `>= t`);
//! - a predictor ranks the candidates; top-1/top-3 score against the chunk
//!   that actually completed.
//!
//! Honest approximation: a chunk's backlog-vs-pending status AT time `t` is
//! unreconstructable, so the "product rule" baseline replays the core of
//! `RoadmapEngine::next()` — deps-ready first, then priority — without the
//! historical pending filter. All ordering is fully tie-broken, so the same
//! corpus always yields the same numbers.

use std::collections::BTreeMap;

use super::{Corpus, CorpusEvent, EventKind};

/// A chunk's structural view inside the replay.
#[derive(Debug, Clone)]
pub struct ChunkView {
    pub id: String,
    pub created: String,
    pub closed: Option<String>,
    pub done: bool,
    pub priority: u32,
    pub deps: Vec<String>,
}

/// One scoreable replay case: the instant `at`, the chunk that actually
/// completed, the open candidate set, and the previously-completed chunk
/// (locality context).
#[derive(Debug)]
pub struct ReplayCase {
    pub at: String,
    pub actual: String,
    pub candidates: Vec<ChunkView>,
    pub last_completed: Option<String>,
}

/// A next-chunk predictor: rank the candidate ids, best first. Must be a
/// total, deterministic ordering of the given candidates.
pub type Predictor = fn(&ReplayCase) -> Vec<String>;

fn chunk_views(corpus: &Corpus) -> Vec<ChunkView> {
    corpus
        .events
        .iter()
        .filter(|e| e.kind == EventKind::Chunk)
        .map(|e: &CorpusEvent| ChunkView {
            id: e.id.clone(),
            created: e.created.clone(),
            closed: e.closed.clone(),
            done: e.status.as_deref() == Some("done"),
            priority: e.priority.unwrap_or(u32::MAX),
            deps: e.deps.clone(),
        })
        .collect()
}

/// Derive the time-ordered replay cases from a corpus. Cases with fewer than
/// two candidates are skipped (nothing to predict).
pub fn replay_cases(corpus: &Corpus) -> Vec<ReplayCase> {
    let views = chunk_views(corpus);
    // Completion timeline: done chunks ordered by (closed, id) — total order.
    let mut completions: Vec<&ChunkView> = views
        .iter()
        .filter(|v| v.done && v.closed.is_some())
        .collect();
    completions.sort_by(|a, b| (a.closed.as_deref(), &a.id).cmp(&(b.closed.as_deref(), &b.id)));

    let mut cases = Vec::new();
    let mut last_completed: Option<String> = None;
    for completion in completions {
        let t = completion.closed.as_deref().unwrap_or_default();
        let candidates: Vec<ChunkView> = views
            .iter()
            .filter(|v| v.created.as_str() <= t && v.closed.as_deref().is_none_or(|c| c >= t))
            .cloned()
            .collect();
        if candidates.len() >= 2 {
            cases.push(ReplayCase {
                at: t.to_owned(),
                actual: completion.id.clone(),
                candidates,
                last_completed: last_completed.clone(),
            });
        }
        last_completed = Some(completion.id.clone());
    }
    cases
}

fn deps_done_before(
    candidate: &ChunkView,
    case: &ReplayCase,
    by_id: &BTreeMap<&str, &ChunkView>,
) -> bool {
    candidate.deps.iter().all(|dep| {
        by_id
            .get(dep.as_str())
            .is_none_or(|d| d.done && d.closed.as_deref().is_some_and(|c| c < case.at.as_str()))
    })
}

fn ranked<F, K>(case: &ReplayCase, key: F) -> Vec<String>
where
    F: Fn(&ChunkView) -> K,
    K: Ord,
{
    let mut ordered: Vec<&ChunkView> = case.candidates.iter().collect();
    ordered.sort_by_key(|c| (key(c), c.id.clone()));
    ordered.into_iter().map(|c| c.id.clone()).collect()
}

/// The product's static rule (the core of `RoadmapEngine::next()`): deps-ready
/// candidates first, then lowest priority. See the module header for why the
/// historical pending filter cannot be replayed.
pub fn predictor_priority_rule(case: &ReplayCase) -> Vec<String> {
    let by_id: BTreeMap<&str, &ChunkView> =
        case.candidates.iter().map(|c| (c.id.as_str(), c)).collect();
    ranked(case, |c| {
        (
            u8::from(!deps_done_before(c, case, &by_id)),
            c.priority,
            c.created.clone(),
        )
    })
}

fn shared_prefix_tokens(a: &str, b: &str) -> usize {
    a.split('-')
        .zip(b.split('-'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// Phase-locality rival: prefer candidates whose id shares the longest
/// dash-token prefix with the most recently completed chunk (work happens in
/// phase trains), then lowest priority. With no history it degrades to the
/// priority rule's shape.
pub fn predictor_locality(case: &ReplayCase) -> Vec<String> {
    let last = case.last_completed.clone().unwrap_or_default();
    ranked(case, |c| {
        let lcp = shared_prefix_tokens(&c.id, &last);
        (usize::MAX - lcp, c.priority, c.created.clone())
    })
}

/// Creation-recency rival (LIFO): newest chunk first — "you finish what you
/// just filed".
pub fn predictor_lifo(case: &ReplayCase) -> Vec<String> {
    ranked(case, |c| std::cmp::Reverse(c.created.clone()))
}

/// Accuracy of one predictor over the replay.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub name: String,
    pub cases: usize,
    pub top1: usize,
    pub top3: usize,
}

impl Score {
    pub fn pct(hits: usize, n: usize) -> f64 {
        if n == 0 {
            0.0
        } else {
            100.0 * hits as f64 / n as f64
        }
    }
}

/// Score a predictor over the cases. Deterministic for deterministic
/// predictors (every baseline here is fully tie-broken). Generic so a
/// trained model's closure scores through the same path as the fn-pointer
/// baselines (eval-nba-model).
pub fn score<F: Fn(&ReplayCase) -> Vec<String>>(
    name: &str,
    cases: &[ReplayCase],
    predictor: F,
) -> Score {
    let mut top1 = 0;
    let mut top3 = 0;
    for case in cases {
        let ranking = predictor(case);
        if ranking.first() == Some(&case.actual) {
            top1 += 1;
        }
        if ranking.iter().take(3).any(|id| id == &case.actual) {
            top3 += 1;
        }
    }
    Score {
        name: name.to_owned(),
        cases: cases.len(),
        top1,
        top3,
    }
}

/// All shipped baselines, in report order.
pub fn baselines() -> Vec<(&'static str, Predictor)> {
    vec![
        ("priority-rule (product next())", predictor_priority_rule),
        ("phase-locality", predictor_locality),
        ("lifo (newest-first)", predictor_lifo),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{CORPUS_VERSION, build_corpus};
    use crate::roadmap::domain::{Chunk, ChunkStatus};

    fn chunk(
        id: &str,
        status: ChunkStatus,
        priority: u32,
        created: &str,
        updated: &str,
        deps: &[&str],
    ) -> Chunk {
        Chunk {
            tier: None,
            id: id.into(),
            title: id.into(),
            name: crate::roadmap::name::derive(id),
            status,
            priority,
            description: String::new(),
            content: None,
            group: None,
            notes: String::new(),
            acceptance: vec![],
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            cross_refs: vec![],
            shared: false,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            project_id: None,
            created_at: created.into(),
            updated_at: updated.into(),
        }
    }

    fn t(day: u32, hour: u32) -> String {
        format!("2026-01-{day:02}T{hour:02}:00:00Z")
    }

    /// A phase-train history: phase-a-1 then phase-a-2 complete back to back
    /// even though unrelated chunk `urgent` has the lowest priority number.
    fn train() -> Corpus {
        let chunks = vec![
            chunk("phase-a-1", ChunkStatus::Done, 50, &t(1, 0), &t(2, 0), &[]),
            chunk("phase-a-2", ChunkStatus::Done, 60, &t(1, 0), &t(3, 0), &[]),
            chunk("urgent", ChunkStatus::Pending, 10, &t(1, 0), &t(1, 0), &[]),
            chunk("later", ChunkStatus::Backlog, 90, &t(1, 0), &t(1, 0), &[]),
        ];
        build_corpus("test", &chunks, std::iter::empty(), &[])
    }

    #[test]
    fn replay_derives_time_ordered_cases_with_open_candidates_only() {
        let cases = replay_cases(&train());
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].actual, "phase-a-1");
        assert_eq!(cases[0].last_completed, None);
        assert_eq!(cases[1].actual, "phase-a-2");
        assert_eq!(cases[1].last_completed.as_deref(), Some("phase-a-1"));
        // phase-a-1 closed strictly before case 2 → no longer a candidate.
        assert!(
            !cases[1].candidates.iter().any(|c| c.id == "phase-a-1"),
            "closed chunks must leave the candidate set"
        );
    }

    #[test]
    fn locality_beats_priority_on_a_phase_train() {
        let cases = replay_cases(&train());
        let prio = score("p", &cases, predictor_priority_rule);
        let loc = score("l", &cases, predictor_locality);
        // The priority rule keeps predicting `urgent` (priority 10) and never
        // hits; locality follows the train and hits case 2.
        assert_eq!(prio.top1, 0);
        assert_eq!(loc.top1, 1);
        assert!(loc.top1 > prio.top1);
    }

    #[test]
    fn unready_deps_rank_below_ready_candidates() {
        let chunks = vec![
            chunk("done-early", ChunkStatus::Done, 99, &t(1, 0), &t(2, 0), &[]),
            chunk(
                "gated",
                ChunkStatus::Done,
                1,
                &t(1, 0),
                &t(4, 0),
                &["blocker"],
            ),
            chunk("blocker", ChunkStatus::Done, 5, &t(1, 0), &t(3, 0), &[]),
            chunk("free", ChunkStatus::Pending, 7, &t(1, 0), &t(1, 0), &[]),
        ];
        let corpus = build_corpus("test", &chunks, std::iter::empty(), &[]);
        let cases = replay_cases(&corpus);
        // Case at t(3,0): `gated`'s dep (`blocker`) completes AT this instant,
        // not before — so `gated` (priority 1) must rank below ready `free`.
        let case = cases.iter().find(|c| c.at == t(3, 0)).expect("case");
        let ranking = predictor_priority_rule(case);
        let pos = |id: &str| ranking.iter().position(|r| r == id).unwrap();
        assert!(pos("free") < pos("gated"), "ranking: {ranking:?}");
    }

    #[test]
    fn scoring_is_deterministic_across_runs() {
        let cases = replay_cases(&train());
        let a = score("x", &cases, predictor_priority_rule);
        let b = score("x", &replay_cases(&train()), predictor_priority_rule);
        assert_eq!(a, b);
        assert_eq!(CORPUS_VERSION, 2);
    }

    #[test]
    fn lifo_prefers_newest() {
        let chunks = vec![
            chunk("old", ChunkStatus::Done, 1, &t(1, 0), &t(5, 0), &[]),
            chunk("new", ChunkStatus::Done, 9, &t(4, 0), &t(6, 0), &[]),
        ];
        let corpus = build_corpus("test", &chunks, std::iter::empty(), &[]);
        let case = &replay_cases(&corpus)[0];
        assert_eq!(
            predictor_lifo(case).first().map(String::as_str),
            Some("new")
        );
    }
}
