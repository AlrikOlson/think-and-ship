use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("think-and-ship");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("think-and-ship");
    }
    PathBuf::from("/tmp/resolute-mcp")
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
            eprintln!(
                "resolute-mcp: could not create data dir {}: {e}",
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
        if let Err(e) = atomic_write(&path, &state) {
            eprintln!("resolute-mcp: failed to persist state: {e}");
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
            eprintln!(
                "resolute-mcp: skipping {} (schema v{}, expected v{SCHEMA_VERSION})",
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

fn atomic_write(path: &Path, state: &PersistedState) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(state).map_err(std::io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
