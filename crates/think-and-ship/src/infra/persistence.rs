//! Atomic JSON persistence under a single XDG data root.
//!
//! Each tool family writes to its own subdirectory ([`Domain::Think`] →
//! `think/sessions/`, [`Domain::Ship`] → `ship/sessions/`,
//! [`Domain::Roadmap`] → `roadmap/sessions/`) so the families stay isolated on
//! disk while sharing the same root.
//!
//! Writes are atomic (`write tmp; rename`). Schema versioning is the
//! CALLER's: this module reads and writes whatever type it is handed, and the
//! two families that version their stored shape — ship and think — carry the
//! version inside that type and refuse a mismatched file in their own
//! wrappers. The families that store no version (roadmap, signal, the trace
//! context, the usage counters) get no such refusal, which is a property of
//! their state types and not something this module supplies.
//!
//! Durability model (family-stores-merge-on-save, extending
//! think-trace-durability): several live server processes — one per agent
//! session — can share one project's state file. A plain overwrite from a
//! process holding stale memory erases mutations another process already
//! acked (the 2026-06-09 think incident; the same mechanism existed here).
//! [`Persistence::save_merging`] / `locked_merge_write` close it: every
//! save takes an exclusive OS advisory lock (`File::lock`) on a sibling
//! `.lock` file, re-reads the on-disk state, and merges it into the
//! in-memory state via a caller-supplied, family-specific merge before
//! writing. The merge policy lives with each family's domain (roadmap /
//! signal / ship), the locking discipline lives here — one seam, three
//! policies.

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
    Roadmap,
    Signal,
    /// Adopted W3C Trace Context (SEP-414). Not a tool family — the one
    /// partition written by the MCP server and read by the `trace export`
    /// CLI in a later process.
    Otel,
    /// Per-tool invocation counts ([`crate::usage`]). Not a tool family, and
    /// deliberately its OWN partition rather than a field on any family's
    /// state: the counter must be readable by the `calls` CLI without loading
    /// a trace, and it must be visibly out of reach of the telemetry
    /// extractor, which reads no partition but its own.
    Usage,
}

impl Domain {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Think => "think",
            Self::Ship => "ship",
            Self::Roadmap => "roadmap",
            Self::Signal => "signal",
            Self::Otel => "otel",
            Self::Usage => "usage",
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
    ///
    /// A blank `THINK_AND_SHIP_DATA_DIR` falls through to the platform chain
    /// rather than resolving to the empty path. Set-but-empty is how a shell
    /// exports a variable it failed to compute, and taking it literally would
    /// point one family at a relative `think/sessions` under the working
    /// directory while every family that guarded against it kept using the
    /// real data root — the same process, its records split across two roots.
    pub fn from_env() -> Self {
        let enabled = env::var(PERSIST_VAR)
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let data_dir = env::var(DATA_DIR_VAR)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(default_data_dir);
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

/// The one data-root fallback chain, shared by every family's config so a
/// process without `HOME` cannot scatter the families across different roots.
pub(crate) fn default_data_dir() -> PathBuf {
    default_data_dir_from(|key| env::var(key).ok())
}

/// Resolve the platform data root through an injected lookup so tests can
/// drive every arm — including the Windows ones — on any OS without touching
/// process environment. Chain: `XDG_DATA_HOME` → `HOME/.local/share` →
/// `%APPDATA%` → `%USERPROFILE%\AppData\Roaming` → the OS temp dir. A set
/// `HOME` wins over `APPDATA` on purpose: git-bash users already have data
/// under the HOME-derived path, and the default must not move it.
pub(crate) fn default_data_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    let non_empty = |key: &str| get(key).filter(|v| !v.trim().is_empty());
    if let Some(xdg) = non_empty("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("think-and-ship");
    }
    if let Some(home) = non_empty("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("think-and-ship");
    }
    if let Some(appdata) = non_empty("APPDATA") {
        return PathBuf::from(appdata).join("think-and-ship");
    }
    if let Some(profile) = non_empty("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join("think-and-ship");
    }
    // No home of any shape (some CI). Persistence still functions; the data
    // is just ephemeral, and the path is valid on both platforms — a literal
    // `/tmp` is not a real place on Windows.
    env::temp_dir().join("think-and-ship")
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
            tracing::warn!(
                "think-and-ship: could not create data dir {}: {e}",
                sessions_dir.display()
            );
        }
        Self {
            enabled: cfg.enabled,
            sessions_dir,
        }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// The store file backing one session. `pub` so a caller that must operate
    /// on the file itself — `roadmap prune`, which backs it up before writing —
    /// doesn't have to reconstruct the path and risk disagreeing with this.
    pub fn path_for(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.json"))
    }

    /// Atomically write `state` to `<sessions_dir>/<session_id>.json`.
    /// No-op when persistence is disabled.
    ///
    /// Plain overwrite — a concurrent process's acked writes are NOT
    /// preserved. Engines whose state can be open in several live processes
    /// at once must use [`Self::save_merging`] instead.
    pub fn save<T: Serialize>(&self, session_id: &str, state: &T) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let path = self.path_for(session_id);
        atomic_write_json(&path, state)
    }

    /// Locked read-merge-write: serialize `state` to
    /// `<sessions_dir>/<session_id>.json` under an exclusive OS advisory
    /// lock, first folding the current on-disk state into it via `merge`.
    /// `merge(memory, disk)` returns what actually lands on disk, so a stale
    /// writer can never erase mutations a concurrent process already
    /// persisted. No-op when persistence is disabled.
    pub fn save_merging<T, F>(&self, session_id: &str, state: &T, merge: F) -> std::io::Result<()>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&T, T) -> T,
    {
        if !self.enabled {
            return Ok(());
        }
        locked_merge_write(&self.path_for(session_id), state, merge)
    }

    /// Read and deserialize the persisted state for `session_id`. Returns
    /// `Ok(None)` when persistence is off or the file doesn't exist.
    pub fn load<T: DeserializeOwned>(&self, session_id: &str) -> std::io::Result<Option<T>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.path_for(session_id);
        match fs::read_to_string(&path) {
            Ok(s) => Ok(Some(
                serde_json::from_str(&s).map_err(std::io::Error::other)?,
            )),
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

/// Serialize `state` to `path` under an exclusive advisory lock on a sibling
/// `<path>.lock` file, merging in whatever is currently on disk via
/// `merge(memory, disk)`. A missing or unreadable on-disk file (corrupt JSON,
/// schema drift) falls back to writing `state` outright — matching `load`'s
/// tolerance. Shared by [`Persistence::save_merging`] and by the ship and
/// think family wrappers, so every family locks identically. The think
/// family's own copy established the pattern and has since been folded onto
/// this one.
pub(crate) fn locked_merge_write<T, F>(path: &Path, state: &T, merge: F) -> std::io::Result<()>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce(&T, T) -> T,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("json.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock()?;
    let on_disk: Option<T> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let merged = on_disk.map(|disk| merge(state, disk));
    let result = atomic_write_json(path, merged.as_ref().unwrap_or(state));
    let _ = lock_file.unlock();
    result
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    // `<name>.json.tmp`, matching the `<name>.json.lock` sibling above. The
    // suffix is appended rather than substituted so the base name survives:
    // the routines that sweep this directory — the reasoning family's
    // project-file deletion and the viewer's session scan — recognise a
    // transient by that suffix, and a bare `.tmp` would swallow the `.json`
    // and hide the file from both.
    let tmp = path.with_extension("json.tmp");
    // Compact (not pretty) JSON: the state file is rewritten in full on every
    // mutation and grows unbounded across sessions, so halving the bytes (and
    // the serialize cost) directly cuts per-mutation write amplification. serde
    // reads pretty and compact interchangeably, so older files still load.
    let json = serde_json::to_string(value).map_err(std::io::Error::other)?;
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

    fn lookup(vars: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn data_dir_prefers_xdg_over_everything() {
        let dir = default_data_dir_from(lookup(vec![
            ("XDG_DATA_HOME", "/xdg"),
            ("HOME", "/home/dev"),
            ("APPDATA", "C:\\Users\\dev\\AppData\\Roaming"),
        ]));
        assert_eq!(dir, Path::new("/xdg").join("think-and-ship"));
    }

    #[test]
    fn data_dir_home_wins_over_appdata() {
        // A git-bash Windows session sets both; the HOME-derived path is where
        // any existing data already lives, so it must keep winning.
        let dir = default_data_dir_from(lookup(vec![
            ("HOME", "/home/dev"),
            ("APPDATA", "C:\\Users\\dev\\AppData\\Roaming"),
        ]));
        assert_eq!(
            dir,
            Path::new("/home/dev")
                .join(".local")
                .join("share")
                .join("think-and-ship")
        );
    }

    #[test]
    fn data_dir_windows_without_home_lands_in_appdata() {
        // The PowerShell / GUI-launched shape: no HOME, no XDG.
        let dir = default_data_dir_from(lookup(vec![(
            "APPDATA",
            "C:\\Users\\dev\\AppData\\Roaming",
        )]));
        assert_eq!(
            dir,
            Path::new("C:\\Users\\dev\\AppData\\Roaming").join("think-and-ship")
        );
    }

    #[test]
    fn data_dir_userprofile_backstops_a_missing_appdata() {
        let dir = default_data_dir_from(lookup(vec![("USERPROFILE", "C:\\Users\\dev")]));
        assert_eq!(
            dir,
            Path::new("C:\\Users\\dev")
                .join("AppData")
                .join("Roaming")
                .join("think-and-ship")
        );
    }

    #[test]
    fn data_dir_empty_values_do_not_capture_the_chain() {
        let dir = default_data_dir_from(lookup(vec![
            ("XDG_DATA_HOME", "  "),
            ("HOME", ""),
            ("APPDATA", "C:\\Users\\dev\\AppData\\Roaming"),
        ]));
        assert_eq!(
            dir,
            Path::new("C:\\Users\\dev\\AppData\\Roaming").join("think-and-ship")
        );
    }

    #[test]
    fn data_dir_falls_back_to_the_os_temp_dir() {
        let dir = default_data_dir_from(lookup(vec![]));
        assert_eq!(dir, env::temp_dir().join("think-and-ship"));
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
    fn roadmap_uses_its_own_subdir() {
        let tmp = TempDir::new().unwrap();
        let r = Persistence::new(&cfg(&tmp), Domain::Roadmap);
        let t = Persistence::new(&cfg(&tmp), Domain::Think);
        let s = Persistence::new(&cfg(&tmp), Domain::Ship);
        assert!(r.sessions_dir().ends_with("roadmap/sessions"));
        assert_ne!(r.sessions_dir(), t.sessions_dir());
        assert_ne!(r.sessions_dir(), s.sessions_dir());
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
