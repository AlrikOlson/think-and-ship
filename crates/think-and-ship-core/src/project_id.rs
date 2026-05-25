use std::env;
use std::path::Path;

pub const PROJECT_SEP: &str = "__";

/// Resolve the canonical project identifier for the current process.
///
/// Checks env vars in order:
///   1. Server-specific override (passed by caller, e.g. `RESOLUTE_PROJECT_NAME`)
///   2. Shared override: `DELIBERATE_PROJECT_NAME`
///   3. Fallback: `<basename>-<fnv1a_6hex(cwd)>`
///
/// The algorithm is identical across all think-and-ship servers so
/// co-deployed servers auto-correlate to the same project.
pub fn resolve_project_id(server_env_var: Option<&str>) -> String {
    if let Some(var) = server_env_var
        && let Ok(raw) = env::var(var)
    {
        let sanitized = sanitize_project_name(raw.trim());
        if !sanitized.is_empty() {
            return sanitized;
        }
    }

    if let Ok(raw) = env::var("DELIBERATE_PROJECT_NAME") {
        let sanitized = sanitize_project_name(raw.trim());
        if !sanitized.is_empty() {
            return sanitized;
        }
    }

    if let Ok(cwd) = env::current_dir() {
        let path = cwd.canonicalize().unwrap_or(cwd.clone());
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(sanitize_project_name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "auto".to_string());
        let basename: String = basename.chars().take(24).collect();
        let hash = path_hash6(&path);
        return format!("{basename}-{hash}");
    }

    "auto".to_string()
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
/// Deterministic across runs (unlike DefaultHasher which is randomized).
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

/// Reduce an arbitrary string to `[a-z0-9_.-]`, collapse runs,
/// trim edges, cap at 32 chars.
fn sanitize_project_name(raw: &str) -> String {
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
    fn namespace_idempotent() {
        let ns = namespace_session_id("my-proj-abc123", "my-proj-abc123__session-1");
        assert_eq!(ns, "my-proj-abc123__session-1");
    }

    #[test]
    fn namespace_prefixes() {
        let ns = namespace_session_id("my-proj-abc123", "session-1");
        assert_eq!(ns, "my-proj-abc123__session-1");
    }
}
