//! Read-only aggregations — every method that produces a JSON view of
//! engine state. None of these mutates; the engine guarantees they're
//! safe to call from inside response paths and from observer code.
//!
//! Includes `history_with_branches` (the internal builder used by
//! `branch_tree` and `export_history` in [`super::export`]) so all
//! "serialize the current state" code lives in one file.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::domain::{Branch, BranchStatus, DeliberateHistory, DeliberateStep};
use crate::util::text::truncate_excerpt;

use super::core::ReasoningServer;

impl ReasoningServer {
    /// Clone the current history with branches folded into the
    /// `branches` field — the on-disk shape, suitable for export.
    /// Internal helper shared by `branch_tree`, `export_history`, and
    /// `history_snapshot`.
    pub(crate) fn history_with_branches(&self) -> DeliberateHistory {
        let mut snapshot = self.history.clone();
        let mut branches: Vec<Branch> = self.branches.values().cloned().collect();
        branches.sort_by(|a, b| a.from_step.cmp(&b.from_step).then(a.id.cmp(&b.id)));
        snapshot.branches = Some(branches);
        snapshot
    }

    /// JSON snapshot of the full history with branches resolved.
    pub fn history_snapshot(&self) -> Value {
        serde_json::to_value(self.history_with_branches()).unwrap_or(Value::Null)
    }

    /// Compact per-branch JSON snapshots, sorted by `from_step`.
    pub fn branches_snapshot(&self) -> Vec<Value> {
        let mut branches: Vec<Branch> = self.branches.values().cloned().collect();
        branches.sort_by(|a, b| a.from_step.cmp(&b.from_step).then(a.id.cmp(&b.id)));
        branches
            .into_iter()
            .filter_map(|b| serde_json::to_value(b).ok())
            .collect()
    }

    /// Per-session summary used by the `deliberate_sessions` tool. Sorted
    /// most-recent first.
    pub fn sessions_snapshot(&self) -> Vec<Value> {
        let mut entries: Vec<_> = self.sessions.iter().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1.last_accessed));
        entries
            .into_iter()
            .map(|(id, e)| {
                let last_accessed = u64::try_from(e.last_accessed).unwrap_or(u64::MAX);
                json!({
                    "session_id": id,
                    "step_count": e.history.steps.len(),
                    "completed": e.history.completed,
                    "last_accessed_ms": last_accessed,
                    "active": self.active_session.as_deref() == Some(id),
                })
            })
            .collect()
    }

    /// Last `limit` steps from main history, in chronological order, with
    /// optional exclusion of a single step number (used by the
    /// response-enrichment path to avoid echoing the just-added step).
    ///
    /// Pinned steps are folded in regardless of their chronological position
    /// so that load-bearing conclusions stay visible after the trace grows
    /// past the window.
    pub fn recent_steps_rollup(&self, limit: usize, exclude_step: Option<u32>) -> Vec<Value> {
        let mut selected: Vec<&DeliberateStep> = Vec::new();
        let mut selected_nums: HashSet<u32> = HashSet::new();

        // First pass: pinned steps that aren't excluded.
        for step in &self.history.steps {
            if exclude_step == Some(step.step_number) {
                continue;
            }
            if step.pinned.unwrap_or(false) && selected_nums.insert(step.step_number) {
                selected.push(step);
            }
        }

        // Second pass: walk recent-first and pick up to `limit` total.
        for step in self.history.steps.iter().rev() {
            if selected.len() >= limit {
                break;
            }
            if exclude_step == Some(step.step_number) {
                continue;
            }
            if selected_nums.insert(step.step_number) {
                selected.push(step);
            }
        }

        // Render in step-number order so the rollup reads chronologically.
        // Null/false fields are omitted so the rollup stays tight at depth.
        // `rationale_excerpt` captures the WHY of each prior step — most
        // valuable when navigating mid-trace, currently invisible after a
        // step falls out of the model's own context.
        selected.sort_by_key(|s| s.step_number);
        selected
            .into_iter()
            .map(|s| {
                let mut obj = Map::new();
                obj.insert("n".into(), json!(s.step_number));
                obj.insert("purpose".into(), json!(s.purpose));
                obj.insert(
                    "thought_excerpt".into(),
                    json!(truncate_excerpt(&s.thought, 80)),
                );
                if !s.rationale.trim().is_empty() {
                    obj.insert(
                        "rationale_excerpt".into(),
                        json!(truncate_excerpt(&s.rationale, 60)),
                    );
                }
                if let Some(c) = s.confidence {
                    obj.insert("confidence".into(), json!(c));
                }
                if let Some(r) = s.revised_by {
                    obj.insert("revised_by".into(), json!(r));
                }
                if let Some(b) = &s.branch_id {
                    obj.insert("branch_id".into(), json!(b));
                }
                if s.pinned.unwrap_or(false) {
                    obj.insert("pinned".into(), json!(true));
                }
                Value::Object(obj)
            })
            .collect()
    }

    /// Compact branch overview suitable for inclusion in step responses.
    pub fn branches_summary(&self) -> Vec<Value> {
        let mut v: Vec<&Branch> = self.branches.values().collect();
        v.sort_by(|a, b| a.from_step.cmp(&b.from_step).then(a.id.cmp(&b.id)));
        v.into_iter()
            .map(|b| {
                json!({
                    "id": b.id,
                    "name": b.name,
                    "from_step": b.from_step,
                    "steps": b.steps.len(),
                    "status": match b.status {
                        BranchStatus::Active => "active",
                        BranchStatus::Merged => "merged",
                        BranchStatus::Abandoned => "abandoned",
                    },
                    "depth": b.depth,
                })
            })
            .collect()
    }

    /// Trace-wide metacognitive snapshot. Aggregates per-step warnings into
    /// patterns that only show up when you look across the whole history:
    /// open hypotheses with no defenders, stale branches that nobody closed,
    /// confidence drift, dependencies hanging off a revised step.
    pub fn checkpoint_snapshot(&self) -> Value {
        // open_hypotheses: purpose=hypothesis with no downstream step whose
        // purpose is validation or correction.
        let mut open_hypotheses: Vec<Value> = Vec::new();
        for step in &self.history.steps {
            if !step.purpose.eq_ignore_ascii_case("hypothesis") {
                continue;
            }
            let dependents = self.direct_dependents(step.step_number);
            let has_check = dependents.iter().any(|n| {
                self.step_by_number(*n)
                    .map(|s| {
                        matches!(
                            s.purpose.to_ascii_lowercase().as_str(),
                            "validation" | "correction"
                        )
                    })
                    .unwrap_or(false)
            });
            if !has_check {
                open_hypotheses.push(json!({
                    "step_number": step.step_number,
                    "thought_excerpt": truncate_excerpt(&step.thought, 80),
                    "confidence": step.confidence,
                }));
            }
        }

        // stale_branches: active branches whose latest step is more than
        // (max_history_size / 4) steps behind the latest main step.
        let stale_threshold = (self.config.system.max_history_size / 4).max(2) as u32;
        let head = self
            .history
            .steps
            .last()
            .map(|s| s.step_number)
            .unwrap_or(0);
        let mut stale_branches: Vec<Value> = Vec::new();
        for branch in self.branches.values() {
            if !matches!(branch.status, BranchStatus::Active) {
                continue;
            }
            let last_step = branch
                .steps
                .iter()
                .map(|s| s.step_number)
                .max()
                .unwrap_or(branch.from_step);
            if head.saturating_sub(last_step) >= stale_threshold {
                stale_branches.push(json!({
                    "id": branch.id,
                    "name": branch.name,
                    "last_step": last_step,
                    "steps_behind": head.saturating_sub(last_step),
                }));
            }
        }

        // confidence_trend: slope of the last 5 confidences. Reported as a
        // qualitative label so the model can react without doing math.
        let recent_conf: Vec<f64> = self
            .history
            .steps
            .iter()
            .rev()
            .filter_map(|s| s.confidence)
            .take(5)
            .collect();
        let trend = if recent_conf.len() < 2 {
            "insufficient_data"
        } else {
            // recent_conf is in reverse order — newest first. Compute slope
            // from oldest to newest by walking from the end.
            let xs: Vec<f64> = (0..recent_conf.len()).map(|i| i as f64).collect();
            let ys: Vec<f64> = recent_conf.iter().rev().copied().collect();
            let n = xs.len() as f64;
            let sx: f64 = xs.iter().sum();
            let sy: f64 = ys.iter().sum();
            let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| x * y).sum();
            let sxx: f64 = xs.iter().map(|x| x * x).sum();
            let denom = n * sxx - sx * sx;
            let slope = if denom == 0.0 {
                0.0
            } else {
                (n * sxy - sx * sy) / denom
            };
            if slope > 0.05 {
                "rising"
            } else if slope < -0.05 {
                "falling"
            } else {
                "stable"
            }
        };

        // revised_but_undefended: steps that have been revised AND have at
        // least one dependent whose step number is GREATER than the revising
        // step's number but did NOT explicitly cite the revising step. That
        // dependent built on the older form without acknowledging the update.
        let mut revised_undefended: Vec<Value> = Vec::new();
        for step in &self.history.steps {
            let Some(rev_by) = step.revised_by else {
                continue;
            };
            let undefended: Vec<u32> = self
                .all_steps()
                .filter(|s| s.step_number > rev_by)
                .filter(|s| {
                    s.dependencies.as_ref().is_some_and(|d| {
                        d.iter().any(|e| e.step() == step.step_number)
                            && !d.iter().any(|e| e.step() == rev_by)
                    })
                })
                .map(|s| s.step_number)
                .collect();
            if !undefended.is_empty() {
                revised_undefended.push(json!({
                    "step_number": step.step_number,
                    "revised_by": rev_by,
                    "depending_steps_unaware": undefended,
                }));
            }
        }

        // refuted_chain_alerts: steps where any transitive upstream dep has
        // been refuted somewhere in the trace.
        let mut refuted_alerts: Vec<Value> = Vec::new();
        for step in &self.history.steps {
            let upstream = self.walk_deps(step.step_number, 256);
            let mut refuted_in_chain: Vec<u32> = Vec::new();
            for anc in &upstream {
                if !self
                    .refuters_of(*anc, /* exclude */ step.step_number)
                    .is_empty()
                {
                    refuted_in_chain.push(*anc);
                }
            }
            if !refuted_in_chain.is_empty() {
                refuted_alerts.push(json!({
                    "step_number": step.step_number,
                    "refuted_ancestors": refuted_in_chain,
                }));
            }
        }

        json!({
            "open_hypotheses": open_hypotheses,
            "stale_branches": stale_branches,
            "confidence_trend": trend,
            "revised_but_undefended": revised_undefended,
            "refuted_chain_alerts": refuted_alerts,
        })
    }

    /// All currently-pinned steps in step-number order. Returns compact
    /// step descriptors suitable for an "anchors" view. Mirrors
    /// `recent_steps_rollup`'s null-omission policy.
    pub fn pinned_steps(&self) -> Vec<Value> {
        let mut pinned: Vec<&DeliberateStep> = self
            .history
            .steps
            .iter()
            .filter(|s| s.pinned.unwrap_or(false))
            .collect();
        pinned.sort_by_key(|s| s.step_number);
        pinned
            .into_iter()
            .map(|s| {
                let mut obj = Map::new();
                obj.insert("step_number".into(), json!(s.step_number));
                obj.insert("purpose".into(), json!(s.purpose));
                obj.insert(
                    "thought_excerpt".into(),
                    json!(truncate_excerpt(&s.thought, 120)),
                );
                obj.insert(
                    "outcome_excerpt".into(),
                    json!(truncate_excerpt(&s.outcome, 120)),
                );
                if let Some(c) = s.confidence {
                    obj.insert("confidence".into(), json!(c));
                }
                if let Some(r) = s.revised_by {
                    obj.insert("revised_by".into(), json!(r));
                }
                if let Some(b) = &s.branch_id {
                    obj.insert("branch_id".into(), json!(b));
                }
                Value::Object(obj)
            })
            .collect()
    }

    /// Runtime introspection: the high-level state the model needs to know
    /// to decide whether the trace is durable, how big it is, and where it
    /// lives on disk. `verbose` folds in the formerly-separate `pinned` and
    /// `sessions` listings.
    pub fn status_snapshot(&self, verbose: bool) -> Value {
        let data_dir = self.config.persistence.data_dir.display().to_string();
        let mut obj = Map::new();
        obj.insert(
            "persistence_enabled".into(),
            json!(self.config.persistence.enabled),
        );
        obj.insert("data_dir".into(), json!(data_dir));
        obj.insert(
            "sessions_enabled".into(),
            json!(self.config.features.enable_sessions),
        );
        // Surface the configured default session_id so an operator can tell
        // at a glance whether DELIBERATE_AUTO_SESSION / DELIBERATE_DEFAULT_SESSION_ID
        // actually took effect in this process. Without this, a stale
        // env-var setup looks identical to a working one from the outside.
        obj.insert(
            "default_session_id".into(),
            json!(self.config.features.default_session_id),
        );
        obj.insert("active_session".into(), json!(self.active_session));
        obj.insert("sessions_count".into(), json!(self.sessions.len()));
        obj.insert("total_steps".into(), json!(self.history.steps.len()));
        obj.insert("branches_count".into(), json!(self.branches.len()));
        obj.insert(
            "pinned_count".into(),
            json!(
                self.history
                    .steps
                    .iter()
                    .filter(|s| s.pinned.unwrap_or(false))
                    .count()
            ),
        );
        obj.insert("completed".into(), json!(self.history.completed));
        obj.insert(
            "recent_steps_limit".into(),
            json!(self.config.system.recent_steps_limit),
        );
        obj.insert(
            "max_history_size".into(),
            json!(self.config.system.max_history_size),
        );
        obj.insert(
            "strict_mode".into(),
            json!(self.config.validation.strict_mode),
        );
        obj.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
        if verbose {
            obj.insert("pinned".into(), Value::Array(self.pinned_steps()));
            obj.insert("sessions".into(), Value::Array(self.sessions_snapshot()));
        }
        Value::Object(obj)
    }
}
