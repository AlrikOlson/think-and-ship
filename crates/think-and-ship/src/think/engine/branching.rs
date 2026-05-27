//! Branch creation — opening a fork, picking an id, depth bookkeeping.
//!
//! Branch *lifecycle* (active → merged → abandoned) lives in
//! [`super::mutations::ReasoningServer::set_branch_status`]; this file
//! owns the "create or extend a branch" side. The
//! `branch_depth_cache` makes repeated depth lookups O(1) once a step
//! is on a branch — invalidated only when a new branch is registered.

use chrono::Utc;

use crate::think::domain::{Branch, BranchStatus, DeliberateStep};

use super::core::ReasoningServer;

impl ReasoningServer {
    pub fn calculate_branch_depth(&mut self, step_number: u32) -> u32 {
        if let Some(&v) = self.branch_depth_cache.get(&step_number) {
            return v;
        }
        let depth = if let Some(branch_id) = self.step_to_branch.get(&step_number) {
            self.branches
                .get(branch_id)
                .map(|b| b.depth + 1)
                .unwrap_or(1)
        } else {
            1
        };
        self.branch_depth_cache.insert(step_number, depth);
        depth
    }

    pub(crate) fn invalidate_branch_depth_cache(&mut self) {
        self.branch_depth_cache.clear();
    }

    pub(crate) fn next_branch_id(&mut self) -> String {
        self.branches_seq += 1;
        format!("branch-{}-{:04}", Self::now_ms(), self.branches_seq)
    }

    /// Resolve or create the branch this step belongs to. No-op when
    /// `branch_from` is unset or branching is disabled in config.
    ///
    /// On first use of a branch_id we register the [`Branch`] entry,
    /// bump metadata counters, and invalidate the depth cache. Repeated
    /// calls with the same branch_id simply push the step onto the
    /// existing branch.
    pub(crate) fn handle_branching(&mut self, step: &mut DeliberateStep) -> Result<(), String> {
        let Some(from) = step.branch_from else {
            return Ok(());
        };
        if !self.config.features.enable_branching {
            return Ok(());
        }
        if from == step.step_number {
            let msg = format!("Cannot branch from self (step {})", step.step_number);
            eprintln!("⚠️ {msg}");
            return Err(msg);
        }
        if !self.step_numbers.contains(&from) {
            let available = self.sorted_step_numbers();
            let msg = format!(
                "Cannot branch from step {from}: step does not exist. Available steps: {}",
                if available.is_empty() {
                    "none".into()
                } else {
                    available
                }
            );
            eprintln!("⚠️ {msg}");
            return Err(msg);
        }

        let branch_id = step
            .branch_id
            .clone()
            .unwrap_or_else(|| self.next_branch_id());
        let branch_name = step
            .branch_name
            .clone()
            .unwrap_or_else(|| format!("Alternative {}", self.branches.len() + 1));

        if !self.branches.contains_key(&branch_id) {
            let depth = self.calculate_branch_depth(from);
            if depth > self.config.system.max_branch_depth {
                let msg = format!(
                    "Branch depth {depth} exceeds maximum {}. Max allowed: {}",
                    self.config.system.max_branch_depth, self.config.system.max_branch_depth
                );
                eprintln!("⚠️ {msg}");
                return Err(msg);
            }
            let branch = Branch {
                id: branch_id.clone(),
                name: branch_name.clone(),
                from_step: from,
                steps: Vec::new(),
                status: BranchStatus::Active,
                created_at: Utc::now().to_rfc3339(),
                depth,
                merged_into: None,
            };
            self.branches.insert(branch_id.clone(), branch);
            self.invalidate_branch_depth_cache();
            if let Some(meta) = self.history.metadata.as_mut() {
                meta.branches_created = Some(meta.branches_created.unwrap_or(0) + 1);
            }
            eprintln!("🌿 Created branch \"{branch_name}\" from step {from} (depth: {depth})");
        }

        step.branch_id = Some(branch_id.clone());
        if let Some(branch) = self.branches.get_mut(&branch_id) {
            branch.steps.push(step.clone());
        }
        self.step_to_branch.insert(step.step_number, branch_id);
        Ok(())
    }
}
