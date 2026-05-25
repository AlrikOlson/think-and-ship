//! Step revisions — back-pointer bookkeeping when a new step revises
//! an earlier one.
//!
//! `process_step` calls this when the incoming step carries
//! `revises_step`. We refuse forward references (you can only revise
//! earlier steps), mark the original step's `revised_by` pointer, and
//! bump the per-history `revisions_count` metadata counter. A missing
//! `revises_step` target is a soft warning, not an error — the new
//! step still gets recorded, just without the back-pointer.

use crate::domain::DeliberateStep;

use super::core::ReasoningServer;

impl ReasoningServer {
    pub(crate) fn handle_revision(&mut self, step: &DeliberateStep) -> Result<(), String> {
        let Some(revises) = step.revises_step else {
            return Ok(());
        };
        if !self.config.features.enable_revisions {
            return Ok(());
        }
        if revises >= step.step_number {
            let msg = format!(
                "Cannot revise step {revises} from step {}: can only revise earlier steps",
                step.step_number
            );
            eprintln!("⚠️ {msg}");
            return Err(msg);
        }

        if let Some(&idx) = self.step_index.get(&revises) {
            self.history.steps[idx].revised_by = Some(step.step_number);
            eprintln!(
                "📝 Revising step {revises}: {}",
                step.revision_reason
                    .as_deref()
                    .unwrap_or("No reason provided")
            );
        } else {
            let available = self.sorted_step_numbers();
            eprintln!(
                "⚠️ Warning: Cannot find step {revises} to revise. Available steps: {}",
                if available.is_empty() {
                    "none".into()
                } else {
                    available
                }
            );
        }

        if let Some(meta) = self.history.metadata.as_mut() {
            meta.revisions_count = Some(meta.revisions_count.unwrap_or(0) + 1);
        }

        Ok(())
    }
}
