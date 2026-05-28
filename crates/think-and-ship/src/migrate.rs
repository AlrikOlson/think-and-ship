//! On-disk migration of v0.1.x session files into the unified layout.
//!
//! v0.1.x persisted sessions under:
//!
//! ```text
//! ~/.local/share/deliberate-mcp/sessions/<project>.json
//! ~/.local/share/resolute-mcp/sessions/<project>.json
//! ```
//!
//! The unified layout (v0.2.0+) partitions both families under one root:
//!
//! ```text
//! ~/.local/share/think-and-ship/think/sessions/<project>.json
//! ~/.local/share/think-and-ship/ship/sessions/<project>.json
//! ```
//!
//! Migration is **one-way** and **idempotent**: a `.migrated-from-v0.1`
//! marker file in the new root short-circuits subsequent runs. Existing
//! files in the new root are never overwritten; conflicts log a warning
//! and skip the offender so manual recovery is possible.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

const MARKER_FILE: &str = ".migrated-from-v0.1";

/// Outcome of a migration pass — useful for tests and CLI status output.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub already_done: bool,
    pub moved: usize,
    pub skipped: usize,
}

/// Migrate v0.1.x persisted state into `new_root`. Idempotent; safe to
/// call on every startup.
pub fn migrate_v0_1_data(new_root: &Path) -> io::Result<MigrationReport> {
    let marker = new_root.join(MARKER_FILE);
    if marker.exists() {
        return Ok(MigrationReport {
            already_done: true,
            ..Default::default()
        });
    }

    let mut report = MigrationReport::default();

    let Some(home) = home_dir() else {
        // No home? Can't locate the v0.1 dirs; do nothing but still drop
        // the marker so we don't keep retrying on every startup.
        fs::create_dir_all(new_root)?;
        write_marker(&marker)?;
        return Ok(report);
    };

    let xdg_root = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"));

    let pairs: &[(&str, &str)] = &[("deliberate-mcp", "think"), ("resolute-mcp", "ship")];

    for (legacy_name, family) in pairs {
        let src = xdg_root.join(legacy_name).join("sessions");
        let dst = new_root.join(family).join("sessions");
        if !src.exists() {
            continue;
        }
        fs::create_dir_all(&dst)?;
        for entry in fs::read_dir(&src)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let dst_path = dst.join(&file_name);
            if dst_path.exists() {
                warn!(
                    "migration: {} already exists in {}; leaving v0.1 copy at {} for manual recovery",
                    file_name.to_string_lossy(),
                    dst.display(),
                    entry.path().display(),
                );
                report.skipped += 1;
                continue;
            }
            fs::rename(entry.path(), &dst_path)?;
            info!(
                "migration: moved {} → {}",
                entry.path().display(),
                dst_path.display(),
            );
            report.moved += 1;
        }
    }

    fs::create_dir_all(new_root)?;
    write_marker(&marker)?;
    Ok(report)
}

fn write_marker(path: &Path) -> io::Result<()> {
    fs::write(path, "v0.1.x persisted data migrated by think-and-ship\n")
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn marker(root: &Path) -> PathBuf {
        root.join(MARKER_FILE)
    }

    #[test]
    fn no_v0_1_dirs_still_writes_marker_and_returns_clean_report() {
        let new_root = TempDir::new().unwrap();
        // Force HOME to a tempdir with nothing in it so the legacy lookup
        // finds no sources.
        let home_override = TempDir::new().unwrap();
        let prev_home = env::var_os("HOME");
        let prev_xdg = env::var_os("XDG_DATA_HOME");
        // SAFETY: parallel tests in this module mostly use unrelated
        // env vars but this one touches HOME; the test is intentionally
        // self-contained and resets at the end.
        unsafe {
            env::set_var("HOME", home_override.path());
            env::remove_var("XDG_DATA_HOME");
        }

        let report = migrate_v0_1_data(new_root.path()).unwrap();
        assert!(!report.already_done);
        assert_eq!(report.moved, 0);
        assert_eq!(report.skipped, 0);
        assert!(marker(new_root.path()).exists());

        // Second run is a no-op (already_done=true).
        let report2 = migrate_v0_1_data(new_root.path()).unwrap();
        assert!(report2.already_done);

        unsafe {
            match prev_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            if let Some(v) = prev_xdg {
                env::set_var("XDG_DATA_HOME", v);
            }
        }
    }

    #[test]
    fn moves_v0_1_files_into_family_subdirs() {
        let xdg = TempDir::new().unwrap();
        let new_root = TempDir::new().unwrap();

        // Stage v0.1.x layout
        let deliberate = xdg.path().join("deliberate-mcp").join("sessions");
        let resolute = xdg.path().join("resolute-mcp").join("sessions");
        fs::create_dir_all(&deliberate).unwrap();
        fs::create_dir_all(&resolute).unwrap();
        fs::write(deliberate.join("alpha.json"), r#"{"v":"think"}"#).unwrap();
        fs::write(resolute.join("alpha.json"), r#"{"v":"ship"}"#).unwrap();

        let prev_home = env::var_os("HOME");
        let prev_xdg = env::var_os("XDG_DATA_HOME");
        let home = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", home.path());
            env::set_var("XDG_DATA_HOME", xdg.path());
        }

        let report = migrate_v0_1_data(new_root.path()).unwrap();
        assert_eq!(report.moved, 2);
        assert_eq!(report.skipped, 0);

        let think_file = new_root
            .path()
            .join("think")
            .join("sessions")
            .join("alpha.json");
        let ship_file = new_root
            .path()
            .join("ship")
            .join("sessions")
            .join("alpha.json");
        assert!(think_file.exists(), "missing {}", think_file.display());
        assert!(ship_file.exists(), "missing {}", ship_file.display());

        // v0.1 files moved away
        assert!(!deliberate.join("alpha.json").exists());
        assert!(!resolute.join("alpha.json").exists());

        unsafe {
            match prev_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(v) => env::set_var("XDG_DATA_HOME", v),
                None => env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    #[test]
    fn conflict_in_new_root_is_skipped_with_warning() {
        let xdg = TempDir::new().unwrap();
        let new_root = TempDir::new().unwrap();

        let deliberate = xdg.path().join("deliberate-mcp").join("sessions");
        fs::create_dir_all(&deliberate).unwrap();
        fs::write(deliberate.join("alpha.json"), r#"{"v":"old"}"#).unwrap();

        // Pre-existing file in the new layout — migration must NOT clobber it.
        let think_sessions = new_root.path().join("think").join("sessions");
        fs::create_dir_all(&think_sessions).unwrap();
        fs::write(think_sessions.join("alpha.json"), r#"{"v":"new"}"#).unwrap();

        let prev_home = env::var_os("HOME");
        let prev_xdg = env::var_os("XDG_DATA_HOME");
        let home = TempDir::new().unwrap();
        unsafe {
            env::set_var("HOME", home.path());
            env::set_var("XDG_DATA_HOME", xdg.path());
        }

        let report = migrate_v0_1_data(new_root.path()).unwrap();
        assert_eq!(report.moved, 0);
        assert_eq!(report.skipped, 1);
        // Pre-existing content preserved.
        let kept = fs::read_to_string(think_sessions.join("alpha.json")).unwrap();
        assert!(kept.contains("new"));
        // v0.1 file left in place for manual recovery.
        assert!(deliberate.join("alpha.json").exists());

        unsafe {
            match prev_home {
                Some(v) => env::set_var("HOME", v),
                None => env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(v) => env::set_var("XDG_DATA_HOME", v),
                None => env::remove_var("XDG_DATA_HOME"),
            }
        }
    }
}
