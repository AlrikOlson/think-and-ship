use std::env;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ship::domain::objective::Objective;
use crate::ship::domain::task::Task;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    schema_version: u32,
    project_id: String,
    objective: Option<Objective>,
    tasks: Vec<Task>,
    next_action_id: u32,
}

/// Merge policy for the locked merge-on-save discipline
/// (family-stores-merge-on-save). Ship state is CYCLE-shaped — one objective
/// plus its tasks — so the cycle identity (`objective.created_at`) decides:
///
/// - **Different cycles**: the newer cycle wins outright. A stale process
///   still scribbling into a cycle another process has reset away loses by
///   design — that is exactly what keeps `reset` resurrection-proof against
///   stale writers. A state with no objective counts as oldest.
/// - **Same cycle** (identical `created_at`): tasks union by id (disk-only
///   tasks another process acked survive; memory wins per shared task — the
///   LWW caveat the think merge also accepted) and `next_action_id` takes
///   the max so action ids never regress.
fn merge_states(memory: &PersistedState, disk: PersistedState) -> PersistedState {
    fn cycle_stamp(s: &PersistedState) -> Option<&str> {
        s.objective.as_ref().and_then(|o| o.created_at.as_deref())
    }
    let mem_stamp = cycle_stamp(memory);
    let disk_stamp = cycle_stamp(&disk);
    if mem_stamp == disk_stamp {
        let mut merged = PersistedState {
            schema_version: memory.schema_version,
            project_id: memory.project_id.clone(),
            objective: memory.objective.clone(),
            tasks: memory.tasks.to_vec(),
            next_action_id: memory.next_action_id.max(disk.next_action_id),
        };
        for task in disk.tasks {
            if !merged.tasks.iter().any(|t| t.id == task.id) {
                merged.tasks.push(task);
            }
        }
        return merged;
    }
    let newer = |a: &str, b: &str| {
        use chrono::DateTime;
        match (
            DateTime::parse_from_rfc3339(a).ok(),
            DateTime::parse_from_rfc3339(b).ok(),
        ) {
            (Some(a), Some(b)) => a > b,
            (Some(_), None) => true,
            _ => false,
        }
    };
    let disk_is_newer = match (disk_stamp, mem_stamp) {
        (Some(d), Some(m)) => newer(d, m),
        (Some(_), None) => true,
        _ => false,
    };
    if disk_is_newer {
        disk
    } else {
        PersistedState {
            schema_version: memory.schema_version,
            project_id: memory.project_id.clone(),
            objective: memory.objective.clone(),
            tasks: memory.tasks.to_vec(),
            next_action_id: memory.next_action_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub data_dir: PathBuf,
}

impl PersistenceConfig {
    pub fn from_env() -> Self {
        let enabled = env::var("THINK_AND_SHIP_PERSIST")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let data_dir = env::var("THINK_AND_SHIP_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_data_dir());

        Self { enabled, data_dir }
    }
}

fn default_data_dir() -> PathBuf {
    crate::infra::persistence::default_data_dir()
}

#[derive(Debug, Clone)]
pub struct Persistence {
    enabled: bool,
    sessions_dir: PathBuf,
}

impl Persistence {
    pub fn new(cfg: &PersistenceConfig) -> Self {
        // Partition under `ship/` so the think family writes to its own
        // sibling subdirectory and the two never share a `<project>.json`
        // path. Mirrors the layout used by `crate::infra::Persistence`.
        let sessions_dir = cfg.data_dir.join("ship").join("sessions");
        if cfg.enabled
            && let Err(e) = fs::create_dir_all(&sessions_dir)
        {
            tracing::warn!(
                "ship: could not create data dir {}: {e}",
                sessions_dir.display()
            );
        }
        Self {
            enabled: cfg.enabled,
            sessions_dir,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Persist the cycle under the locked merge-on-save discipline (see
    /// `merge_states`) so a concurrent process's acked tasks — or its newer
    /// cycle — are never erased by a stale writer.
    pub fn save(
        &self,
        project_id: &str,
        objective: &Option<Objective>,
        tasks: &[Task],
        next_action_id: u32,
    ) {
        if !self.enabled {
            return;
        }
        let state = PersistedState {
            schema_version: SCHEMA_VERSION,
            project_id: project_id.to_string(),
            objective: objective.clone(),
            tasks: tasks.to_vec(),
            next_action_id,
        };
        let path = self.sessions_dir.join(format!("{project_id}.json"));
        if let Err(e) = crate::infra::persistence::locked_merge_write(&path, &state, merge_states) {
            tracing::warn!("ship: failed to persist state: {e}");
        }
    }

    pub fn load(&self, project_id: &str) -> Option<(Option<Objective>, Vec<Task>, u32)> {
        if !self.enabled {
            return None;
        }
        let path = self.sessions_dir.join(format!("{project_id}.json"));
        let data = fs::read_to_string(&path).ok()?;
        let state: PersistedState = serde_json::from_str(&data).ok()?;
        if state.schema_version != SCHEMA_VERSION {
            tracing::warn!(
                "ship: skipping {} (schema v{}, expected v{SCHEMA_VERSION})",
                path.display(),
                state.schema_version
            );
            return None;
        }
        Some((state.objective, state.tasks, state.next_action_id))
    }

    pub fn clear(&self, project_id: &str) {
        if !self.enabled {
            return;
        }
        let path = self.sessions_dir.join(format!("{project_id}.json"));
        let _ = fs::remove_file(&path);
    }
}
