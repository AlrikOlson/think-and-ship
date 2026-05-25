//! Shared application state. One [`AppState`] is constructed at startup
//! (via [`AppState::discover`]) and mutated from background tasks. Tauri
//! commands take a snapshot through `&Arc<Mutex<AppState>>`.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use deliberate_mcp::persistence::{Persistence, read_history};
use deliberate_mcp::types::{Branch, BranchStatus, DeliberateHistory, DeliberateStep};
use serde::Serialize;

/// Which data path is currently delivering live updates. Reflected in
/// the status bar so the user always knows whether they are seeing a
/// real-time stream or a polled file.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    /// Neither socket nor file source is producing data.
    None,
    /// Only the on-disk session files are being watched.
    File,
    /// Only the broadcast socket is delivering frames.
    Socket,
    /// Both are active; the socket is treated as primary.
    SocketAndFile,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub mode: SourceMode,
    pub socket_path: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub persistence_enabled: bool,
}

/// A single session's snapshot — what the frontend renders. `branches` is
/// flat (keyed by id) so the frontend can lay out lanes via `from_step`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub session_id: Option<String>,
    pub history: DeliberateHistory,
    pub branches: HashMap<String, Branch>,
}

impl SessionSnapshot {
    fn empty(session_id: Option<String>) -> Self {
        Self {
            session_id,
            history: DeliberateHistory {
                steps: Vec::new(),
                branches: Some(Vec::new()),
                completed: false,
                session_id: None,
                created_at: None,
                updated_at: None,
                metadata: None,
            },
            branches: HashMap::new(),
        }
    }

    /// Reconstruct branches map from a history's persisted `branch_id`
    /// fields. Matches the rebuild [`deliberate_mcp::server::ReasoningServer::new`]
    /// performs when it rehydrates from disk.
    fn rebuild_branches(history: &DeliberateHistory) -> HashMap<String, Branch> {
        let mut branches: HashMap<String, Branch> = HashMap::new();
        // Steps in branches are stored in their own Branch entries in
        // server memory, but on disk they live as top-level history
        // steps with a branch_id. Reconstruct accordingly.
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
        // The on-disk `branches` field, when present, supplies authoritative
        // status + merged_into pointers.
        if let Some(persisted) = &history.branches {
            for b in persisted {
                if let Some(existing) = branches.get_mut(&b.id) {
                    existing.status = b.status;
                    existing.merged_into = b.merged_into;
                    existing.name = b.name.clone();
                    existing.depth = b.depth;
                    existing.from_step = b.from_step;
                    existing.created_at = b.created_at.clone();
                } else {
                    branches.insert(b.id.clone(), b.clone());
                }
            }
        }
        branches
    }

    /// Append a step (main line or branch). No-op if a step with the
    /// same number already exists — broadcast frames may race with a
    /// just-arrived file snapshot.
    pub fn apply_step_appended(&mut self, step: DeliberateStep) {
        if self
            .history
            .steps
            .iter()
            .any(|s| s.step_number == step.step_number)
        {
            return;
        }
        // If the step belongs to a branch, also update the branch entry.
        if let (Some(bid), Some(from)) = (&step.branch_id, step.branch_from) {
            let entry = self.branches.entry(bid.clone()).or_insert_with(|| Branch {
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
        self.history.steps.push(step);
    }

    pub fn apply_step_revised(&mut self, revised_step: u32, by_step: u32) {
        if let Some(target) = self
            .history
            .steps
            .iter_mut()
            .find(|s| s.step_number == revised_step)
        {
            target.revised_by = Some(by_step);
        }
    }

    pub fn apply_pin_changed(&mut self, step_number: u32, pinned: bool) {
        if let Some(target) = self
            .history
            .steps
            .iter_mut()
            .find(|s| s.step_number == step_number)
        {
            target.pinned = if pinned { Some(true) } else { None };
        }
    }

    pub fn apply_estimate_revised(&mut self, _old: u32, new: u32) {
        if let Some(last) = self.history.steps.last_mut() {
            last.estimated_total = new;
        }
    }

    pub fn apply_branch_status(
        &mut self,
        branch_id: &str,
        status: BranchStatus,
        merged_into: Option<u32>,
    ) {
        if let Some(b) = self.branches.get_mut(branch_id) {
            b.status = status;
            b.merged_into = if matches!(status, BranchStatus::Merged) {
                merged_into
            } else {
                None
            };
        }
    }

    /// Replace contents with a freshly-loaded `DeliberateHistory` from disk.
    pub fn replace_from_history(&mut self, history: DeliberateHistory) {
        self.branches = Self::rebuild_branches(&history);
        self.history = history;
    }
}

pub struct AppState {
    pub source: SourceInfo,
    /// Indexed by session_id; the empty-string key holds the default
    /// (no-session-id) trace.
    pub sessions: HashMap<String, SessionSnapshot>,
    /// Most recently focused session. Defaults to "" (the default trace).
    pub active_session: String,
}

impl AppState {
    pub fn discover() -> Self {
        let socket_path = resolve_socket_path();
        let data_dir = resolve_data_dir();
        // Persistence is effectively "on" for the viewer when the
        // sessions directory actually exists on disk — that is the
        // observable signal that the server has been writing there.
        // Reading our own `DELIBERATE_PERSIST` env was wrong: the viewer
        // doesn't need that var set; the *server* does.
        let persistence_enabled = data_dir
            .as_ref()
            .map(|d| d.join("sessions").is_dir())
            .unwrap_or(false);

        let mut sessions: HashMap<String, SessionSnapshot> = HashMap::new();
        sessions.insert(String::new(), SessionSnapshot::empty(None));

        if let Some(dir) = &data_dir {
            let sessions_dir = dir.join("sessions");
            // Default history first.
            let default_path = sessions_dir.join(format!("{}.json", Persistence::default_stem()));
            if let Some(history) = read_history(&default_path) {
                if let Some(snap) = sessions.get_mut("") {
                    snap.replace_from_history(history);
                }
            }
            // Then any named sessions.
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if stem == Persistence::default_stem() {
                        continue;
                    }
                    if path.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    if let Some(history) = read_history(&path) {
                        let mut snap = SessionSnapshot::empty(Some(stem.to_string()));
                        snap.replace_from_history(history);
                        sessions.insert(stem.to_string(), snap);
                    }
                }
            }
        }

        let mode = match (socket_path.is_some(), data_dir.is_some()) {
            (true, true) => SourceMode::SocketAndFile,
            (true, false) => SourceMode::Socket,
            (false, true) => SourceMode::File,
            (false, false) => SourceMode::None,
        };

        Self {
            source: SourceInfo {
                mode,
                socket_path,
                data_dir,
                persistence_enabled,
            },
            sessions,
            active_session: String::new(),
        }
    }

    pub fn session_mut(&mut self, session_id: &Option<String>) -> &mut SessionSnapshot {
        let key = session_id.clone().unwrap_or_default();
        self.sessions
            .entry(key.clone())
            .or_insert_with(|| SessionSnapshot::empty(session_id.clone()))
    }

    pub fn session(&self, key: &str) -> Option<&SessionSnapshot> {
        self.sessions.get(key)
    }
}

/// Default socket path the viewer probes when `DELIBERATE_BROADCAST_PATH`
/// isn't exported in its own env. Matches the path documented in the
/// README and in the example `~/.claude.json` env block, so the common
/// case requires no setup on the viewer side.
const DEFAULT_SOCKET_PATH: &str = "/tmp/deliberate.sock";

fn resolve_socket_path() -> Option<PathBuf> {
    if let Ok(raw) = env::var("DELIBERATE_BROADCAST_PATH") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    Some(PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// Mirror `deliberate_mcp::config::default_data_dir` exactly so the
/// viewer ends up at the same path the server wrote to. On macOS this
/// means we deliberately ignore `dirs::data_dir()` (which would point
/// at `~/Library/Application Support/`) and follow the same XDG-with-
/// Linux-fallback the server uses. The path is returned whether or
/// not it exists yet — the watcher creates the sessions subdir on
/// demand.
fn resolve_data_dir() -> Option<PathBuf> {
    if let Ok(raw) = env::var("DELIBERATE_DATA_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("deliberate-mcp"));
        }
    }
    if let Ok(home) = env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return Some(
                PathBuf::from(trimmed)
                    .join(".local")
                    .join("share")
                    .join("deliberate-mcp"),
            );
        }
    }
    None
}
