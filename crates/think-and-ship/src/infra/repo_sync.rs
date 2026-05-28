//! Git-native trace sync (Phase 23b).
//!
//! Mirrors think/ship trace records into the repository under
//! `.think-and-ship/` as a strict superset of the Agent Trace v0.1.0 standard
//! (see `docs/SCHEMA.md`): every line is a valid Agent Trace record, and the
//! full think/ship payload rides in `metadata["dev.thinkandship"]`. One JSONL
//! file per session; `shared` records are committed to git (one commit per
//! session on close), `local` records go to a gitignored partition.
//!
//! This module is the **generic core**. It is parameterised over an opaque
//! [`serde_json::Value`] payload and knows nothing about the think/ship domain
//! types — keeping `infra` at the bottom of the dependency graph (DIP). The
//! domain → record mapping (which `kind`, which `files_touched`) lives in the
//! engines and is wired in Phase 23b2.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use serde_json::{Map, Value, json};
use uuid::Uuid;

const ENV_SYNC_TARGET: &str = "THINK_AND_SHIP_SYNC_TARGET";
const ENV_MODEL_ID: &str = "THINK_AND_SHIP_MODEL_ID";
/// The Agent Trace spec version this envelope conforms to.
const AGENT_TRACE_VERSION: &str = "0.1.0";
/// Reverse-domain extension key carrying our richer semantics.
const EXT_KEY: &str = "dev.thinkandship";
/// Our extension schema version, evolved independently of the Agent Trace one.
const EXT_SCHEMA: &str = "1";
/// In-repo directory holding the trace partitions.
pub const TRACE_DIR: &str = ".think-and-ship";

/// Where trace records are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncTarget {
    /// Per-user XDG persistence only — the v0.2.x default behaviour.
    #[default]
    Local,
    /// Additionally mirror records into the repo's `.think-and-ship/` as
    /// Agent Trace JSONL, committing shared sessions to git.
    RepoGit,
}

impl SyncTarget {
    /// Resolve from `THINK_AND_SHIP_SYNC_TARGET`. Defaults to [`SyncTarget::Local`]
    /// when unset or unrecognised, so misconfiguration never silently enables
    /// repo writes.
    pub fn from_env() -> Self {
        std::env::var(ENV_SYNC_TARGET)
            .map(|v| Self::parse(&v))
            .unwrap_or_default()
    }

    /// Parse a target string. Accepts `repo-git`, `repo_git`, `repogit`, `git`
    /// (case-insensitive) for [`SyncTarget::RepoGit`]; everything else
    /// (including `local` and the empty string) is [`SyncTarget::Local`].
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "repo-git" | "repo_git" | "repogit" | "git" => Self::RepoGit,
            _ => Self::Local,
        }
    }
}

/// Per-record context: the Agent Trace envelope fields the caller supplies.
///
/// Kept as plain owned data so the record builder stays a pure function
/// (unit-testable with fixed values). Production fills it via [`RecordCtx::resolve`].
#[derive(Debug, Clone)]
pub struct RecordCtx {
    /// Unique record id (UUID).
    pub id: String,
    /// RFC 3339 timestamp.
    pub timestamp: String,
    /// Git revision the record was recorded against (HEAD sha; empty if none yet).
    pub revision: String,
    /// `tool.version` — the think-and-ship crate version.
    pub tool_version: String,
    /// models.dev-style `provider/model` id for code attribution, if known.
    pub model_id: Option<String>,
}

impl RecordCtx {
    /// Production constructor: fresh UUID, current time, repo `HEAD`, the crate
    /// version, and the model id from `THINK_AND_SHIP_MODEL_ID` (the server
    /// can't otherwise know the calling model).
    pub fn resolve(repo_root: &Path) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            revision: current_revision(repo_root).unwrap_or_default(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            model_id: std::env::var(ENV_MODEL_ID).ok().filter(|s| !s.is_empty()),
        }
    }

    /// Build one Agent Trace v0.1.0 record wrapping `payload`.
    ///
    /// `family` is `"think"` or `"ship"`; `kind` is `"step" | "objective" |
    /// "task" | "action" | "check"`. `files` is the Agent Trace `files[]`
    /// attribution (build entries with [`file_attribution`]); pass an empty
    /// vec for records that attribute no code.
    pub fn build_record(
        &self,
        family: &str,
        kind: &str,
        session_id: &str,
        shared: bool,
        payload: Value,
        files: Vec<Value>,
    ) -> Value {
        let mut ext = Map::new();
        ext.insert("schema".into(), json!(EXT_SCHEMA));
        ext.insert("family".into(), json!(family));
        ext.insert("kind".into(), json!(kind));
        ext.insert("session_id".into(), json!(session_id));
        ext.insert("shared".into(), json!(shared));
        ext.insert("record".into(), payload);

        let mut metadata = Map::new();
        metadata.insert(EXT_KEY.into(), Value::Object(ext));

        let mut rec = Map::new();
        rec.insert("version".into(), json!(AGENT_TRACE_VERSION));
        rec.insert("id".into(), json!(self.id));
        rec.insert("timestamp".into(), json!(self.timestamp));
        rec.insert(
            "vcs".into(),
            json!({ "type": "git", "revision": self.revision }),
        );
        rec.insert(
            "tool".into(),
            json!({ "name": "think-and-ship", "version": self.tool_version }),
        );
        rec.insert("files".into(), Value::Array(files));
        rec.insert("metadata".into(), Value::Object(metadata));
        Value::Object(rec)
    }
}

/// One Agent Trace `files[]` attribution entry for `path`, crediting the AI
/// contributor. Whole-file range (`1..=1`) is used as a placeholder when
/// precise line ranges aren't known — Agent Trace permits whole-file
/// attribution; 23c can refine with `content_hash`.
pub fn file_attribution(path: &str, model_id: Option<&str>) -> Value {
    let mut contributor = Map::new();
    contributor.insert("type".into(), json!("ai"));
    if let Some(m) = model_id {
        contributor.insert("model_id".into(), json!(m));
    }
    json!({
        "path": path,
        "conversations": [{
            "contributor": Value::Object(contributor),
            "ranges": [{ "start_line": 1, "end_line": 1 }],
        }],
    })
}

/// Discover the git repository root containing `cwd` via
/// `git rev-parse --show-toplevel`. Returns `None` when `cwd` is not inside a
/// git repository or git is unavailable — the caller then falls back to
/// [`SyncTarget::Local`].
pub fn discover_repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8(out.stdout).ok()?;
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

/// Current `HEAD` revision of the repo, or `None` if there are no commits yet
/// (or git is unavailable).
pub fn current_revision(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rev = String::from_utf8(out.stdout).ok()?;
    let rev = rev.trim();
    if rev.is_empty() {
        None
    } else {
        Some(rev.to_string())
    }
}

/// Writes Agent Trace JSONL into a repository's `.think-and-ship/` partitions
/// and commits shared sessions.
pub struct RepoSink {
    repo_root: PathBuf,
}

impl RepoSink {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }

    fn trace_dir(&self) -> PathBuf {
        self.repo_root.join(TRACE_DIR)
    }

    /// Partition subdirectory name for a given sharedness.
    fn partition(shared: bool) -> &'static str {
        if shared { "sessions" } else { "local" }
    }

    /// Repo-relative path to a session's JSONL file (forward slashes; used as a
    /// git pathspec).
    fn rel_session_path(session_id: &str, shared: bool) -> PathBuf {
        Path::new(TRACE_DIR)
            .join(Self::partition(shared))
            .join(format!("{session_id}.jsonl"))
    }

    /// Ensure `.think-and-ship/.gitignore` excludes the `local/` partition.
    /// Idempotent.
    fn ensure_gitignore(&self) -> std::io::Result<()> {
        let dir = self.trace_dir();
        std::fs::create_dir_all(&dir)?;
        let gi = dir.join(".gitignore");
        let current = std::fs::read_to_string(&gi).unwrap_or_default();
        if !current.lines().any(|l| l.trim() == "local/") {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&gi)?;
            f.write_all(b"local/\n")?;
        }
        Ok(())
    }

    /// Append one record as a JSONL line to the session's partition file,
    /// creating the partition directory and `.gitignore` on first use. Returns
    /// the file written to.
    pub fn append(
        &self,
        session_id: &str,
        shared: bool,
        record: &Value,
    ) -> std::io::Result<PathBuf> {
        self.ensure_gitignore()?;
        let dir = self.trace_dir().join(Self::partition(shared));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        Ok(path)
    }

    /// Stage and commit a shared session's JSONL file (plus the partition
    /// `.gitignore`) as a single commit. No-op for sessions with no committable
    /// changes (returns `Ok(false)`). Local sessions are never committed —
    /// pass only shared session ids here.
    pub fn commit_session(&self, session_id: &str) -> std::io::Result<bool> {
        let rel = Self::rel_session_path(session_id, true);
        let gitignore = Path::new(TRACE_DIR).join(".gitignore");

        // Stage our paths (needed so newly-created files become committable).
        let add = self.git(["add", "--"]).arg(&rel).arg(&gitignore).output()?;
        if !add.status.success() {
            return Err(std::io::Error::other(format!(
                "git add failed: {}",
                String::from_utf8_lossy(&add.stderr)
            )));
        }

        // Commit only our paths, so unrelated staged work isn't swept in.
        let msg = format!("chore(trace): session {session_id}");
        let commit = self
            .git(["commit", "-m", &msg, "--"])
            .arg(&rel)
            .arg(&gitignore)
            .output()?;
        if commit.status.success() {
            return Ok(true);
        }
        let stdout = String::from_utf8_lossy(&commit.stdout);
        let stderr = String::from_utf8_lossy(&commit.stderr);
        if stdout.contains("nothing to commit") || stderr.contains("nothing to commit") {
            Ok(false)
        } else {
            Err(std::io::Error::other(format!(
                "git commit failed: {stdout}{stderr}"
            )))
        }
    }

    /// `git -C <repo_root> <args...>` — base command other calls extend.
    fn git<const N: usize>(&self, args: [&str; N]) -> Command {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.repo_root).args(args);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    // ── SyncTarget parsing ───────────────────────────────────────────────

    #[test]
    fn sync_target_parse_variants() {
        assert_eq!(SyncTarget::parse("repo-git"), SyncTarget::RepoGit);
        assert_eq!(SyncTarget::parse("repo_git"), SyncTarget::RepoGit);
        assert_eq!(SyncTarget::parse("RepoGit"), SyncTarget::RepoGit);
        assert_eq!(SyncTarget::parse("  git "), SyncTarget::RepoGit);
        assert_eq!(SyncTarget::parse("local"), SyncTarget::Local);
        assert_eq!(SyncTarget::parse(""), SyncTarget::Local);
        assert_eq!(SyncTarget::parse("nonsense"), SyncTarget::Local);
        assert_eq!(SyncTarget::default(), SyncTarget::Local);
    }

    fn ctx() -> RecordCtx {
        RecordCtx {
            id: "11111111-1111-1111-1111-111111111111".into(),
            timestamp: "2026-05-28T19:30:00Z".into(),
            revision: "de31806".into(),
            tool_version: "0.3.0".into(),
            model_id: Some("anthropic/claude-opus-4-8".into()),
        }
    }

    // ── Record envelope ──────────────────────────────────────────────────

    #[test]
    fn build_record_is_valid_agent_trace_envelope() {
        let payload = json!({ "step_number": 64, "purpose": "x" });
        let rec = ctx().build_record("think", "step", "proj-abc", true, payload.clone(), vec![]);

        // Agent Trace required fields present + correct.
        assert_eq!(rec["version"], "0.1.0");
        assert_eq!(rec["id"], "11111111-1111-1111-1111-111111111111");
        assert_eq!(rec["timestamp"], "2026-05-28T19:30:00Z");
        assert_eq!(rec["vcs"]["type"], "git");
        assert_eq!(rec["vcs"]["revision"], "de31806");
        assert_eq!(rec["tool"]["name"], "think-and-ship");
        assert_eq!(rec["tool"]["version"], "0.3.0");
        assert!(rec["files"].as_array().unwrap().is_empty());

        // Our extension carries the verbatim payload + metadata.
        let ext = &rec["metadata"]["dev.thinkandship"];
        assert_eq!(ext["schema"], "1");
        assert_eq!(ext["family"], "think");
        assert_eq!(ext["kind"], "step");
        assert_eq!(ext["session_id"], "proj-abc");
        assert_eq!(ext["shared"], true);
        assert_eq!(ext["record"], payload);

        // The whole record serializes as one line (JSONL-ready).
        let line = serde_json::to_string(&rec).unwrap();
        assert!(!line.contains('\n'));
    }

    #[test]
    fn file_attribution_credits_ai_with_optional_model() {
        let with = file_attribution("src/app.rs", Some("anthropic/claude-opus-4-8"));
        assert_eq!(with["path"], "src/app.rs");
        let contrib = &with["conversations"][0]["contributor"];
        assert_eq!(contrib["type"], "ai");
        assert_eq!(contrib["model_id"], "anthropic/claude-opus-4-8");
        assert_eq!(with["conversations"][0]["ranges"][0]["start_line"], 1);

        let without = file_attribution("src/app.rs", None);
        assert!(
            without["conversations"][0]["contributor"]
                .get("model_id")
                .is_none()
        );
    }

    #[test]
    fn ship_code_action_record_carries_files_attribution() {
        let payload = json!({ "id": 2, "type": "code", "files_touched": ["Cargo.toml"] });
        let files = vec![file_attribution(
            "Cargo.toml",
            Some("anthropic/claude-opus-4-8"),
        )];
        let rec = ctx().build_record("ship", "action", "proj-abc", true, payload, files);
        assert_eq!(rec["files"][0]["path"], "Cargo.toml");
        assert_eq!(rec["metadata"]["dev.thinkandship"]["family"], "ship");
    }

    // ── Partition routing + append ───────────────────────────────────────

    #[test]
    fn append_routes_shared_and_local_partitions() {
        let tmp = TempDir::new().unwrap();
        let sink = RepoSink::new(tmp.path());
        let rec = ctx().build_record("think", "step", "s1", true, json!({}), vec![]);

        let shared_path = sink.append("s1", true, &rec).unwrap();
        let local_path = sink.append("s1", false, &rec).unwrap();

        assert!(shared_path.ends_with(".think-and-ship/sessions/s1.jsonl"));
        assert!(local_path.ends_with(".think-and-ship/local/s1.jsonl"));

        // Append again to the shared file → two lines.
        sink.append("s1", true, &rec).unwrap();
        let body = std::fs::read_to_string(&shared_path).unwrap();
        assert_eq!(body.lines().count(), 2);
        // Each line is independently parseable JSON.
        for line in body.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["version"], "0.1.0");
        }
    }

    #[test]
    fn ensure_gitignore_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let sink = RepoSink::new(tmp.path());
        sink.append(
            "s1",
            false,
            &ctx().build_record("think", "step", "s1", false, json!({}), vec![]),
        )
        .unwrap();
        sink.append(
            "s2",
            false,
            &ctx().build_record("think", "step", "s2", false, json!({}), vec![]),
        )
        .unwrap();
        let gi = std::fs::read_to_string(tmp.path().join(".think-and-ship/.gitignore")).unwrap();
        assert_eq!(gi.matches("local/").count(), 1, "no duplicate local/ rule");
    }

    // ── Real-git integration ─────────────────────────────────────────────

    fn git(repo: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn init_repo(repo: &Path) {
        assert!(git(repo, &["init", "-q"]).status.success());
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "user.name", "Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
    }

    #[test]
    fn discover_repo_root_finds_initialised_repo() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let found = discover_repo_root(tmp.path()).expect("should discover repo root");
        // git canonicalises symlinks (e.g. /var → /private/var on macOS); compare canonical forms.
        assert_eq!(
            found.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn commit_session_makes_one_commit_and_gitignores_local() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let sink = RepoSink::new(tmp.path());
        let c = ctx();

        // Two shared records + one local record in session s1.
        sink.append(
            "s1",
            true,
            &c.build_record("think", "step", "s1", true, json!({"n": 1}), vec![]),
        )
        .unwrap();
        sink.append(
            "s1",
            true,
            &c.build_record("ship", "action", "s1", true, json!({"n": 2}), vec![]),
        )
        .unwrap();
        sink.append(
            "s1",
            false,
            &c.build_record("think", "step", "s1", false, json!({"n": 3}), vec![]),
        )
        .unwrap();

        // Commit on session close.
        assert!(
            sink.commit_session("s1").unwrap(),
            "first commit should happen"
        );

        // Exactly one commit touching .think-and-ship/.
        let log = git(tmp.path(), &["log", "--oneline", "--", ".think-and-ship/"]);
        let lines = String::from_utf8_lossy(&log.stdout);
        assert_eq!(lines.lines().count(), 1, "one commit per session: {lines}");

        // Shared file is tracked; local file is NOT (gitignored).
        let tracked = git(tmp.path(), &["ls-files", ".think-and-ship/"]);
        let tracked = String::from_utf8_lossy(&tracked.stdout);
        assert!(
            tracked.contains("sessions/s1.jsonl"),
            "shared tracked: {tracked}"
        );
        assert!(
            tracked.contains(".gitignore"),
            "gitignore tracked: {tracked}"
        );
        assert!(
            !tracked.contains("local/"),
            "local must not be tracked: {tracked}"
        );

        // git agrees local/ is ignored.
        let ignored = git(
            tmp.path(),
            &["check-ignore", ".think-and-ship/local/s1.jsonl"],
        );
        assert!(ignored.status.success(), "local/ must be gitignored");

        // Line counts survived the round-trip.
        let shared =
            std::fs::read_to_string(tmp.path().join(".think-and-ship/sessions/s1.jsonl")).unwrap();
        let local =
            std::fs::read_to_string(tmp.path().join(".think-and-ship/local/s1.jsonl")).unwrap();
        assert_eq!(shared.lines().count(), 2);
        assert_eq!(local.lines().count(), 1);

        // No new appends → nothing to commit.
        assert!(
            !sink.commit_session("s1").unwrap(),
            "second commit is a no-op"
        );
    }

    #[test]
    fn current_revision_after_commit_is_some() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        assert!(current_revision(tmp.path()).is_none(), "no commits yet");
        let sink = RepoSink::new(tmp.path());
        sink.append(
            "s1",
            true,
            &ctx().build_record("think", "step", "s1", true, json!({}), vec![]),
        )
        .unwrap();
        sink.commit_session("s1").unwrap();
        assert!(
            current_revision(tmp.path()).is_some(),
            "HEAD exists after commit"
        );
    }
}
