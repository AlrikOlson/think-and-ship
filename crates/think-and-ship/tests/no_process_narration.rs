//! Source gate for docs/STYLE.md S8: no process narration in Rust source.
//!
//! Comments in this codebase carry design arguments — the WHY — and those
//! must reference things a reader can resolve: types, modules, public specs,
//! commit shas. Markers of the project's own development process (internal
//! era numbering, reasoning-trace step references, private tracker ids and
//! URLs, the internal name of the deliberate-breakage verification ritual)
//! resolve to nothing outside this repository, so none may appear in any
//! tracked `.rs` file in the workspace.
//!
//! Every needle below is built with `concat!` so this file cannot satisfy
//! its own scan. Each needle's expected count is pinned at zero: a new file
//! that reintroduces a marker fails the gate instead of being absorbed.
//!
//! Deliberately NOT gated, because each has a legitimate meaning here:
//! `SEP-<n>` (public MCP Spec Enhancement Proposals — citations, like RFCs),
//! `chunk` (the roadmap domain term), `this run` (the current push/sweep
//! execution), `api.linear.app` and `linear.app/oauth` (product endpoints),
//! and small-number wire-format fixtures such as `ext:linear/THI-1`.

use std::fs;
use std::path::{Path, PathBuf};

/// A forbidden marker: a literal prefix and whether it must be followed by
/// an ASCII digit to count (so e.g. a prose word containing the prefix as a
/// substring does not fire where digits are the distinguishing feature).
struct Needle {
    label: &'static str,
    prefix: &'static str,
    /// 0 = the prefix alone is a hit; N = the prefix must be followed by at
    /// least N ASCII digits.
    min_digits: usize,
}

const NEEDLES: &[Needle] = &[
    Needle {
        label: "development phase marker",
        prefix: concat!("Ph", "ase "),
        min_digits: 1,
    },
    Needle {
        label: "development phase marker (hyphenated)",
        prefix: concat!("Ph", "ase-"),
        min_digits: 1,
    },
    Needle {
        label: concat!("development sa", "ga marker"),
        prefix: concat!("sa", "ga"),
        min_digits: 0,
    },
    Needle {
        label: concat!("development sa", "ga marker (capitalized)"),
        prefix: concat!("Sa", "ga"),
        min_digits: 0,
    },
    // Reasoning-trace citations use the real trace's 3+ digit step numbers;
    // wire-format fixtures in tests stay below 100 by convention.
    Needle {
        label: "reasoning-trace step reference",
        prefix: concat!("th", "ink:"),
        min_digits: 3,
    },
    Needle {
        label: "private tracker workspace URL",
        prefix: concat!("lin", "ear.app/te"),
        min_digits: 0,
    },
    Needle {
        label: "internal verification-ritual name",
        prefix: concat!("sab", "otage"),
        min_digits: 0,
    },
    Needle {
        label: "internal verification-ritual name (capitalized)",
        prefix: concat!("Sab", "otage"),
        min_digits: 0,
    },
];

fn workspace_crates_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/think-and-ship; the scan covers every
    // crate in the workspace, not just this one.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate lives inside the workspace crates/ dir")
        .to_path_buf()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Build output is the only directory tree under crates/ that
            // holds .rs files nobody authored.
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn hits_in(text: &str, needle: &Needle) -> Vec<usize> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let mut rest = line;
        let mut found = false;
        while let Some(pos) = rest.find(needle.prefix) {
            let after = &rest[pos + needle.prefix.len()..];
            let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
            if digits >= needle.min_digits {
                found = true;
                break;
            }
            rest = &rest[pos + needle.prefix.len()..];
        }
        if found {
            hits.push(idx + 1);
        }
    }
    hits
}

/// Every needle's count across all workspace Rust sources is pinned at zero.
#[test]
fn narration_markers_absent_from_workspace_source() {
    let mut files = Vec::new();
    collect_rs_files(&workspace_crates_dir(), &mut files);
    assert!(
        files.len() > 100,
        "the scan found only {} .rs files — the walk is broken, which would \
         make this gate pass vacuously",
        files.len()
    );

    let mut offenses = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for needle in NEEDLES {
            for line in hits_in(&text, needle) {
                offenses.push(format!("{}:{line}: {}", file.display(), needle.label));
            }
        }
    }

    assert!(
        offenses.is_empty(),
        "process narration in source (docs/STYLE.md S8) — keep the reason, \
         drop the pointer:\n{}",
        offenses.join("\n")
    );
}

/// The commit-message half is enforced by a tracked hook, not by memory.
/// This pins the hook's existence and its coverage of the same markers.
#[test]
fn commit_msg_hook_present_and_covers_the_markers() {
    let repo_root = workspace_crates_dir()
        .parent()
        .expect("crates/ lives at the repo root")
        .to_path_buf();
    let hook = repo_root.join(".githooks").join("commit-msg");
    let text = fs::read_to_string(&hook)
        .unwrap_or_else(|_| panic!("{} must exist and be readable", hook.display()));

    for marker in [
        concat!("hase", "[- ]"),
        concat!("ag", "a"),
        concat!("hink", ":"),
        concat!("HI", "-"),
        concat!("abot", "age"),
    ] {
        assert!(
            text.contains(marker),
            "{} no longer screens for a marker containing {marker:?}",
            hook.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hook)
            .expect("hook metadata")
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{} must be executable", hook.display());
    }
}
