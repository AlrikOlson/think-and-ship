//! Project-wide step-number bookkeeping.
//!
//! Step numbers are unique across every session that belongs to the
//! same project. The engine's `step_numbers` set covers only the
//! currently-loaded session; the methods here extend the check to
//! cover every *other* session in the project — important when
//! process_step needs to refuse a number that's already taken in a
//! sibling session.
//!
//! The two free functions at the bottom (`renumber_project_for_uniqueness`
//! and `rewrite_history_step_numbers`) handle the one-time migration
//! from the pre-iteration-I per-session numbering scheme. They run
//! once at server startup; idempotent when numbers are already unique.

use std::collections::{HashMap, HashSet};

use crate::domain::{DeliberateHistory, DepEdge, SessionEntry};
use crate::persistence::Persistence;

use super::core::ReasoningServer;

impl ReasoningServer {
    /// Iterate over every session entry that belongs to the same project
    /// as this server *and* is not the currently-active session. The active
    /// session's numbers live in `self.step_numbers` — combine the two
    /// when you need project-wide coverage.
    pub(crate) fn other_project_sessions(&self) -> impl Iterator<Item = &SessionEntry> {
        let active_sid = self.active_session.as_deref();
        let project_id = self.project_id.as_str();
        self.sessions.iter().filter_map(move |(sid, entry)| {
            if Some(sid.as_str()) == active_sid {
                return None;
            }
            let belongs = entry
                .history
                .metadata
                .as_ref()
                .and_then(|m| m.project_id.as_deref())
                == Some(project_id);
            if !belongs {
                return None;
            }
            Some(entry)
        })
    }

    /// True when `n` is recorded in the active session **or** any other
    /// session in this project. With an empty project_id (analysis-only
    /// servers, single-history mode) only the active session is consulted.
    pub(crate) fn step_number_taken_in_project(&self, n: u32) -> bool {
        if self.step_numbers.contains(&n) {
            return true;
        }
        if self.project_id.is_empty() {
            return false;
        }
        for entry in self.other_project_sessions() {
            if entry.history.steps.iter().any(|s| s.step_number == n) {
                return true;
            }
        }
        false
    }

    /// Highest step_number across the project (active session + every
    /// other session whose project_id matches). Returns 0 when nothing
    /// is recorded.
    pub(crate) fn max_step_number_in_project(&self) -> u32 {
        let active_max = self.step_numbers.iter().copied().max().unwrap_or(0);
        if self.project_id.is_empty() {
            return active_max;
        }
        let cross_max = self
            .other_project_sessions()
            .flat_map(|entry| entry.history.steps.iter().map(|s| s.step_number))
            .max()
            .unwrap_or(0);
        active_max.max(cross_max)
    }

    /// Total step count across every session in this project (active + others).
    pub(crate) fn total_steps_in_project(&self) -> usize {
        let active = self.step_numbers.len();
        if self.project_id.is_empty() {
            return active;
        }
        active
            + self
                .other_project_sessions()
                .map(|entry| entry.history.steps.len())
                .sum::<usize>()
    }

    pub(crate) fn sorted_step_numbers(&self) -> String {
        let mut nums: Vec<u32> = self.step_numbers.iter().copied().collect();
        nums.sort_unstable();
        nums.iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Walk every history that belongs to `project_id`, sort them by
/// `created_at`, and reassign step numbers 1..N globally so the
/// project's step_number sequence is unique. Rewrites every reference
/// field (`revises_step`, `revised_by`, `branch_from`, `dependencies`)
/// using the new mapping so the trace stays self-consistent.
///
/// Idempotent: a no-op when the project's step_numbers are already unique.
pub(crate) fn renumber_project_for_uniqueness(
    history: &mut DeliberateHistory,
    sessions: &mut HashMap<String, SessionEntry>,
    project_id: &str,
    persistence: &Persistence,
) {
    if project_id.is_empty() {
        return;
    }

    let belongs = |h: &DeliberateHistory| -> bool {
        h.metadata
            .as_ref()
            .and_then(|m| m.project_id.as_deref())
            == Some(project_id)
    };

    // Members are ordered by `created_at`. `None` represents the default
    // (no-session-id) history; `Some(sid)` is a named session.
    let mut member_keys: Vec<Option<String>> = Vec::new();
    if belongs(history) {
        member_keys.push(None);
    }
    for (sid, entry) in sessions.iter() {
        if belongs(&entry.history) {
            member_keys.push(Some(sid.clone()));
        }
    }
    if member_keys.is_empty() {
        return;
    }
    member_keys.sort_by(|a, b| {
        let ts_a = match a {
            None => history.created_at.clone().unwrap_or_default(),
            Some(sid) => sessions
                .get(sid)
                .and_then(|e| e.history.created_at.clone())
                .unwrap_or_default(),
        };
        let ts_b = match b {
            None => history.created_at.clone().unwrap_or_default(),
            Some(sid) => sessions
                .get(sid)
                .and_then(|e| e.history.created_at.clone())
                .unwrap_or_default(),
        };
        ts_a.cmp(&ts_b).then_with(|| match (a, b) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => x.cmp(y),
        })
    });

    // Detect duplicates. Cheap pass: collect every step_number across the
    // project's members and compare to the unique-set size.
    let mut total_steps: usize = 0;
    let mut unique: HashSet<u32> = HashSet::new();
    for key in &member_keys {
        let h = match key {
            None => &*history,
            Some(sid) => match sessions.get(sid) {
                Some(e) => &e.history,
                None => continue,
            },
        };
        for step in &h.steps {
            total_steps += 1;
            unique.insert(step.step_number);
        }
    }
    if total_steps == unique.len() {
        return;
    }

    // Build (member_idx, step_idx) → new_number and (member_idx, old_number)
    // → new_number. Later occurrences of the same old number overwrite the
    // map entry — matches the runtime `step_index` "last write wins" rule
    // for branch steps that share a number with the main line.
    let mut new_for_position: HashMap<(usize, usize), u32> = HashMap::new();
    let mut new_for_old: HashMap<(usize, u32), u32> = HashMap::new();
    let mut next_num: u32 = 1;
    for (member_idx, key) in member_keys.iter().enumerate() {
        let h = match key {
            None => &*history,
            Some(sid) => match sessions.get(sid) {
                Some(e) => &e.history,
                None => continue,
            },
        };
        for (step_idx, step) in h.steps.iter().enumerate() {
            new_for_position.insert((member_idx, step_idx), next_num);
            new_for_old.insert((member_idx, step.step_number), next_num);
            next_num += 1;
        }
    }

    // Apply rewrites. Default first to satisfy the borrow checker
    // (`history` and `sessions` can't be borrowed mutably together via
    // the same iterator).
    for (member_idx, key) in member_keys.iter().enumerate() {
        match key {
            None => rewrite_history_step_numbers(
                history,
                member_idx,
                &new_for_position,
                &new_for_old,
            ),
            Some(sid) => {
                if let Some(entry) = sessions.get_mut(sid) {
                    rewrite_history_step_numbers(
                        &mut entry.history,
                        member_idx,
                        &new_for_position,
                        &new_for_old,
                    );
                }
            }
        }
    }

    // Persist back to disk so the renumber survives across restarts.
    // Skip when persistence is off (in-memory test runs).
    if persistence.enabled() {
        for key in &member_keys {
            match key {
                None => persistence.save_default(history),
                Some(sid) => {
                    if let Some(entry) = sessions.get(sid) {
                        persistence.save_session(sid, &entry.history);
                    }
                }
            }
        }
    }

    eprintln!(
        "📐 Renumbered {} steps across {} session(s) in project '{project_id}' for project-wide step_number uniqueness",
        total_steps,
        member_keys.len()
    );
}

/// Rewrite every step in `history` to use its new global step_number,
/// then rewrite reference fields using `new_for_old` (keyed by the OLD
/// number). Numbers that can't be resolved (dangling references to
/// steps that no longer exist) are set to `None` on Option fields and
/// left untouched on `DepEdge` so the user can still see the original
/// intent.
fn rewrite_history_step_numbers(
    history: &mut DeliberateHistory,
    member_idx: usize,
    new_for_position: &HashMap<(usize, usize), u32>,
    new_for_old: &HashMap<(usize, u32), u32>,
) {
    // Pass 1: rewrite each step's own step_number using its position.
    for (step_idx, step) in history.steps.iter_mut().enumerate() {
        if let Some(&new) = new_for_position.get(&(member_idx, step_idx)) {
            step.step_number = new;
        }
    }
    // Pass 2: rewrite reference fields. revises_step/revised_by/branch_from
    // and every DepEdge target are looked up by their ORIGINAL number
    // (which is still what they hold — we never overwrote those, only
    // step_number).
    for step in history.steps.iter_mut() {
        if let Some(old) = step.revises_step {
            step.revises_step = new_for_old.get(&(member_idx, old)).copied();
        }
        if let Some(old) = step.revised_by {
            step.revised_by = new_for_old.get(&(member_idx, old)).copied();
        }
        if let Some(old) = step.branch_from {
            step.branch_from = new_for_old.get(&(member_idx, old)).copied();
        }
        if let Some(deps) = step.dependencies.as_mut() {
            for dep in deps.iter_mut() {
                match dep {
                    DepEdge::Bare(n) => {
                        if let Some(&new) = new_for_old.get(&(member_idx, *n)) {
                            *n = new;
                        }
                    }
                    DepEdge::Tagged { step: n, .. } => {
                        if let Some(&new) = new_for_old.get(&(member_idx, *n)) {
                            *n = new;
                        }
                    }
                }
            }
        }
    }
    // Pass 3: rewrite the persisted branches array, if present. Live
    // servers rebuild branches from history.steps after load, but this
    // keeps disk self-consistent for external viewers that read the
    // field directly.
    if let Some(branches) = history.branches.as_mut() {
        for branch in branches.iter_mut() {
            if let Some(&new) = new_for_old.get(&(member_idx, branch.from_step)) {
                branch.from_step = new;
            }
            if let Some(old) = branch.merged_into {
                branch.merged_into = new_for_old.get(&(member_idx, old)).copied();
            }
        }
    }
}
