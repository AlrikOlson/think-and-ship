//! Step/trace queries — point lookups and substring search.
//!
//! Read-only methods that find a single step (`step_by_number`,
//! `latest_revision_of`), iterate every recorded step (`all_steps`), or
//! produce structured search hits (`search_steps`).

use serde_json::{Value, json};

use crate::domain::{DeliberateStep, NextAction};
use crate::util::text::excerpt_around;

use super::core::ReasoningServer;

impl ReasoningServer {
    /// Lookup a step by its step number, including any in branches.
    pub fn step_by_number(&self, n: u32) -> Option<DeliberateStep> {
        if let Some(&i) = self.step_index.get(&n) {
            return self.history.steps.get(i).cloned();
        }
        for branch in self.branches.values() {
            if let Some(s) = branch.steps.iter().find(|s| s.step_number == n) {
                return Some(s.clone());
            }
        }
        None
    }

    /// Walk the `revised_by` chain forward from `n` and return the live
    /// (latest-revision) step. Returns the same step when nothing has revised
    /// it, and `None` when `n` doesn't exist. Capped at 256 hops so a
    /// cyclical chain can't pin a CPU.
    pub fn latest_revision_of(&self, n: u32) -> Option<DeliberateStep> {
        let mut cur = self.step_by_number(n)?;
        let mut guard = 0u32;
        while let Some(next) = cur.revised_by {
            if guard >= 256 || next == cur.step_number {
                break;
            }
            guard += 1;
            match self.step_by_number(next) {
                Some(s) => cur = s,
                None => break,
            }
        }
        Some(cur)
    }

    /// Iterate every recorded step. Branch steps are also stored in
    /// `self.history.steps` (push happens unconditionally in `process_step`),
    /// so iterating main is sufficient — chaining branches in would double-count.
    pub(crate) fn all_steps(&self) -> impl Iterator<Item = &DeliberateStep> {
        self.history.steps.iter()
    }

    /// Case-insensitive substring search across the text-bearing fields of
    /// every recorded step (main + branches). Returns at most `limit` matches
    /// in step-number order, each carrying a short excerpt and the field
    /// that matched.
    pub fn search_steps(&self, query: &str, limit: usize) -> Vec<Value> {
        let needle = query.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut matches: Vec<(u32, Value)> = Vec::new();
        for step in self.all_steps() {
            let next_action_text = match &step.next_action {
                NextAction::Text(t) => t.clone(),
                NextAction::Structured(a) => {
                    let mut s = a.action.clone();
                    if let Some(tool) = &a.tool {
                        s.push_str(&format!(" [{tool}]"));
                    }
                    s
                }
            };

            let exec_ref = step.execution_ref.as_deref().unwrap_or("");

            let fields: [(&str, &str); 7] = [
                ("thought", &step.thought),
                ("outcome", &step.outcome),
                ("context", &step.context),
                ("purpose", &step.purpose),
                ("rationale", &step.rationale),
                ("next_action", &next_action_text),
                ("execution_ref", exec_ref),
            ];

            for (name, val) in &fields {
                let lower = val.to_ascii_lowercase();
                if let Some(pos) = lower.find(&needle) {
                    let excerpt = excerpt_around(val, pos, needle.len(), 60);
                    matches.push((
                        step.step_number,
                        json!({
                            "step_number": step.step_number,
                            "purpose": step.purpose,
                            "matched_field": name,
                            "excerpt": excerpt,
                            "branch_id": step.branch_id,
                            "confidence": step.confidence,
                            "revised_by": step.revised_by,
                        }),
                    ));
                    break;
                }
            }
        }

        matches.sort_by_key(|(n, _)| *n);
        matches.into_iter().map(|(_, v)| v).take(limit).collect()
    }
}
