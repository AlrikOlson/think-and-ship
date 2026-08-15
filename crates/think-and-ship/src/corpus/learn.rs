//! Learned next-best-action predictors (`eval-nba-model` + `eval-nba-v2`).
//!
//! Linear rankers over five inspectable, case-local candidate features. Every
//! static baseline in [`super::eval`] is a corner of this hypothesis space
//! (locality / lifo / priority+deps), so comparisons are fair by construction.
//!
//! The v2 additions, each grounded in the drift/stream-eval literature:
//! a **prequential** (test-then-train) driver — each case is
//! predicted by a model trained only on its past; **exponential recency
//! weighting** of training samples; a **pairwise hinge** loss that targets
//! the top-1 margin directly; and a **think-adjacency** feature joining the
//! corpus's chunk `cross_refs` (v2) against think-event timestamps so only
//! pre-case activity counts.
//!
//! Fully deterministic: zeros init, full-batch gradient steps, fixed lr and
//! epoch count, no randomness anywhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::eval::{ChunkView, ReplayCase};
use super::{Corpus, EventKind};

/// Feature names, in weight order (printed with trained weights).
pub const FEATURES: [&str; 5] = [
    "locality",
    "neg_priority",
    "deps_ready",
    "recency",
    "think_adjacency",
];

const EPOCHS: usize = 300;
const LR: f64 = 0.5;

/// Corpus-derived context the feature extractor joins against.
#[derive(Debug, Default, Clone)]
pub struct FeatureContext {
    /// think step id (e.g. "1090") → its RFC 3339 timestamp.
    think_times: BTreeMap<String, String>,
    /// chunk id → think step ids it links (`think:N` cross_refs, v2 corpora).
    chunk_links: BTreeMap<String, Vec<String>>,
}

impl FeatureContext {
    pub fn from_corpus(corpus: &Corpus) -> Self {
        let mut ctx = Self::default();
        for e in &corpus.events {
            match e.kind {
                EventKind::ThinkStep => {
                    if !e.created.is_empty() {
                        ctx.think_times.insert(e.id.clone(), e.created.clone());
                    }
                }
                EventKind::Chunk => {
                    let steps: Vec<String> = e
                        .cross_refs
                        .iter()
                        .filter_map(|r| r.strip_prefix("think:").map(str::to_owned))
                        .collect();
                    if !steps.is_empty() {
                        ctx.chunk_links.insert(e.id.clone(), steps);
                    }
                }
                EventKind::Signal => {}
            }
        }
        ctx
    }

    /// Think steps linked to `chunk` whose own timestamp precedes `at`.
    /// Caveat: the link set is the
    /// chunk's FINAL cross_refs — links are normally created at step time,
    /// so `timestamp < at` is a faithful proxy for link-known-at-`at`, but a
    /// retroactively-added link would count optimistically.
    fn adjacency(&self, chunk: &str, at: &str) -> usize {
        self.chunk_links
            .get(chunk)
            .map(|steps| {
                steps
                    .iter()
                    .filter(|s| self.think_times.get(*s).is_some_and(|t| t.as_str() < at))
                    .count()
            })
            .unwrap_or(0)
    }
}

fn shared_prefix_tokens(a: &str, b: &str) -> usize {
    a.split('-')
        .zip(b.split('-'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// A dep still in the candidate set is by definition not closed before
/// `case.at`; a dep absent from the set either closed earlier (ready) or
/// never existed (treated as ready, matching the engine's `deps_satisfied`
/// semantics for unknown ids).
fn deps_done_before(candidate: &ChunkView, case: &ReplayCase) -> bool {
    candidate
        .deps
        .iter()
        .all(|dep| !case.candidates.iter().any(|c| &c.id == dep))
}

/// Raw per-candidate features. All inputs are case-local or pre-`case.at` —
/// nothing peeks forward, so temporal protocols cannot leak.
fn raw_features(candidate: &ChunkView, case: &ReplayCase, ctx: &FeatureContext) -> [f64; 5] {
    let last = case.last_completed.as_deref().unwrap_or_default();
    [
        shared_prefix_tokens(&candidate.id, last) as f64,
        -(candidate.priority as f64),
        f64::from(u8::from(deps_done_before(candidate, case))),
        case.candidates
            .iter()
            .filter(|c| c.created < candidate.created)
            .count() as f64,
        ctx.adjacency(&candidate.id, &case.at) as f64,
    ]
}

/// Per-case z-score normalization so weights are scale-free. An ablated
/// (all-zero) feature normalizes to zero and contributes nothing.
fn case_features(case: &ReplayCase, ctx: &FeatureContext, use_adjacency: bool) -> Vec<[f64; 5]> {
    let mut raw: Vec<[f64; 5]> = case
        .candidates
        .iter()
        .map(|c| raw_features(c, case, ctx))
        .collect();
    if !use_adjacency {
        for f in &mut raw {
            f[4] = 0.0;
        }
    }
    let n = raw.len().max(1) as f64;
    let mut out = raw.clone();
    for k in 0..5 {
        let mean = raw.iter().map(|f| f[k]).sum::<f64>() / n;
        let var = raw.iter().map(|f| (f[k] - mean).powi(2)).sum::<f64>() / n;
        let sd = var.sqrt();
        for (o, r) in out.iter_mut().zip(&raw) {
            o[k] = if sd > 1e-12 { (r[k] - mean) / sd } else { 0.0 };
        }
    }
    out
}

/// Training objective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loss {
    /// Softmax NLL over the candidate list (probability mass).
    Listwise,
    /// Hinge on the actual-vs-rival margin (targets top-1 directly).
    Pairwise,
}

/// A trained-variant recipe. `decay` is the per-case-age multiplier on
/// sample weight (1.0 = uniform); fixed a priori — never tuned on test data.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TrainOpts {
    pub loss: Loss,
    pub decay: f64,
    pub use_adjacency: bool,
}

/// Trained model: a weight per feature, pinned to the corpus it learned from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub corpus_digest: String,
    pub features: Vec<String>,
    pub weights: [f64; 5],
    pub opts: TrainOpts,
    pub train_cases: usize,
}

fn dot(w: &[f64; 5], f: &[f64; 5]) -> f64 {
    w.iter().zip(f).map(|(a, b)| a * b).sum()
}

/// Train on `cases` (sample i weighted by `decay^(n-1-i)` — newest heaviest)
/// by full-batch gradient descent. Deterministic.
pub fn train(
    cases: &[ReplayCase],
    ctx: &FeatureContext,
    opts: TrainOpts,
    corpus_digest: &str,
) -> Model {
    let prepared: Vec<(Vec<[f64; 5]>, usize, f64)> = cases
        .iter()
        .enumerate()
        .filter_map(|(i, case)| {
            let target = case.candidates.iter().position(|c| c.id == case.actual)?;
            let age = (cases.len() - 1 - i) as f64;
            Some((
                case_features(case, ctx, opts.use_adjacency),
                target,
                opts.decay.powf(age),
            ))
        })
        .collect();
    let total_weight: f64 = prepared.iter().map(|(_, _, w)| w).sum::<f64>().max(1e-12);

    let mut w = [0.0f64; 5];
    for _ in 0..EPOCHS {
        let mut grad = [0.0f64; 5];
        for (feats, target, sample_w) in &prepared {
            match opts.loss {
                Loss::Listwise => {
                    let scores: Vec<f64> = feats.iter().map(|f| dot(&w, f)).collect();
                    let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let exps: Vec<f64> = scores.iter().map(|s| (s - max).exp()).collect();
                    let z: f64 = exps.iter().sum();
                    for (i, f) in feats.iter().enumerate() {
                        let p = exps[i] / z;
                        let indicator = f64::from(u8::from(i == *target));
                        for k in 0..5 {
                            grad[k] += sample_w * (indicator - p) * f[k];
                        }
                    }
                }
                Loss::Pairwise => {
                    let target_f = &feats[*target];
                    let target_s = dot(&w, target_f);
                    for (i, f) in feats.iter().enumerate() {
                        if i == *target {
                            continue;
                        }
                        if 1.0 - (target_s - dot(&w, f)) > 0.0 {
                            for k in 0..5 {
                                grad[k] += sample_w * (target_f[k] - f[k]);
                            }
                        }
                    }
                }
            }
        }
        for k in 0..5 {
            w[k] += LR * grad[k] / total_weight;
        }
    }

    Model {
        corpus_digest: corpus_digest.to_owned(),
        features: FEATURES.iter().map(|s| (*s).to_owned()).collect(),
        weights: w,
        opts,
        train_cases: prepared.len(),
    }
}

/// Rank a case's candidates by a trained model, best first (id tie-break).
pub fn rank(model: &Model, case: &ReplayCase, ctx: &FeatureContext) -> Vec<String> {
    let feats = case_features(case, ctx, model.opts.use_adjacency);
    let mut scored: Vec<(f64, &str)> = case
        .candidates
        .iter()
        .zip(&feats)
        .map(|(c, f)| (dot(&model.weights, f), c.id.as_str()))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(b.1))
    });
    scored.into_iter().map(|(_, id)| id.to_owned()).collect()
}

/// Time-ordered split (31j-b's protocol, kept for continuity): first
/// `train_frac` of cases train, the rest are holdout.
pub fn temporal_split(cases: &[ReplayCase], train_frac: f64) -> (&[ReplayCase], &[ReplayCase]) {
    let cut = ((cases.len() as f64) * train_frac).floor() as usize;
    cases.split_at(cut.min(cases.len()))
}

/// Prequential (test-then-train) driver — the streaming-eval standard: each
/// case `t >= warmup` is predicted by a model trained ONLY on cases `< t`,
/// then absorbed. Returns (hits, final model) via the score struct of
/// [`super::eval`]. Deterministic; leak-free by construction.
pub fn prequential(
    cases: &[ReplayCase],
    ctx: &FeatureContext,
    warmup: usize,
    opts: TrainOpts,
    name: &str,
) -> super::eval::Score {
    let mut top1 = 0;
    let mut top3 = 0;
    let mut tested = 0;
    for t in warmup..cases.len() {
        let model = train(&cases[..t], ctx, opts, "");
        let ranking = rank(&model, &cases[t], ctx);
        tested += 1;
        if ranking.first() == Some(&cases[t].actual) {
            top1 += 1;
        }
        if ranking.iter().take(3).any(|id| id == &cases[t].actual) {
            top3 += 1;
        }
    }
    super::eval::Score {
        name: name.to_owned(),
        cases: tested,
        top1,
        top3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::build_corpus;
    use crate::corpus::eval::{replay_cases, score};
    use crate::roadmap::domain::{Chunk, ChunkStatus};

    fn chunk(id: &str, status: ChunkStatus, priority: u32, created: &str, updated: &str) -> Chunk {
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
            deps: vec![],
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

    fn t(day: u32) -> String {
        format!("2026-02-{day:02}T00:00:00Z")
    }

    const LISTWISE: TrainOpts = TrainOpts {
        loss: Loss::Listwise,
        decay: 1.0,
        use_adjacency: false,
    };
    const PAIRWISE: TrainOpts = TrainOpts {
        loss: Loss::Pairwise,
        decay: 1.0,
        use_adjacency: false,
    };

    /// A long phase train where priority always points away from the truth.
    fn train_corpus() -> crate::corpus::Corpus {
        let mut chunks = vec![chunk("decoy", ChunkStatus::Pending, 1, &t(1), &t(1))];
        for i in 1..=10u32 {
            chunks.push(chunk(
                &format!("train-x-{i}"),
                ChunkStatus::Done,
                50 + i,
                &t(1),
                &t(i + 1),
            ));
        }
        build_corpus("t", &chunks, std::iter::empty(), &[])
    }

    #[test]
    fn both_losses_discover_locality_and_beat_the_priority_corner() {
        let corpus = train_corpus();
        let cases = replay_cases(&corpus);
        let ctx = FeatureContext::from_corpus(&corpus);
        for opts in [LISTWISE, PAIRWISE] {
            let (tr, ho) = temporal_split(&cases, 0.6);
            let model = train(tr, &ctx, opts, "sha256:test");
            let learned = score("learned", ho, |c| rank(&model, c, &ctx));
            let prio = score("prio", ho, crate::corpus::eval::predictor_priority_rule);
            assert!(
                learned.top1 > prio.top1,
                "{opts:?}: learned {} vs prio {} (weights {:?})",
                learned.top1,
                prio.top1,
                model.weights
            );
        }
    }

    #[test]
    fn prequential_is_deterministic_and_leak_bounded() {
        let corpus = train_corpus();
        let cases = replay_cases(&corpus);
        let ctx = FeatureContext::from_corpus(&corpus);
        let a = prequential(&cases, &ctx, 3, LISTWISE, "x");
        let b = prequential(&cases, &ctx, 3, LISTWISE, "x");
        assert_eq!(a, b);
        assert_eq!(
            a.cases,
            cases.len() - 3,
            "exactly the post-warmup cases are tested"
        );
    }

    #[test]
    fn recency_decay_tracks_a_regime_change() {
        // Early regime: completions follow PRIORITY (locality misleads).
        // Late regime: completions follow LOCALITY. A decayed model evaluated
        // prequentially must do at least as well as uniform on the late cases.
        let mut chunks = Vec::new();
        // Early: scattered ids, priority-ordered completion.
        for i in 1..=8u32 {
            chunks.push(chunk(
                &format!("alpha{i}-z"),
                ChunkStatus::Done,
                i,
                &t(1),
                &t(i + 1),
            ));
        }
        // Late: a phase train with anti-priority ordering.
        for i in 1..=8u32 {
            chunks.push(chunk(
                &format!("beta-x-{i}"),
                ChunkStatus::Done,
                100 - i,
                &t(9),
                &t(9 + i),
            ));
        }
        chunks.push(chunk("noise", ChunkStatus::Pending, 50, &t(1), &t(1)));
        let corpus = build_corpus("t", &chunks, std::iter::empty(), &[]);
        let cases = replay_cases(&corpus);
        let ctx = FeatureContext::from_corpus(&corpus);
        let uniform = prequential(&cases, &ctx, 4, LISTWISE, "u");
        let decayed = prequential(
            &cases,
            &ctx,
            4,
            TrainOpts {
                loss: Loss::Listwise,
                decay: 0.8,
                use_adjacency: false,
            },
            "d",
        );
        assert!(
            decayed.top1 >= uniform.top1,
            "decayed {} vs uniform {}",
            decayed.top1,
            uniform.top1
        );
    }

    #[test]
    fn think_adjacency_counts_only_pre_case_steps() {
        let mut c = chunk("with-links", ChunkStatus::Pending, 5, &t(1), &t(1));
        c.cross_refs = vec!["think:7".into(), "think:9".into(), "task:x".into()];
        let chunks = vec![c, chunk("plain", ChunkStatus::Done, 9, &t(1), &t(5))];
        let step = |n: u32, ts: String| {
            let mut s: crate::think::domain::step::ThinkStep =
                serde_json::from_str(&format!("{{\"step_number\":{n}}}")).expect("step");
            s.timestamp = Some(ts);
            s
        };
        // step 7 precedes the case at t(5); step 9 is after — must NOT count.
        let steps = [step(7, t(2)), step(9, t(8))];
        let corpus = build_corpus("t", &chunks, steps.iter(), &[]);
        let ctx = FeatureContext::from_corpus(&corpus);
        assert_eq!(ctx.adjacency("with-links", &t(5)), 1);
        assert_eq!(ctx.adjacency("with-links", &t(1)), 0);
        assert_eq!(ctx.adjacency("plain", &t(5)), 0);
    }
}
