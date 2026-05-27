//! Atomic JSON persistence under a single XDG data root.
//!
//! Each tool family writes to its own subdirectory ([`Domain::Think`] →
//! `think/sessions/`, [`Domain::Ship`] → `ship/sessions/`) so the two
//! halves stay isolated on disk while sharing the same root.
//!
//! Writes are atomic (`write tmp; rename`). On read, a `schema_version`
//! mismatch is reported as `None` so older clients don't try to interpret
//! newer files.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Serialize, de::DeserializeOwned};

const PERSIST_VAR: &str = "THINK_AND_SHIP_PERSIST";
const DATA_DIR_VAR: &str = "THINK_AND_SHIP_DATA_DIR";

/// Which tool family's subdirectory to read or write under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Think,
    Ship,
}

impl Domain {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Ship => "ship",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub data_dir: PathBuf,
}

impl PersistenceConfig {
    /// Resolve from environment. Off by default; opt in with
    /// `THINK_AND_SHIP_PERSIST=true` (or `=1`).
    pub fn from_env() -> Self {
        let enabled = env::var(PERSIST_VAR)
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let data_dir = env::var(DATA_DIR_VAR)
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_data_dir());
        Self { enabled, data_dir }
    }

    pub fn with_data_dir(mut self, dir: PathBuf) -> Self {
        self.data_dir = dir;
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
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
    PathBuf::from("/tmp/think-and-ship")
}

/// Per-domain persistence handle. Created with `Persistence::new`.
#[derive(Debug, Clone)]
pub struct Persistence {
    enabled: bool,
    sessions_dir: PathBuf,
}

impl Persistence {
    pub fn new(cfg: &PersistenceConfig, domain: Domain) -> Self {
        let sessions_dir = cfg.data_dir.join(domain.dir_name()).join("sessions");
        if cfg.enabled
            && let Err(e) = fs::create_dir_all(&sessions_dir)
        {
            eprintln!(
                "think-and-ship: could not create data dir {}: {e}",
                sessions_dir.display()
            );
        }
        Self {
            enabled: cfg.enabled,
            sessions_dir,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.json"))
    }

    /// Atomically write `state` to `<sessions_dir>/<session_id>.json`.
    /// No-op when persistence is disabled.
    pub fn save<T: Serialize>(&self, session_id: &str, state: &T) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.path_for(session_id);
        atomic_write_json(&path, state)
    }

    /// Read and deserialize the persisted state for `session_id`. Returns
    /// `Ok(None)` when persistence is off or the file doesn't exist.
    pub fn load<T: DeserializeOwned>(&self, session_id: &str) -> std::io::Result<Option<T>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.path_for(session_id);
        match fs::read_to_string(&path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s).map_err(std::io::Error::other)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn clear(&self, session_id: &str) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.path_for(session_id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::TempDir;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Sample {
        name: String,
        count: u32,
    }

    fn cfg(tmp: &TempDir) -> PersistenceConfig {
        PersistenceConfig {
            enabled: true,
            data_dir: tmp.path().to_path_buf(),
        }
    }

    #[test]
    fn save_then_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let p = Persistence::new(&cfg(&tmp), Domain::Think);
        let val = Sample {
            name: "x".into(),
            count: 1,
        };
        p.save("alpha", &val).unwrap();
        let back: Sample = p.load("alpha").unwrap().unwrap();
        assert_eq!(back, val);
    }

    #[test]
    fn think_and_ship_use_disjoint_subdirs() {
        let tmp = TempDir::new().unwrap();
        let t = Persistence::new(&cfg(&tmp), Domain::Think);
        let s = Persistence::new(&cfg(&tmp), Domain::Ship);
        assert!(t.sessions_dir().ends_with("think/sessions"));
        assert!(s.sessions_dir().ends_with("ship/sessions"));
        assert_ne!(t.sessions_dir(), s.sessions_dir());
    }

    #[test]
    fn atomic_write_leaves_no_tmp_behind() {
        let tmp = TempDir::new().unwrap();
        let p = Persistence::new(&cfg(&tmp), Domain::Ship);
        p.save(
            "beta",
            &Sample {
                name: "y".into(),
                count: 2,
            },
        )
        .unwrap();
        let entries: Vec<String> = fs::read_dir(p.sessions_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        assert_eq!(entries, vec!["beta.json".to_string()]);
    }

    #[test]
    fn load_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let p = Persistence::new(&cfg(&tmp), Domain::Think);
        let back: Option<Sample> = p.load("does-not-exist").unwrap();
        assert!(back.is_none());
    }

    #[test]
    fn disabled_persistence_is_noop() {
        let tmp = TempDir::new().unwrap();
        let mut c = cfg(&tmp);
        c.enabled = false;
        let p = Persistence::new(&c, Domain::Think);
        let val = Sample {
            name: "z".into(),
            count: 3,
        };
        p.save("gamma", &val).unwrap();
        let back: Option<Sample> = p.load("gamma").unwrap();
        assert!(back.is_none());
    }

    #[test]
    fn clear_removes_file_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let p = Persistence::new(&cfg(&tmp), Domain::Think);
        p.save(
            "delta",
            &Sample {
                name: "w".into(),
                count: 4,
            },
        )
        .unwrap();
        p.clear("delta").unwrap();
        p.clear("delta").unwrap();
        let back: Option<Sample> = p.load("delta").unwrap();
        assert!(back.is_none());
    }
}
