//! On-disk persistence for deliberation history.
//!
//! When enabled, the engine writes the default history and every named
//! session as a JSON file under the `think` partition of the shared data root.
//!
//! **This module holds policy, not mechanism.** The advisory lock, the
//! read-merge-write order, the atomic rename and the partition path all come
//! from [`crate::infra::persistence`], which every family shares. What lives
//! here is what is true of a reasoning trace in particular:
//!
//! 1. **Per-project default file.** The default history persists to
//!    `<project_id>.json`, not a shared global file, so processes from
//!    different projects never contend on the same bytes. The legacy global
//!    `_default.json` is migrated from (cwd-attributed) on first load and
//!    left in place for other projects' migrations.
//! 2. **Union by step identity.** On save, the on-disk copy is folded in by
//!    [`merge_histories`]: steps are known by number, by record id or by
//!    content, so a stale process can never erase steps a concurrent process
//!    already persisted — the bug that lost steps 1037–1072 on 2026-06-09 —
//!    and a renumbering writer is not mistaken for a source of new ones. The
//!    disk file is the durable archive and may grow beyond the in-memory
//!    `max_history_size` window.
//! 3. **A versioned envelope.** The history is stored inside a
//!    `schema_version` wrapper, and a file written under a version this build
//!    does not know is neither read nor merged.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::infra::PersistenceConfig;
use crate::infra::project_id_for_path;
use crate::think::domain::{HistoryMetadata, ThinkHistory};

/// Bump when the on-disk shape changes. Files with mismatched versions are
/// skipped on load (with a stderr warning) so they don't abort startup.
const SCHEMA_VERSION: u32 = 1;

/// Special filename stem used for the "no session_id" default history.
const DEFAULT_STEM: &str = "_default";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedHistory {
    schema_version: u32,
    history: ThinkHistory,
}

#[derive(Debug, Clone)]
pub struct Persistence {
    enabled: bool,
    sessions_dir: PathBuf,
    /// Filename stem of the default history: the project id when known
    /// (engine path), the legacy `_default` otherwise (viewer, raw handles).
    stem: String,
}

impl Persistence {
    /// Build a persistence handle from config. When persistence is enabled,
    /// the sessions directory is created on demand. The default history uses
    /// the legacy global stem — engine code should prefer [`Self::for_project`].
    pub fn new(cfg: &PersistenceConfig) -> Self {
        Self::for_project(cfg, DEFAULT_STEM)
    }

    /// Build a persistence handle whose default history is scoped to
    /// `project_id` (`<project_id>.json`), so concurrent servers in other
    /// projects never write the same default file.
    pub fn for_project(cfg: &PersistenceConfig, project_id: &str) -> Self {
        // The partition path — and its creation — comes from the shared
        // handle under `Domain::Think`, so this family cannot drift onto a
        // directory the rest of the server does not look in.
        let sessions_dir = crate::infra::Persistence::new(cfg, crate::infra::Domain::Think)
            .sessions_dir()
            .to_path_buf();
        let stem = if is_safe_session_id(project_id) {
            project_id.to_string()
        } else {
            if project_id != DEFAULT_STEM {
                tracing::warn!(
                    "Project id \"{project_id}\" is not filename-safe; falling back to the {DEFAULT_STEM} history"
                );
            }
            DEFAULT_STEM.to_string()
        };
        Self {
            enabled: cfg.enabled,
            sessions_dir,
            stem,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Load the default history if it was previously persisted. Returns
    /// `None` when persistence is off, the file doesn't exist, or the file's
    /// schema version doesn't match. Project-scoped handles additionally run
    /// a one-time adoption of this project's steps from the legacy global
    /// `_default.json`, guarded by `metadata.legacy_default_migrated` — NOT
    /// by the project file's existence, because a May-era bare-project
    /// session file can pre-exist under exactly this filename (errata). The
    /// legacy file is left in place for other projects to migrate from.
    pub fn load_default(&self) -> Option<ThinkHistory> {
        if !self.enabled {
            return None;
        }
        let loaded = read_history(&self.path_for(&self.stem));
        if self.stem == DEFAULT_STEM {
            return loaded;
        }
        let already_migrated = loaded
            .as_ref()
            .and_then(|h| h.metadata.as_ref())
            .and_then(|m| m.legacy_default_migrated)
            .unwrap_or(false);
        if already_migrated {
            return loaded;
        }
        self.adopt_from_legacy_default(loaded)
    }

    /// One-time adoption of this project's steps from the legacy global
    /// `_default.json` (which mixed every project under one file). A step
    /// belongs to this project when the cwd it recorded resolves to this
    /// project id; adopted steps are UNIONED into whatever already sits in
    /// the project file (a stale bare-project session, an earlier partial
    /// state) — existing steps win on step_number conflict. Persists the
    /// migration marker so the adoption never re-runs.
    fn adopt_from_legacy_default(&self, loaded: Option<ThinkHistory>) -> Option<ThinkHistory> {
        let legacy_steps: Vec<_> = read_history(&self.path_for(DEFAULT_STEM))
            .map(|h| {
                h.steps
                    .into_iter()
                    .filter(|s| {
                        s.cwd
                            .as_deref()
                            .map(|cwd| project_id_for_path(Path::new(cwd)) == self.stem)
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if legacy_steps.is_empty() {
            // Nothing to adopt (yet) — don't write a marker, so steps that a
            // still-running legacy-binary writer lands later are adopted on
            // a future startup.
            return loaded;
        }
        let mut merged = loaded.unwrap_or_else(|| ThinkHistory {
            steps: Vec::new(),
            branches: None,
            completed: false,
            session_id: None,
            created_at: None,
            updated_at: None,
            metadata: None,
        });
        let adopted_count = legacy_steps.len();
        merged = merge_histories(
            &merged,
            ThinkHistory {
                steps: legacy_steps,
                branches: None,
                completed: false,
                session_id: None,
                created_at: None,
                updated_at: None,
                metadata: None,
            },
        );
        merged
            .metadata
            .get_or_insert_with(HistoryMetadata::default)
            .legacy_default_migrated = Some(true);
        let path = self.path_for(&self.stem);
        if let Err(e) = locked_write(&path, &merged, MergeMode::Merge) {
            tracing::warn!("Failed to adopt legacy default history: {e}");
        } else {
            eprintln!(
                "think-and-ship: adopted {adopted_count} step(s) for {} from the legacy _default history",
                self.stem
            );
        }
        Some(merged)
    }

    /// Persist an empty, migration-marked default history. Called by wipe
    /// after deleting this project's files so a later restart can't
    /// re-adopt steps from the legacy `_default.json`.
    pub fn save_default_tombstone(&self) {
        if !self.enabled || self.stem == DEFAULT_STEM {
            return;
        }
        let tombstone = ThinkHistory {
            steps: Vec::new(),
            branches: None,
            completed: false,
            session_id: None,
            created_at: None,
            updated_at: None,
            metadata: Some(HistoryMetadata {
                legacy_default_migrated: Some(true),
                ..HistoryMetadata::default()
            }),
        };
        let path = self.path_for(&self.stem);
        if let Err(e) = locked_write(&path, &tombstone, MergeMode::Replace) {
            tracing::warn!("Failed to persist wipe tombstone: {e}");
        }
    }

    /// Filename stem used for the default (no-session-id) history file.
    pub fn default_stem() -> &'static str {
        DEFAULT_STEM
    }

    /// Load every session file in `sessions_dir`, keyed by session id.
    /// Silently skips files whose schema is out of date.
    pub fn load_sessions(&self) -> HashMap<String, ThinkHistory> {
        let mut out: HashMap<String, ThinkHistory> = HashMap::new();
        if !self.enabled {
            return out;
        }
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
            Err(e) => {
                tracing::warn!(
                    "Failed to read sessions dir {}: {e}",
                    self.sessions_dir.display()
                );
                return out;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            // Skip the legacy global default and this handle's own default
            // file — the latter is the main history, not a named session.
            if stem == DEFAULT_STEM || stem == self.stem {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Some(history) = read_history(&path) {
                out.insert(stem, history);
            }
        }
        out
    }

    pub fn save_default(&self, history: &ThinkHistory) {
        if !self.enabled {
            return;
        }
        let path = self.path_for(&self.stem);
        if let Err(e) = locked_write(&path, history, MergeMode::Merge) {
            tracing::warn!("Failed to persist default history: {e}");
        }
    }

    /// Save the default history WITHOUT merging in on-disk steps. Only for
    /// callers that just rewrote step identities (startup renumbering) —
    /// merging there would resurrect the old-numbered steps as duplicates.
    pub fn save_default_replacing(&self, history: &ThinkHistory) {
        if !self.enabled {
            return;
        }
        let path = self.path_for(&self.stem);
        if let Err(e) = locked_write(&path, history, MergeMode::Replace) {
            tracing::warn!("Failed to persist default history: {e}");
        }
    }

    pub fn save_session(&self, session_id: &str, history: &ThinkHistory) {
        self.save_session_inner(session_id, history, MergeMode::Merge);
    }

    /// Session-file variant of [`Self::save_default_replacing`].
    pub fn save_session_replacing(&self, session_id: &str, history: &ThinkHistory) {
        self.save_session_inner(session_id, history, MergeMode::Replace);
    }

    fn save_session_inner(&self, session_id: &str, history: &ThinkHistory, mode: MergeMode) {
        if !self.enabled {
            return;
        }
        if !is_safe_session_id(session_id) {
            tracing::warn!(
                "Refusing to persist session with unsafe id \"{session_id}\" — use [A-Za-z0-9_.-] only"
            );
            return;
        }
        let path = self.path_for(session_id);
        if let Err(e) = locked_write(&path, history, mode) {
            tracing::warn!("Failed to persist session {session_id}: {e}");
        }
    }

    /// Remove this project's persisted files: the default history plus every
    /// session namespaced under it. Called by `clear_history` so the disk
    /// state matches the in-memory wipe — scoped to the project so a wipe
    /// can never destroy other projects' traces in the shared data dir.
    pub fn delete_project_files(&self) {
        if !self.enabled {
            return;
        }
        let session_prefix = format!("{}{}", self.stem, crate::infra::PROJECT_SEP);
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(
                    "Failed to read sessions dir {}: {e}",
                    self.sessions_dir.display()
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            // Project ids may legitimately contain dots, so strip the known
            // artifact suffixes rather than splitting on the first dot.
            let Some(stem_of) = name
                .strip_suffix(".json")
                .or_else(|| name.strip_suffix(".json.lock"))
                .or_else(|| name.strip_suffix(".json.tmp"))
            else {
                continue;
            };
            if stem_of != self.stem && !stem_of.starts_with(&session_prefix) {
                continue;
            }
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("Failed to delete {}: {e}", path.display());
            }
        }
    }

    fn path_for(&self, stem: &str) -> PathBuf {
        self.sessions_dir.join(format!("{stem}.json"))
    }

    /// On-disk path of this project's default history. Exposed so `prune` can
    /// back the file up before rewriting it.
    pub fn default_store_path(&self) -> PathBuf {
        self.path_for(&self.stem)
    }
}

/// Parse a single session file from disk, returning `None` for missing
/// files, unreadable files, malformed JSON, or schema-version mismatches.
/// Exposed so a passive viewer (e.g. the Tauri desktop GUI) can load the
/// same files the server writes without re-implementing the validation.
pub fn read_history(path: &Path) -> Option<ThinkHistory> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("Failed to read {}: {e}", path.display());
            return None;
        }
    };
    let parsed: PersistedHistory = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to parse {}: {e}", path.display());
            return None;
        }
    };
    if parsed.schema_version != SCHEMA_VERSION {
        tracing::warn!(
            "Skipping {} — schema version {} != {SCHEMA_VERSION}",
            path.display(),
            parsed.schema_version
        );
        return None;
    }
    Some(parsed.history)
}

/// Whether a save unions in steps already on disk (the durability default)
/// or replaces the file outright (startup renumbering, which rewrites step
/// identities and must not resurrect the old-numbered copies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeMode {
    Merge,
    Replace,
}

/// Serialize `history` to `path` under the shared locked merge-on-save
/// discipline in [`crate::infra::persistence`]. The advisory lock, the
/// read-merge-write order and the atomic rename are that one implementation;
/// what is family-specific — and all that is passed in here — is the policy
/// deciding what the on-disk copy contributes.
fn locked_write(path: &Path, history: &ThinkHistory, mode: MergeMode) -> io::Result<()> {
    let state = PersistedHistory {
        schema_version: SCHEMA_VERSION,
        history: history.clone(),
    };
    crate::infra::persistence::locked_merge_write(path, &state, |memory, disk| match mode {
        // Renumbering rewrote step identities before this save, so folding the
        // on-disk copy back in would resurrect the old-numbered steps as
        // duplicates. The lock is still taken — this replaces the file, it
        // does not race for it.
        MergeMode::Replace => memory.clone(),
        // A file written under a schema this build does not know is not
        // interpreted, matching `read_history`'s refusal to parse one.
        MergeMode::Merge if disk.schema_version != SCHEMA_VERSION => {
            tracing::warn!(
                "Not merging a history at schema version {} != {SCHEMA_VERSION}",
                disk.schema_version
            );
            memory.clone()
        }
        MergeMode::Merge => PersistedHistory {
            schema_version: SCHEMA_VERSION,
            history: merge_histories(&memory.history, disk.history),
        },
    })
}

/// Union `disk` into a copy of `memory`: steps are keyed by `step_number`
/// (memory wins on conflict — pin/revise mutations must stick), branches by
/// id. Disk-only steps surviving the union is the anti-clobber property; it
/// also makes the file a durable archive that outlives the in-memory
/// `max_history_size` trim window.
/// THE one conflict rule for two copies of the think trace: union steps by
/// `step_number` (existing/memory always wins — a step is never replaced) and
/// branches by id. Shared by the disk merge and the cloud reconcile
/// (`ReasoningServer::adopt_steps`), so the system has exactly one
/// implementation of the think merge (sync-think-reconcile).
///
/// A step is "known" by number OR by content identity: when a concurrent
/// writer renumbers the trace (the startup renumber rewrites step_numbers but
/// never prose or the server-set timestamp), the other copy must recognize the
/// renumbered steps as the SAME steps, not adopt them as new ones — otherwise
/// every renumber+merge round duplicates the trace (cli-renumber-duplication).
pub(crate) fn merge_histories(memory: &ThinkHistory, disk: ThinkHistory) -> ThinkHistory {
    let mut merged = memory.clone();
    let known: HashSet<u32> = merged.steps.iter().map(|s| s.step_number).collect();
    let known_content: HashSet<(String, String, String)> = merged
        .steps
        .iter()
        .filter_map(|s| s.content_key())
        .collect();
    let known_ids: HashSet<&str> = merged
        .steps
        .iter()
        .filter_map(|s| s.record_id.as_deref())
        .collect();
    let mut adopted: Vec<_> = disk
        .steps
        .into_iter()
        .filter(|s| {
            !known.contains(&s.step_number)
                && s.record_id
                    .as_deref()
                    .is_none_or(|id| !known_ids.contains(id))
                && s.content_key().is_none_or(|k| !known_content.contains(&k))
        })
        .collect();
    let mem_branch_ids: HashSet<String> = merged
        .branches
        .iter()
        .flatten()
        .map(|b| b.id.clone())
        .collect();
    let extra_branches: Vec<_> = disk
        .branches
        .into_iter()
        .flatten()
        .filter(|b| !mem_branch_ids.contains(&b.id))
        .collect();
    if adopted.is_empty() && extra_branches.is_empty() {
        return merged;
    }
    if !adopted.is_empty() {
        merged.steps.append(&mut adopted);
        merged.steps.sort_by_key(|s| s.step_number);
    }
    if !extra_branches.is_empty() {
        merged
            .branches
            .get_or_insert_with(Vec::new)
            .extend(extra_branches);
    }
    merged
}

/// Strict allowlist for session-id filenames. Prevents path traversal
/// (`..`), separators (`/`, `\`), and weird shell characters from landing
/// on disk regardless of what the LLM sends.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && id != "."
        && id != ".."
}
