//! Core reasoning-step engine. Owns the `ReasoningServer` struct and its sibling impl blocks.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use chrono::Utc;

use crate::think::broadcast::Broadcaster;
use crate::think::config::{DeliberateConfig, resolve_project_id};
use crate::think::domain::{
    Branch, BranchStatus, DeliberateHistory, DeliberateStep, HistoryMetadata, NextAction,
    SessionEntry,
};
use crate::think::formatter::Formatter;
use crate::think::persistence::Persistence;

#[derive(Debug, Clone)]
pub struct ProcessOk {
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ProcessErr {
    pub text: String,
}

pub type ProcessResult = Result<ProcessOk, ProcessErr>;

pub struct ReasoningServer {
    // Fields are `pub(crate)` so sibling modules under `engine::*` can
    // share access without bouncing through accessors. Anything outside
    // this crate still goes through the `config()` / `history()` /
    // `branches()` / `sessions()` accessors.
    pub(crate) history: DeliberateHistory,
    pub(crate) config: DeliberateConfig,
    pub(crate) formatter: Formatter,
    pub(crate) persistence: Persistence,
    pub(crate) start_time: Instant,
    pub(crate) branches: HashMap<String, Branch>,
    pub(crate) sessions: HashMap<String, SessionEntry>,
    pub(crate) step_index: HashMap<u32, usize>,
    pub(crate) step_numbers: HashSet<u32>,
    pub(crate) tools_used: HashSet<String>,
    pub(crate) step_to_branch: HashMap<u32, String>,
    pub(crate) branch_depth_cache: HashMap<u32, u32>,
    pub(crate) steps_since_cleanup: u32,
    pub(crate) branches_seq: u64,
    /// When sessions are enabled, the session whose history is currently
    /// loaded into `self.history`. `None` means we're on the default
    /// non-session history.
    pub(crate) active_session: Option<String>,
    /// Optional NDJSON-over-Unix-socket fan-out. Spawned only when
    /// `config.broadcast.path` is set; absent (or failed-to-spawn) means
    /// the server runs unobserved. Calls are fire-and-forget.
    pub(crate) broadcaster: Option<Broadcaster>,
    /// Canonical working directory captured once at server start.
    /// Stamped on every recorded step's `cwd` field so the project
    /// root travels with the data — no more "where did this step come
    /// from?" archaeology when sessions span multiple projects.
    pub(crate) cwd: Option<String>,
    /// Project id resolved once at startup (`<basename>-<6hex>` or a
    /// `DELIBERATE_PROJECT_NAME` override). Used to namespace every
    /// caller-supplied session id and stamped into each session's
    /// metadata so the viewer can group without parsing.
    pub(crate) project_id: String,
    /// Optional git-native trace sink. When set (SyncTarget::RepoGit + the
    /// process runs inside a repo), every recorded step is mirrored as an
    /// Agent Trace JSONL record into `.think-and-ship/` and the session is
    /// committed on close. `None` = the default Local behaviour. Writes are
    /// fire-and-forget — a sink error never fails `process_step`.
    pub(crate) repo_sink: Option<crate::infra::RepoSink>,
    /// Whether mirrored records are `shared` (committed `sessions/`) vs
    /// `local` (gitignored). Default `false`. Only meaningful with `repo_sink`.
    pub(crate) repo_shared: bool,
}

impl ReasoningServer {
    /// Attach an externally-spawned broadcaster, overriding whatever
    /// `new()` set up from `config.broadcast.path`. Use when one
    /// process serves both tool families and they share a single
    /// underlying socket.
    pub fn with_broadcaster(mut self, broadcaster: Broadcaster) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Attach a git-native trace sink so recorded steps are mirrored into the
    /// repo's `.think-and-ship/` as Agent Trace JSONL. `shared` selects the
    /// committed `sessions/` partition (`true`) vs the gitignored `local/`
    /// partition (`false`). Wired by `cli::build_unified` when
    /// `THINK_AND_SHIP_SYNC_TARGET=repo-git` and the process is inside a repo.
    pub fn with_repo_sink(mut self, sink: crate::infra::RepoSink, shared: bool) -> Self {
        self.repo_sink = Some(sink);
        self.repo_shared = shared;
        self
    }

    pub fn new(config: DeliberateConfig) -> Self {
        let formatter = Formatter::new(config.display.color_output);
        let persistence = Persistence::new(&config.persistence);

        // Rehydrate from disk if persistence is enabled. The default history
        // takes precedence over a freshly constructed empty one; named
        // sessions populate the sessions map.
        let mut history = persistence.load_default().unwrap_or_else(Self::new_history);
        let persisted_sessions = persistence.load_sessions();

        let mut sessions: HashMap<String, SessionEntry> = persisted_sessions
            .into_iter()
            .map(|(id, hist)| {
                (
                    id,
                    SessionEntry {
                        history: hist,
                        last_accessed: Self::now_ms(),
                    },
                )
            })
            .collect();

        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.canonicalize().unwrap_or(p))
            .map(|p| p.display().to_string());
        let project_id = resolve_project_id();

        // Step numbers must be unique across every session that belongs to
        // this project. Old persistence layouts (per-session numbering) had
        // step #1 in every session — a stitched cross-session view would
        // show duplicates. Walk this project's sessions in created_at order
        // and reassign 1..N globally, rewriting every reference field so
        // revises_step / branch_from / dependencies stay correct.
        super::numbering::renumber_project_for_uniqueness(
            &mut history,
            &mut sessions,
            &project_id,
            &persistence,
        );

        let mut step_index: HashMap<u32, usize> = HashMap::new();
        let mut step_numbers: HashSet<u32> = HashSet::new();
        let mut step_to_branch: HashMap<u32, String> = HashMap::new();
        let mut tools_used_set: HashSet<String> = HashSet::new();
        let mut branches: HashMap<String, Branch> = HashMap::new();

        for (idx, step) in history.steps.iter().enumerate() {
            step_index.insert(step.step_number, idx);
            step_numbers.insert(step.step_number);
            if let Some(bid) = &step.branch_id {
                step_to_branch.insert(step.step_number, bid.clone());
            }
        }
        if let Some(meta) = &history.metadata {
            if let Some(tools) = &meta.tools_used {
                for t in tools {
                    tools_used_set.insert(t.clone());
                }
            }
        }
        // Reconstruct branches from persisted history steps: every step with
        // a `branch_id` belongs to that branch. We rebuild Branch entries so
        // depth/from_step/status are recovered.
        for step in &history.steps {
            let (Some(bid), Some(from)) = (&step.branch_id, step.branch_from) else {
                continue;
            };
            let entry = branches.entry(bid.clone()).or_insert_with(|| Branch {
                id: bid.clone(),
                name: step.branch_name.clone().unwrap_or_else(|| bid.clone()),
                from_step: from,
                steps: Vec::new(),
                status: BranchStatus::Active,
                created_at: step.timestamp.clone().unwrap_or_default(),
                depth: 1,
                merged_into: None,
            });
            entry.steps.push(step.clone());
        }

        // Ensure metadata reflects loaded state.
        if let Some(meta) = history.metadata.as_mut() {
            meta.tools_used = Some({
                let mut v: Vec<String> = tools_used_set.iter().cloned().collect();
                v.sort();
                v
            });
        }

        let broadcaster = config.broadcast.path.clone().and_then(Broadcaster::spawn);

        Self {
            history,
            config,
            formatter,
            persistence,
            start_time: Instant::now(),
            branches,
            sessions,
            step_index,
            step_numbers,
            tools_used: tools_used_set,
            step_to_branch,
            branch_depth_cache: HashMap::new(),
            steps_since_cleanup: 0,
            branches_seq: 0,
            active_session: None,
            broadcaster,
            cwd,
            project_id,
            repo_sink: None,
            repo_shared: false,
        }
    }

    /// Build a read-only view of a history loaded from elsewhere — e.g.
    /// the GUI viewer parses a session file from disk and wants to call
    /// `impact_of` / `checkpoint_snapshot` / `recent_steps_rollup` on it.
    /// No persistence handle, no broadcaster, no disk I/O. The returned
    /// server's mutating methods still work but they only touch memory.
    pub fn for_analysis(history: DeliberateHistory, branches: HashMap<String, Branch>) -> Self {
        let config = DeliberateConfig::default();

        let mut step_index: HashMap<u32, usize> = HashMap::new();
        let mut step_numbers: HashSet<u32> = HashSet::new();
        let mut step_to_branch: HashMap<u32, String> = HashMap::new();
        let mut tools_used: HashSet<String> = HashSet::new();
        for (idx, step) in history.steps.iter().enumerate() {
            step_index.insert(step.step_number, idx);
            step_numbers.insert(step.step_number);
            if let Some(bid) = &step.branch_id {
                step_to_branch.insert(step.step_number, bid.clone());
            }
        }
        for branch in branches.values() {
            for step in &branch.steps {
                step_numbers.insert(step.step_number);
                step_to_branch.insert(step.step_number, branch.id.clone());
            }
        }
        if let Some(meta) = &history.metadata {
            if let Some(tools) = &meta.tools_used {
                for t in tools {
                    tools_used.insert(t.clone());
                }
            }
        }

        let formatter = Formatter::new(false);
        let persistence = Persistence::new(&config.persistence);

        Self {
            history,
            config,
            formatter,
            persistence,
            start_time: Instant::now(),
            branches,
            sessions: HashMap::new(),
            step_index,
            step_numbers,
            tools_used,
            step_to_branch,
            branch_depth_cache: HashMap::new(),
            steps_since_cleanup: 0,
            branches_seq: 0,
            active_session: None,
            broadcaster: None,
            cwd: None,
            // for_analysis is read-only — no namespacing decisions get
            // made through this constructor, so an empty project id is
            // fine. Any path that would consult it (process_step) is
            // not reachable from a viewer-side `for_analysis` server.
            project_id: String::new(),
            // Read-only view: never mirrors to a repo.
            repo_sink: None,
            repo_shared: false,
        }
    }

    pub(crate) fn new_history() -> DeliberateHistory {
        let now = Utc::now().to_rfc3339();
        DeliberateHistory {
            steps: Vec::new(),
            branches: Some(Vec::new()),
            completed: false,
            session_id: None,
            created_at: Some(now.clone()),
            updated_at: Some(now),
            metadata: Some(HistoryMetadata {
                total_duration_ms: Some(0),
                revisions_count: Some(0),
                branches_created: Some(0),
                tools_used: Some(Vec::new()),
                project_id: None,
            }),
        }
    }

    pub fn config(&self) -> &DeliberateConfig {
        &self.config
    }

    pub fn history(&self) -> &DeliberateHistory {
        &self.history
    }

    pub fn branches(&self) -> &HashMap<String, Branch> {
        &self.branches
    }

    pub fn sessions(&self) -> &HashMap<String, SessionEntry> {
        &self.sessions
    }

    // validate_thought_prefix, validate_rationale, validate_purpose
    // moved to `super::validation`.

    pub fn extract_tools_used(step: &DeliberateStep) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        if let Some(tools) = &step.tools_used {
            for t in tools {
                if seen.insert(t.clone()) {
                    out.push(t.clone());
                }
            }
        }
        if let NextAction::Structured(a) = &step.next_action {
            if let Some(t) = &a.tool {
                if seen.insert(t.clone()) {
                    out.push(t.clone());
                }
            }
        }
        out
    }

    // recover_xml_injection, validate_required_fields, validate_confidence
    // moved to `super::validation`.

    /// Persist whatever history is currently loaded — the active session
    /// when one is set, otherwise the default history. Logs but doesn't
    /// propagate I/O errors so the in-memory state always stays consistent.
    pub(crate) fn persist_active(&self) {
        if !self.persistence.enabled() {
            return;
        }
        match &self.active_session {
            Some(id) => self.persistence.save_session(id, &self.history),
            None => self.persistence.save_default(&self.history),
        }
    }
}

// Text helpers moved to `crate::think::util::text`.
// Recovery helpers moved to `crate::think::engine::recovery`.
