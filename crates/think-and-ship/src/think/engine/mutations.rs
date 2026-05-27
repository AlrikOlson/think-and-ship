//! State mutations that aren't full step recording.
//!
//! `process_step` (the big one) stays in `core` for now; this file
//! collects the smaller mutators: revise the running estimate, pin or
//! unpin a step, set a branch's lifecycle status, and wipe everything.

use chrono::Utc;
use std::time::Instant;

use crate::think::broadcast::BroadcastFrame;
use crate::think::domain::BranchStatus;

use super::core::ReasoningServer;

impl ReasoningServer {
    /// Update the `estimated_total` on the most recently recorded step in
    /// place — no new step is appended, no validation rules re-fire.
    pub fn revise_estimate(&mut self, new_estimate: u32) -> Result<(u32, u32), String> {
        if new_estimate == 0 {
            return Err("estimated_total must be >= 1".into());
        }
        let len = self.history.steps.len();
        let Some(last) = self.history.steps.last_mut() else {
            return Err(
                "No steps recorded yet — call `deliberate` at least once before revising the estimate"
                    .into(),
            );
        };
        let previous = last.estimated_total;
        last.estimated_total = new_estimate;
        let last_n = last.step_number;
        self.history.updated_at = Some(Utc::now().to_rfc3339());
        eprintln!(
            "📐 estimated_total revised: {previous} → {new_estimate} (last step is #{last_n}, {len} total)"
        );
        self.persist_active();
        if let Some(b) = &self.broadcaster {
            b.emit(BroadcastFrame::EstimateRevised {
                session_id: self.active_session.clone(),
                old: previous,
                new: new_estimate,
                reason: None,
            });
        }
        Ok((previous, new_estimate))
    }

    /// Toggle the `pinned` flag on a step. Pinned steps are surfaced in
    /// `recent_steps` even after they fall out of the chronological window.
    pub fn pin_step(&mut self, step_number: u32, pinned: bool) -> Result<bool, String> {
        let Some(&idx) = self.step_index.get(&step_number) else {
            return Err(format!(
                "No step #{step_number} found in the current trace."
            ));
        };
        let previous = self.history.steps[idx].pinned.unwrap_or(false);
        self.history.steps[idx].pinned = if pinned { Some(true) } else { None };
        self.history.updated_at = Some(Utc::now().to_rfc3339());
        self.persist_active();
        if let Some(b) = &self.broadcaster {
            b.emit(BroadcastFrame::PinChanged {
                session_id: self.active_session.clone(),
                step_number,
                pinned,
            });
        }
        Ok(previous)
    }

    /// Set a branch's status. Returns the previous and new status strings.
    /// Optional `merged_into` records the step that synthesized this branch
    /// back into the main reasoning line; ignored when status != merged,
    /// and the prior value is cleared when status moves away from merged.
    pub fn set_branch_status(
        &mut self,
        branch_id: &str,
        status: &str,
        merged_into: Option<u32>,
    ) -> Result<(&'static str, &'static str), String> {
        let new_status = match status.trim().to_ascii_lowercase().as_str() {
            "active" => BranchStatus::Active,
            "merged" => BranchStatus::Merged,
            "abandoned" => BranchStatus::Abandoned,
            other => {
                return Err(format!(
                    "Unknown status \"{other}\". Use one of: active, merged, abandoned."
                ));
            }
        };
        if let Some(step_n) = merged_into {
            if !self.step_numbers.contains(&step_n) {
                return Err(format!(
                    "merged_into refers to step #{step_n}, which does not exist."
                ));
            }
        }
        let Some(branch) = self.branches.get_mut(branch_id) else {
            let known: Vec<&String> = self.branches.keys().collect();
            return Err(format!(
                "Unknown branch_id \"{branch_id}\". Known: {known:?}"
            ));
        };
        let prev = match branch.status {
            BranchStatus::Active => "active",
            BranchStatus::Merged => "merged",
            BranchStatus::Abandoned => "abandoned",
        };
        let new_str = match new_status {
            BranchStatus::Active => "active",
            BranchStatus::Merged => "merged",
            BranchStatus::Abandoned => "abandoned",
        };
        branch.status = new_status;
        // Track the merged_into pointer with the status: set on merge, clear
        // on a move away from merged so stale pointers don't survive.
        if matches!(new_status, BranchStatus::Merged) {
            if let Some(into) = merged_into {
                branch.merged_into = Some(into);
            }
        } else {
            branch.merged_into = None;
        }
        let emit_merged_into = branch.merged_into;
        let branch_id_owned = branch_id.to_string();
        self.history.updated_at = Some(Utc::now().to_rfc3339());
        self.persist_active();
        if let Some(b) = &self.broadcaster {
            b.emit(BroadcastFrame::BranchStatusChanged {
                session_id: self.active_session.clone(),
                branch_id: branch_id_owned,
                status: new_status,
                merged_into: emit_merged_into,
            });
        }
        Ok((prev, new_str))
    }

    /// Wipe the trace: steps, branches, sessions, active-session pointer,
    /// and persisted files. Destructive — there is no undo.
    pub fn clear_history(&mut self) {
        self.history = Self::new_history();
        self.branches.clear();
        self.sessions.clear();
        self.step_index.clear();
        self.step_numbers.clear();
        self.tools_used.clear();
        self.step_to_branch.clear();
        self.branch_depth_cache.clear();
        self.steps_since_cleanup = 0;
        self.start_time = Instant::now();
        self.active_session = None;
        // Clear disk too — otherwise a restart would resurrect the wiped trace.
        self.persistence.delete_all();
        if let Some(b) = &self.broadcaster {
            b.emit(BroadcastFrame::Cleared);
        }
        eprintln!("🔄 Deliberation history cleared");
    }
}
