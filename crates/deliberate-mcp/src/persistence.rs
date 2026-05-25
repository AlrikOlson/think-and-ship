//! On-disk persistence for deliberation history.
//!
//! When enabled, the engine writes the default history and every named
//! session as a JSON file under `<data_dir>/sessions/`. Writes are atomic
//! (write to `.tmp`, then rename) so a partial write can't corrupt state.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::PersistenceConfig;
use crate::types::DeliberateHistory;

/// Bump when the on-disk shape changes. Files with mismatched versions are
/// skipped on load (with a stderr warning) so they don't abort startup.
const SCHEMA_VERSION: u32 = 1;

/// Special filename stem used for the "no session_id" default history.
const DEFAULT_STEM: &str = "_default";

#[derive(Debug, Serialize, Deserialize)]
struct PersistedHistory {
    schema_version: u32,
    history: DeliberateHistory,
}

#[derive(Debug, Clone)]
pub struct Persistence {
    enabled: bool,
    sessions_dir: PathBuf,
}

impl Persistence {
    /// Build a persistence handle from config. When persistence is enabled,
    /// the sessions directory is created on demand.
    pub fn new(cfg: &PersistenceConfig) -> Self {
        let sessions_dir = cfg.data_dir.join("sessions");
        if cfg.enabled {
            if let Err(e) = fs::create_dir_all(&sessions_dir) {
                eprintln!(
                    "⚠️ Persistence enabled but could not create data dir {}: {e}",
                    sessions_dir.display()
                );
            }
        }
        Self {
            enabled: cfg.enabled,
            sessions_dir,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Load the default (no-session-id) history if it was previously
    /// persisted. Returns `None` when persistence is off, the file doesn't
    /// exist, or the file's schema version doesn't match.
    pub fn load_default(&self) -> Option<DeliberateHistory> {
        if !self.enabled {
            return None;
        }
        let path = self.path_for(DEFAULT_STEM);
        read_history(&path)
    }

    /// Filename stem used for the default (no-session-id) history file.
    pub fn default_stem() -> &'static str {
        DEFAULT_STEM
    }

    /// Load every session file in `sessions_dir`, keyed by session id.
    /// Silently skips files whose schema is out of date.
    pub fn load_sessions(&self) -> HashMap<String, DeliberateHistory> {
        let mut out: HashMap<String, DeliberateHistory> = HashMap::new();
        if !self.enabled {
            return out;
        }
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
            Err(e) => {
                eprintln!(
                    "⚠️ Failed to read sessions dir {}: {e}",
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
            if stem == DEFAULT_STEM {
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

    pub fn save_default(&self, history: &DeliberateHistory) {
        if !self.enabled {
            return;
        }
        let path = self.path_for(DEFAULT_STEM);
        if let Err(e) = write_history(&path, history) {
            eprintln!("⚠️ Failed to persist default history: {e}");
        }
    }

    pub fn save_session(&self, session_id: &str, history: &DeliberateHistory) {
        if !self.enabled {
            return;
        }
        if !is_safe_session_id(session_id) {
            eprintln!(
                "⚠️ Refusing to persist session with unsafe id \"{session_id}\" — use [A-Za-z0-9_.-] only"
            );
            return;
        }
        let path = self.path_for(session_id);
        if let Err(e) = write_history(&path, history) {
            eprintln!("⚠️ Failed to persist session {session_id}: {e}");
        }
    }

    /// Remove every persisted file. Called by `clear_history` so the disk
    /// state matches the in-memory wipe.
    pub fn delete_all(&self) {
        if !self.enabled {
            return;
        }
        let entries = match fs::read_dir(&self.sessions_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                eprintln!(
                    "⚠️ Failed to read sessions dir {}: {e}",
                    self.sessions_dir.display()
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("⚠️ Failed to delete {}: {e}", path.display());
            }
        }
    }

    fn path_for(&self, stem: &str) -> PathBuf {
        self.sessions_dir.join(format!("{stem}.json"))
    }
}

/// Parse a single session file from disk, returning `None` for missing
/// files, unreadable files, malformed JSON, or schema-version mismatches.
/// Exposed so a passive viewer (e.g. the Tauri desktop GUI) can load the
/// same files the server writes without re-implementing the validation.
pub fn read_history(path: &Path) -> Option<DeliberateHistory> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("⚠️ Failed to read {}: {e}", path.display());
            return None;
        }
    };
    let parsed: PersistedHistory = match serde_json::from_str(&raw) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("⚠️ Failed to parse {}: {e}", path.display());
            return None;
        }
    };
    if parsed.schema_version != SCHEMA_VERSION {
        eprintln!(
            "⚠️ Skipping {} — schema version {} != {SCHEMA_VERSION}",
            path.display(),
            parsed.schema_version
        );
        return None;
    }
    Some(parsed.history)
}

fn write_history(path: &Path, history: &DeliberateHistory) -> io::Result<()> {
    let body = PersistedHistory {
        schema_version: SCHEMA_VERSION,
        history: history.clone(),
    };
    let serialized = serde_json::to_string_pretty(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // Atomic write: dump to a sibling tmp file, then rename. Rename is atomic
    // on POSIX within the same directory.
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, serialized)?;
    fs::rename(tmp, path)?;
    Ok(())
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
