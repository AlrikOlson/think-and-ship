//! Session lifecycle — switch the active session, expire idle ones, the
//! tiny clock helpers (`now_ms`, `session_timeout_ms`) that both rely
//! on.
//!
//! Sessions only exist when `config.features.enable_sessions` is true.
//! [`Self::switch_to_session`] saves the current history into the
//! previously-active session entry before loading the requested one, so
//! the indexes (`step_index`, `step_numbers`, `step_to_branch`,
//! `tools_used`, `branch_depth_cache`) get rebuilt against the
//! freshly-loaded history.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::think::constants::SESSION_CLEANUP_INTERVAL;
use crate::think::domain::SessionEntry;

use super::core::ReasoningServer;

impl ReasoningServer {
    pub(crate) fn session_timeout_ms(&self) -> u128 {
        u128::from(self.config.system.session_timeout) * 60 * 1000
    }

    pub(crate) fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// Save current `self.history` into the active session entry (if any)
    /// and load `session_id`'s history. Rebuilds the per-history indexes.
    pub(crate) fn switch_to_session(&mut self, session_id: &str) {
        if self.active_session.as_deref() == Some(session_id) {
            // Same session — just touch the access timestamp.
            if let Some(entry) = self.sessions.get_mut(session_id) {
                entry.last_accessed = Self::now_ms();
            }
            return;
        }

        if let Some(prev) = self.active_session.clone() {
            let now = Self::now_ms();
            if let Some(entry) = self.sessions.get_mut(&prev) {
                entry.history = self.history.clone();
                entry.last_accessed = now;
            }
        }

        let now = Self::now_ms();
        let entry = self
            .sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionEntry {
                history: Self::new_history(),
                last_accessed: now,
            });
        entry.last_accessed = now;
        // Cross-project guard: refuse to load a session whose stamped
        // project id disagrees with this server's project. Prevents
        // silent merges (e.g. mounting a session file from one repo
        // into a server running in another). The check is best-effort
        // — empty `self.project_id` (analysis-only servers) skips it,
        // and legacy sessions without a project_id stamp are accepted
        // (the next step write will stamp them).
        if !self.project_id.is_empty() {
            if let Some(stamped) = entry
                .history
                .metadata
                .as_ref()
                .and_then(|m| m.project_id.as_deref())
                && stamped != self.project_id
            {
                eprintln!(
                    "⚠️ Refusing to switch into session '{session_id}': stamped \
                     project_id={stamped:?} differs from server's project_id={:?}. \
                     This usually means the sessions directory contains a file \
                     from another project. Leaving previous session active.",
                    self.project_id
                );
                return;
            }
        }
        self.history = entry.history.clone();
        self.active_session = Some(session_id.to_string());

        self.step_index.clear();
        self.step_numbers.clear();
        self.tools_used.clear();
        self.step_to_branch.clear();
        self.branch_depth_cache.clear();
        for (idx, existing) in self.history.steps.iter().enumerate() {
            self.step_index.insert(existing.step_number, idx);
            self.step_numbers.insert(existing.step_number);
            if let Some(bid) = &existing.branch_id {
                self.step_to_branch
                    .insert(existing.step_number, bid.clone());
            }
        }
        if let Some(meta) = &self.history.metadata {
            if let Some(tools) = &meta.tools_used {
                for t in tools {
                    self.tools_used.insert(t.clone());
                }
            }
        }
    }

    /// Drop session entries that haven't been accessed within the
    /// configured timeout window. Only runs every
    /// `SESSION_CLEANUP_INTERVAL` process-step calls unless `force` is
    /// set; cheap no-op when sessions are disabled.
    pub fn cleanup_expired_sessions(&mut self, force: bool) {
        if !self.config.features.enable_sessions {
            return;
        }
        self.steps_since_cleanup += 1;
        if !force && self.steps_since_cleanup < SESSION_CLEANUP_INTERVAL {
            return;
        }
        self.steps_since_cleanup = 0;

        let now = Self::now_ms();
        let timeout = self.session_timeout_ms();
        let expired: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|(id, entry)| {
                if now.saturating_sub(entry.last_accessed) > timeout {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        for id in expired {
            self.sessions.remove(&id);
            eprintln!("🗑️ Session {id} expired and removed");
        }
    }
}
