//! Project identity derived from the current working directory.
//!
//! Two co-deployed callers in the same directory must produce the same
//! `project_id` so persisted state and live broadcasts correlate.

use std::env;
use std::path::Path;

pub const PROJECT_SEP: &str = "__";

const SHARED_OVERRIDE_VAR: &str = "THINK_AND_SHIP_PROJECT_NAME";

/// The directory a project marks itself with, and the file inside it.
///
/// `.think-and-ship/` already exists in marked repositories — the repo-git trace
/// sink writes there — so this is an anchor that was half-built rather than a
/// new one.
pub const PROJECT_DIR: &str = ".think-and-ship";
pub const PROJECT_FILE: &str = "project.json";

/// Where a project's identity came from. Reported by `status`, because
/// "this is the project" and "this is the project BECAUSE a file says so" are
/// different facts and the difference is what a user needs when one repository
/// answers two ways from two directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdSource {
    /// An environment variable. A user who set one meant it.
    Environment,
    /// A committed `.think-and-ship/project.json`, found by walking up.
    RepoFile,
    /// `<basename>-<fnv1a_6hex(cwd)>` — the answer when nothing declares one.
    DerivedFromPath,
}

impl IdSource {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "the environment",
            Self::RepoFile => "this repository's project file",
            Self::DerivedFromPath => "this directory's path",
        }
    }
}

/// A project's declared identity. IDENTITY ONLY, and that is enforced rather
/// than merely intended.
///
/// This file is COMMITTED, which decides its contents completely. A url, a
/// profile, a token or a tenant in here would be inherited by everyone who
/// clones the repository — the exact false positive `73088dc` gates against,
/// where a colleague who clones a connected project must not be handed a
/// connection they hold no credential for. So the struct has two fields, no
/// `serde(flatten)`, and no map to smuggle anything through: a key added to the
/// file on disk cannot be read back out, because there is nowhere for it to
/// land. How THIS machine reaches the workspace lives in `connections.json` and
/// the credential store, which are per-machine and never committed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectIdentity {
    /// The machine identity every store is keyed by.
    pub id: String,
    /// What to show a human. Optional; absence means "derive it".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Find the identity file by walking UP from `start`, the way git finds `.git`.
///
/// `start` is a parameter rather than a read of the current directory because
/// `env::current_dir` is process-global: a test that changed it would race every
/// other test in the binary, and this lane has already shipped two gates that
/// agreed with whoever ran them instead of testing anything.
#[must_use]
pub fn find_project_file(start: &Path) -> Option<std::path::PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(PROJECT_DIR).join(PROJECT_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Read the declared identity for `start`, if the repository declares one.
#[must_use]
pub fn declared_identity_in(start: &Path) -> Option<ProjectIdentity> {
    let path = find_project_file(start)?;
    let text = std::fs::read_to_string(path).ok()?;
    let declared: ProjectIdentity = serde_json::from_str(&text).ok()?;
    let id = sanitize_project_name(declared.id.trim());
    if id.is_empty() {
        return None;
    }
    Some(ProjectIdentity {
        id,
        name: declared
            .name
            .map(|n| sanitize_project_name(n.trim()))
            .filter(|n| !n.is_empty()),
    })
}

/// Resolve the canonical project identifier.
///
/// Order of precedence:
///   1. `server_env_var` (caller-supplied override).
///   2. `THINK_AND_SHIP_PROJECT_NAME` (shared override).
///   3. `.think-and-ship/project.json`, found by walking up from the cwd.
///   4. `<basename>-<fnv1a_6hex(cwd)>` (deterministic fallback).
///
/// STEP 4 IS LOAD-BEARING, not legacy. Every install that predates the file has
/// no file, and every store they hold is keyed by what step 4 returns — so it
/// must keep returning exactly that, or their whole history orphans on upgrade.
pub fn resolve_project_id(server_env_var: Option<&str>) -> String {
    resolve_project_id_with(server_env_var, env::current_dir().ok().as_deref()).0
}

/// [`resolve_project_id`] with the starting directory supplied, returning WHERE
/// the answer came from as well as what it is.
pub fn resolve_project_id_with(
    server_env_var: Option<&str>,
    cwd: Option<&Path>,
) -> (String, IdSource) {
    if let Some(var) = server_env_var
        && let Ok(raw) = env::var(var)
    {
        let sanitized = sanitize_project_name(raw.trim());
        if !sanitized.is_empty() {
            return (sanitized, IdSource::Environment);
        }
    }

    if let Ok(raw) = env::var(SHARED_OVERRIDE_VAR) {
        let sanitized = sanitize_project_name(raw.trim());
        if !sanitized.is_empty() {
            return (sanitized, IdSource::Environment);
        }
    }

    if let Some(cwd) = cwd {
        let path = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        if let Some(declared) = declared_identity_in(&path) {
            return (declared.id, IdSource::RepoFile);
        }
        return (project_id_for_path(&path), IdSource::DerivedFromPath);
    }

    ("auto".to_string(), IdSource::DerivedFromPath)
}

/// Project id derived purely from a filesystem path (no env overrides):
/// `<basename>-<fnv1a_6hex(path)>`. This is the cwd half of
/// [`resolve_project_id`], exposed so persisted records that carry their
/// recording cwd (e.g. think steps) can be attributed to a project later —
/// the path is hashed as given, NOT canonicalized, because stored cwds were
/// canonicalized at record time and may no longer exist on disk.
pub fn project_id_for_path(path: &Path) -> String {
    let basename = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_project_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "auto".to_string());
    let basename: String = basename.chars().take(24).collect();
    let hash = path_hash6(path);
    format!("{basename}-{hash}")
}

/// The human-readable half of the project's identity: the directory basename
/// with no hash suffix — `think-and-ship`, not `think-and-ship-676f38`.
///
/// This is what outward-facing surfaces (a tracker initiative's name) should
/// show a human; [`resolve_project_id`] stays the machine identity. Honours
/// the same env overrides, because a project that overrode its name meant it
/// everywhere. `None` when nothing usable can be derived — the caller decides
/// what absence means, rather than inheriting an "auto" nobody chose.
pub fn project_display_name() -> Option<String> {
    for var in [SHARED_OVERRIDE_VAR] {
        if let Ok(raw) = env::var(var) {
            let sanitized = sanitize_project_name(raw.trim());
            if !sanitized.is_empty() {
                return Some(sanitized);
            }
        }
    }
    let cwd = env::current_dir().ok()?;
    let path = cwd.canonicalize().unwrap_or(cwd);
    // A repository that declared a display name meant that one, and a
    // subdirectory of it must not answer with its own basename.
    if let Some(declared) = declared_identity_in(&path)
        && let Some(name) = declared.name
    {
        return Some(name);
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(sanitize_project_name)?;
    (!name.is_empty()).then_some(name)
}

/// Write the identity file for `root`, declaring `id`.
///
/// THE ID IS SUPPLIED, NEVER MINTED HERE, and that is the whole safety of this
/// function. Every existing store — think steps, chunks, signals — is keyed by
/// whatever the project resolves to TODAY. Seeding a fresh slug would silently
/// orphan all of it while looking like a tidy-up. Callers pass the
/// already-resolved id, so marking an existing project changes nothing about
/// what it is; it only writes down what was previously recomputed.
///
/// Refuses to overwrite: a project that already declares an identity has one,
/// and a second opinion is how two clones stop agreeing.
pub fn write_project_file(
    root: &Path,
    id: &str,
    name: Option<&str>,
) -> std::io::Result<std::path::PathBuf> {
    let dir = root.join(PROJECT_DIR);
    let path = dir.join(PROJECT_FILE);
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let identity = ProjectIdentity {
        id: id.to_string(),
        name: name.map(str::to_string),
    };
    let body = serde_json::to_string_pretty(&identity)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, format!("{body}\n"))?;
    Ok(path)
}

/// Namespace a caller-supplied session id within the current project.
/// Idempotent: already-prefixed ids pass through unchanged.
pub fn namespace_session_id(project_id: &str, session_id: &str) -> String {
    if session_id.starts_with(project_id) {
        session_id.to_string()
    } else {
        format!("{project_id}{PROJECT_SEP}{session_id}")
    }
}

/// FNV-1a 64-bit, truncated to 24 bits, formatted as 6 hex chars.
/// Deterministic across runs (unlike `DefaultHasher`, which is randomized).
fn path_hash6(path: &Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut h: u64 = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{:06x}", (h & 0xff_ffff) as u32)
}

/// Reduce an arbitrary string to `[a-z0-9_.-]`, collapse runs of replaced
/// chars to a single `-`, trim leading/trailing separators, cap at 32 chars.
pub(crate) fn sanitize_project_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_replace = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.') {
            out.push(c.to_ascii_lowercase());
            last_was_replace = false;
        } else if !last_was_replace && !out.is_empty() {
            out.push('-');
            last_was_replace = true;
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '-' || c == '.');
    let capped: String = trimmed.chars().take(32).collect();
    capped.trim_end_matches(['-', '.']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basic() {
        assert_eq!(sanitize_project_name("My Project!"), "my-project");
    }

    #[test]
    fn sanitize_special_chars() {
        assert_eq!(sanitize_project_name("foo/bar/baz"), "foo-bar-baz");
    }

    #[test]
    fn sanitize_leading_trailing() {
        assert_eq!(sanitize_project_name("--hello--"), "hello");
    }

    #[test]
    fn sanitize_caps_at_32_chars() {
        let long = "a".repeat(64);
        assert_eq!(sanitize_project_name(&long).len(), 32);
    }

    #[test]
    fn hash_is_deterministic() {
        let p = Path::new("/tmp/test-project");
        assert_eq!(path_hash6(p), path_hash6(p));
    }

    #[test]
    fn hash_is_6_hex_chars() {
        let h = path_hash6(Path::new("/some/path"));
        assert_eq!(h.len(), 6);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_differs_for_different_paths() {
        assert_ne!(path_hash6(Path::new("/a")), path_hash6(Path::new("/b")));
    }

    #[test]
    fn namespace_idempotent() {
        let ns = namespace_session_id("my-proj-abc123", "my-proj-abc123__session-1");
        assert_eq!(ns, "my-proj-abc123__session-1");
    }

    #[test]
    fn namespace_prefixes() {
        let ns = namespace_session_id("my-proj-abc123", "session-1");
        assert_eq!(ns, "my-proj-abc123__session-1");
    }

    /// THE MEASURED SPLIT, as a test. In this repository the root answered
    /// `think-and-ship-676f38` and `crates/think-and-ship` answered
    /// `think-and-ship-6353e7` — same repository, one directory down, a
    /// different project with no history and no connection. Worse, the crate
    /// directory shares the repo's basename, so only the hash differed, which
    /// is the least legible form the difference could take.
    #[test]
    fn every_directory_under_a_marked_repo_answers_the_same_project() {
        let repo = tempfile::TempDir::new().unwrap();
        let deep = repo.path().join("crates").join("inner").join("src");
        std::fs::create_dir_all(&deep).unwrap();

        // Unmarked: the split is real, and this is the state every existing
        // install is in.
        let (root_before, src_before) = (
            resolve_project_id_with(None, Some(repo.path())),
            resolve_project_id_with(None, Some(&deep)),
        );
        assert_ne!(
            root_before.0, src_before.0,
            "precondition: without a declaration these are two projects",
        );
        assert_eq!(root_before.1, IdSource::DerivedFromPath);

        // Marked with the id the ROOT already resolves to — never a fresh one.
        write_project_file(repo.path(), &root_before.0, Some("inner-project")).unwrap();

        let (root_after, root_src) = resolve_project_id_with(None, Some(repo.path()));
        let (deep_after, deep_src) = resolve_project_id_with(None, Some(&deep));
        assert_eq!(root_after, deep_after, "one repository is one project");
        assert_eq!(
            root_after, root_before.0,
            "marking an existing project must not change what it is — every store \
             it already holds is keyed by this",
        );
        assert_eq!(root_src, IdSource::RepoFile);
        assert_eq!(deep_src, IdSource::RepoFile);
    }

    /// Two clones at different paths are one project, which is the promise
    /// cloud sync exists to make and could not previously keep.
    #[test]
    fn two_clones_at_different_paths_are_one_project() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        write_project_file(a.path(), "shared-identity", None).unwrap();
        write_project_file(b.path(), "shared-identity", None).unwrap();
        assert_eq!(
            resolve_project_id_with(None, Some(a.path())).0,
            resolve_project_id_with(None, Some(b.path())).0,
        );
    }

    /// The fallback is load-bearing, not legacy: every install that predates
    /// the file has none, and every store they hold is keyed by exactly what
    /// this returns. Changing it orphans their whole history on upgrade.
    #[test]
    fn an_unmarked_directory_resolves_exactly_as_it_always_did() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().canonicalize().unwrap();
        let (id, source) = resolve_project_id_with(None, Some(&path));
        assert_eq!(id, project_id_for_path(&path));
        assert_eq!(source, IdSource::DerivedFromPath);
    }

    /// THE ABSENCE, because this file is COMMITTED. A url, profile, token or
    /// tenant in here would be handed to everyone who clones the repository —
    /// the cloned-repo false positive `73088dc` gates against, where a
    /// colleague must not inherit a connection they hold no credential for.
    ///
    /// Asserted on a file that HAS those keys, read back through the real
    /// parser: they must be unreachable, not merely unwritten. The hazard is a
    /// field somebody adds later, and only a shape with nowhere to put one
    /// forecloses it.
    #[test]
    fn the_committed_identity_file_cannot_carry_a_connection() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(PROJECT_DIR)).unwrap();
        std::fs::write(
            tmp.path().join(PROJECT_DIR).join(PROJECT_FILE),
            r#"{
              "id": "smuggler",
              "name": "Smuggler",
              "cloud_url": "https://api.example",
              "profile": "someone-elses-profile",
              "token": "cloud_tok_do_not_inherit_this",
              "tenant": "acme"
            }"#,
        )
        .unwrap();

        let declared = declared_identity_in(tmp.path()).expect("the identity still parses");
        assert_eq!(declared.id, "smuggler");
        assert_eq!(declared.name.as_deref(), Some("smuggler"));

        // Round-tripping is the proof: what this type can hold is all it can
        // ever hand back, so the smuggled keys are gone rather than ignored.
        let back = serde_json::to_string(&declared).unwrap();
        for forbidden in ["cloud_url", "profile", "token", "tenant", "api.example"] {
            assert!(
                !back.contains(forbidden),
                "a committed file must not be able to carry {forbidden}: {back}",
            );
        }
    }

    /// Marking is not re-marking. A project that already declares an identity
    /// has one, and a second opinion is how two clones stop agreeing.
    #[test]
    fn marking_a_project_twice_does_not_change_its_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_project_file(tmp.path(), "first-answer", None).unwrap();
        write_project_file(tmp.path(), "second-answer", None).unwrap();
        assert_eq!(
            resolve_project_id_with(None, Some(tmp.path())).0,
            "first-answer",
        );
    }
}
