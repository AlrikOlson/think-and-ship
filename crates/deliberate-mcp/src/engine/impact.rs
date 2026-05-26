//! Dependency-graph walks — `impact_of` and its helpers.
//!
//! `impact_of` is the public entry point that powers the
//! `deliberate_step_impact` tool: it answers "if I revise this step,
//! what re-breaks?" by walking the dep graph upstream and downstream,
//! partitioning direct dependents by relation label, and surfacing the
//! revision chain plus any branches that fork off the target.
//!
//! All walks are capped at 256 nodes so a malicious or cyclical trace
//! can't pin a CPU.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::domain::{BranchStatus, DepEdge};

use super::core::ReasoningServer;

impl ReasoningServer {
    /// Trace the dependency and revision graph around a step. Returns
    /// upstream (what this step builds on, transitively), downstream (what
    /// depends on it, direct + transitive, partitioned by relation label
    /// when present), the chain of revisions through this step, and any
    /// branches that fork off it (with their merged_into pointer when set).
    pub fn impact_of(&self, step_number: u32) -> Result<Value, String> {
        let Some(target) = self.step_by_number(step_number) else {
            return Err(format!(
                "No step #{step_number} found in the current trace."
            ));
        };

        // Upstream: walk `dependencies` backwards. Capped at 256 nodes so a
        // malicious/circular trace can't pin a CPU.
        let upstream = self.walk_deps(step_number, 256);
        let downstream = self.walk_dependents(step_number, 256);
        let revision_chain = self.revision_chain_through(step_number);

        // Partition direct dependents by the relation label they used on
        // their edge to this step. Steps with no relation land in
        // `unlabeled`.
        let mut supported_by: Vec<u32> = Vec::new();
        let mut refuted_by: Vec<u32> = Vec::new();
        let mut depended_on_by: Vec<u32> = Vec::new();
        let mut unlabeled: Vec<u32> = Vec::new();
        for s in self.all_steps() {
            let Some(deps) = &s.dependencies else {
                continue;
            };
            for edge in deps {
                if edge.step() == step_number {
                    match edge.relation() {
                        Some("supports") => supported_by.push(s.step_number),
                        Some("refutes") => refuted_by.push(s.step_number),
                        Some("depends_on") => depended_on_by.push(s.step_number),
                        Some(_) => unlabeled.push(s.step_number),
                        None => unlabeled.push(s.step_number),
                    }
                    break;
                }
            }
        }
        for v in [
            &mut supported_by,
            &mut refuted_by,
            &mut depended_on_by,
            &mut unlabeled,
        ] {
            v.sort_unstable();
            v.dedup();
        }

        // Upstream direct deps: emit as step numbers (the structured edge is
        // already on the target step; consumers can re-fetch via deliberate_step).
        let upstream_direct: Vec<u32> = target
            .dependencies
            .as_ref()
            .map(|deps| deps.iter().map(DepEdge::step).collect())
            .unwrap_or_default();

        let branches_from: Vec<Value> = self
            .branches
            .values()
            .filter(|b| b.from_step == step_number)
            .map(|b| {
                let mut obj = serde_json::Map::new();
                obj.insert("id".into(), json!(b.id));
                obj.insert("name".into(), json!(b.name));
                obj.insert(
                    "status".into(),
                    json!(match b.status {
                        BranchStatus::Active => "active",
                        BranchStatus::Merged => "merged",
                        BranchStatus::Abandoned => "abandoned",
                    }),
                );
                obj.insert("step_count".into(), json!(b.steps.len()));
                if let Some(into) = b.merged_into {
                    obj.insert("merged_into".into(), json!(into));
                }
                Value::Object(obj)
            })
            .collect();

        Ok(json!({
            "step_number": step_number,
            "purpose": target.purpose,
            "confidence": target.confidence,
            "revised_by": target.revised_by,
            "revises_step": target.revises_step,
            "branch_id": target.branch_id,
            "upstream": {
                "direct": upstream_direct,
                "transitive": upstream,
            },
            "downstream": {
                "direct": self.direct_dependents(step_number),
                "transitive": downstream,
                "by_relation": {
                    "supports": supported_by,
                    "refutes": refuted_by,
                    "depends_on": depended_on_by,
                    "unlabeled": unlabeled,
                },
            },
            "revision_chain": revision_chain,
            "branches_from": branches_from,
        }))
    }

    pub(crate) fn walk_deps(&self, start: u32, cap: usize) -> Vec<u32> {
        let mut seen: HashSet<u32> = HashSet::new();
        let mut stack: Vec<u32> = vec![start];
        let mut out: Vec<u32> = Vec::new();
        while let Some(n) = stack.pop() {
            if seen.len() >= cap {
                break;
            }
            if let Some(step) = self.step_by_number(n) {
                if let Some(deps) = step.dependencies {
                    for edge in deps {
                        let d = edge.step();
                        if seen.insert(d) {
                            out.push(d);
                            stack.push(d);
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    pub(crate) fn direct_dependents(&self, target: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .all_steps()
            .filter(|s| {
                s.dependencies
                    .as_ref()
                    .is_some_and(|d| d.iter().any(|e| e.step() == target))
            })
            .map(|s| s.step_number)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Step numbers of every step whose edge to `target` carries
    /// `relation: "refutes"`. Used by `compute_step_warnings` to flag
    /// building on something that's been refuted elsewhere.
    pub(crate) fn refuters_of(&self, target: u32, exclude: u32) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .all_steps()
            .filter(|s| s.step_number != exclude)
            .filter(|s| {
                s.dependencies.as_ref().is_some_and(|d| {
                    d.iter()
                        .any(|e| e.step() == target && e.relation() == Some("refutes"))
                })
            })
            .map(|s| s.step_number)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub(crate) fn walk_dependents(&self, start: u32, cap: usize) -> Vec<u32> {
        let mut seen: HashSet<u32> = HashSet::new();
        let mut stack: Vec<u32> = vec![start];
        let mut out: Vec<u32> = Vec::new();
        while let Some(n) = stack.pop() {
            if seen.len() >= cap {
                break;
            }
            for d in self.direct_dependents(n) {
                if seen.insert(d) {
                    out.push(d);
                    stack.push(d);
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Build the revision chain that runs through `step_number`: walk back
    /// through `revises_step` to the original, then forward through
    /// `revised_by` to the latest. Returns step numbers in chronological
    /// order.
    pub(crate) fn revision_chain_through(&self, step_number: u32) -> Vec<u32> {
        let Some(start) = self.step_by_number(step_number) else {
            return Vec::new();
        };

        let mut origin = step_number;
        let mut cur = start.clone();
        let mut guard = 0u32;
        while let Some(prev) = cur.revises_step {
            if guard >= 256 {
                break;
            }
            guard += 1;
            origin = prev;
            match self.step_by_number(prev) {
                Some(p) => cur = p,
                None => break,
            }
        }

        let mut chain: Vec<u32> = vec![origin];
        let mut cur_n = origin;
        let mut guard = 0u32;
        loop {
            if guard >= 256 {
                break;
            }
            guard += 1;
            let Some(s) = self.step_by_number(cur_n) else {
                break;
            };
            let Some(next) = s.revised_by else {
                break;
            };
            chain.push(next);
            cur_n = next;
        }

        if chain.len() <= 1 { Vec::new() } else { chain }
    }
}
