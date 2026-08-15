//! `init` / `doctor` / `status` CLI commands — project setup for the unified
//! server. Mirrors the npm `cli.js` installer so the `cargo install`
//! and `npm install -g` paths agree on the config they write, extended with the
//! `roadmap` data partition the npm wrapper predates.
//!
//! The MCP config written is `command: "think-and-ship"` (PATH-resolved) with
//! `THINK_AND_SHIP_PERSIST=true` baked in — persistence is off by default, and a
//! roadmap/think/ship server that forgets its state between sessions is the most
//! common setup mistake.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const MCP_SERVER_NAME: &str = "think-and-ship";
const CLAUDE_MD_MARKER: &str = "<!-- think-and-ship -->";
/// Closes the generated section so a force-replace knows where OUR text stops.
///
/// Without it the section has a start and no end, and "replace" can only mean
/// "keep what is above the marker" — which silently deletes every rule the user
/// wrote below it. CLAUDE.md is exactly the file people append their own rules
/// to, so that is the tail most likely to be lost.
const CLAUDE_MD_END_MARKER: &str = "<!-- /think-and-ship -->";

/// Which JSON key holds the server map in a host's MCP config.
///
/// Not cosmetic. VS Code reads `.vscode/mcp.json` with its servers under
/// `servers`, while Claude Code, Cursor and Windsurf use `mcpServers`. An entry
/// written under the wrong key is perfectly valid JSON that the host simply
/// never sees — the quietest possible way for "Connected" to be a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerContainer {
    /// `mcpServers` — Claude Code, Cursor, Windsurf.
    McpServers,
    /// `servers` — VS Code's `.vscode/mcp.json`.
    Servers,
}

impl ServerContainer {
    /// The JSON key this container occupies.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::McpServers => "mcpServers",
            Self::Servers => "servers",
        }
    }
}

/// A host whose MCP config we know how to read, and sometimes to author.
struct HostTarget {
    name: &'static str,
    /// Marker directory that signals this host is in use.
    ///
    /// EVERY host has one. It used to be `Option`, with Claude Code carrying
    /// `None` and reachable only when no other marker existed — a client
    /// selected by the ABSENCE of the others, which is no evidence at all.
    dir: &'static str,
    /// Environment variables this host sets in the environment of any command it
    /// runs. Positive identity: the caller saying who it is, rather than the
    /// repository being asked to guess on its behalf.
    ///
    /// Empty means the host publishes no marker we could confirm. Windsurf is in
    /// that position — neither its documentation nor its forum names one — so it
    /// is identified by its directory alone, and this stays empty rather than
    /// holding a variable nobody sets.
    env_markers: &'static [&'static str],
    config_file: &'static str,
    container: ServerContainer,
    /// Whether `init` / `connect` may AUTHOR an entry here for a host that is
    /// merely present.
    ///
    /// VS Code is deliberately excluded from authoring. A `.vscode/` directory
    /// exists in a large share of repositories that never touch VS Code's agent
    /// mode, so treating it as an authoring signal would move `init`'s output for
    /// those projects. FINDING and updating an entry there is a different act
    /// with a lower bar, and that is the one `connect` needs — plus, now, saying
    /// out loud that VS Code is here and needs a hand.
    authorable: bool,
}

/// Every client we know how to configure, each with a positive marker.
///
/// `TERM_PROGRAM` appears nowhere in this table on purpose. It names the
/// TERMINAL, not the agent — measured on the machine this was written on, a
/// Claude Code session reports `TERM_PROGRAM=rio`, the emulator it happened to
/// be launched from. And `TERM_PROGRAM=vscode` is a FAMILY marker that Cursor
/// and Windsurf both inherit by being VS Code forks, so it cannot pick a client
/// even when it is present.
const HOST_TARGETS: &[HostTarget] = &[
    HostTarget {
        name: "Cursor",
        dir: ".cursor",
        // Documented by Cursor's own forum as the way to detect its integrated
        // terminal; `CURSOR_CLI` is set for its agent terminal.
        env_markers: &["CURSOR_TRACE_ID", "CURSOR_CLI"],
        config_file: ".cursor/mcp.json",
        container: ServerContainer::McpServers,
        authorable: true,
    },
    HostTarget {
        name: "Windsurf",
        dir: ".windsurf",
        env_markers: &[],
        config_file: ".windsurf/mcp.json",
        container: ServerContainer::McpServers,
        authorable: true,
    },
    HostTarget {
        name: "VS Code",
        dir: ".vscode",
        env_markers: &[],
        config_file: ".vscode/mcp.json",
        container: ServerContainer::Servers,
        authorable: false,
    },
    HostTarget {
        name: "Claude Code",
        // Claude Code writes its own project settings here, so this is a real
        // first-party marker rather than a stand-in for "nothing else matched".
        dir: ".claude",
        env_markers: &[
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SESSION_ID",
        ],
        config_file: ".mcp.json",
        container: ServerContainer::McpServers,
        authorable: true,
    },
];

/// The host authored into when NOTHING identifies a client — not because the
/// others are absent, but because a portable `.mcp.json` is the documented
/// default and something has to be written.
///
/// Named, and announced to the user as a default, so it can never be mistaken
/// for a detection. That distinction is the whole difference between this and
/// the `rfind` it replaced.
const DEFAULT_AUTHORING_HOST: &str = "Claude Code";

struct ProjectType {
    name: &'static str,
    marker: &'static str,
    verify: &'static [&'static str],
}

const PROJECT_TYPES: &[ProjectType] = &[
    ProjectType {
        name: "Rust",
        marker: "Cargo.toml",
        verify: &["cargo test", "cargo clippy --all-targets -- -D warnings"],
    },
    ProjectType {
        name: "Node",
        marker: "package.json",
        verify: &["npm test", "npm run lint"],
    },
    ProjectType {
        name: "Python",
        marker: "pyproject.toml",
        verify: &["pytest", "ruff check"],
    },
    ProjectType {
        name: "Python",
        marker: "setup.py",
        verify: &["pytest", "ruff check"],
    },
    ProjectType {
        name: "Go",
        marker: "go.mod",
        verify: &["go test ./...", "go vet ./..."],
    },
];

/// The local server entry written into the IDE's MCP config by `init`:
/// persistence on, no cloud sync.
fn mcp_server_config() -> Value {
    json!({
        "command": "think-and-ship",
        "args": ["serve"],
        "env": { "THINK_AND_SHIP_PERSIST": "true" }
    })
}

/// The cloud-configured server entry written by `connect`: persistence on plus
/// write-through cloud sync (`SYNC_TARGET=cloud` + the per-tenant URL + the name
/// of the profile holding the token).
///
/// `SYNC_TARGET` is what actually arms write-through — a resolvable token alone
/// is inert (see `cli::build_unified`).
///
/// NO SECRET GOES IN HERE, and that is the contract this function exists to
/// keep. It used to write the agent token as `THINK_AND_SHIP_CLOUD_TOKEN`, into
/// a file that is variously committed to a repo (`.mcp.json`,
/// `.cursor/mcp.json`), synced out of a home directory (`~/.claude.json`), and
/// pasted into support threads. What it writes now is a profile NAME, which is
/// not a secret and is meaningless without the credential store — see
/// [`crate::cloud::credential`]. The server resolves the name at startup.
fn cloud_server_config(cloud_url: &str, profile: &str) -> Value {
    let mut server = json!({
        "command": "think-and-ship",
        "args": ["serve"],
        "env": {
            "THINK_AND_SHIP_PERSIST": "true",
            "THINK_AND_SHIP_SYNC_TARGET": "cloud",
            "THINK_AND_SHIP_CLOUD_URL": cloud_url,
        }
    });
    // Inserted rather than written as a literal so the key can only ever be the
    // one the server reads back. A literal here and a renamed constant there is
    // exactly the drift that would leave connect writing a name nobody resolves.
    server["env"][crate::cloud::credential::PROFILE_ENV] = json!(profile);
    server
}

/// Which clients the CALLER's own environment names, as a value.
///
/// The environment arrives as data rather than being read where it is used, for
/// the reason the previous chunk learned the hard way: a rule reachable only by
/// mutating process-global environment is a rule no test can hold still while
/// its neighbours run, so the gate ends up exercising the rule and never the
/// wiring. [`Self::from_env`] is the ONLY ambient read in this lane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerEnv {
    /// Host names (from [`HOST_TARGETS`]) whose markers are set, in table order.
    claims: Vec<&'static str>,
}

impl CallerEnv {
    /// The ONLY ambient environment read in this lane.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// [`Self::from_env`] over a supplied lookup, so a test can drive an
    /// environment this process has never held.
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            claims: HOST_TARGETS
                .iter()
                .filter(|t| {
                    t.env_markers
                        .iter()
                        .any(|k| lookup(k).is_some_and(|v| !v.trim().is_empty()))
                })
                .map(|t| t.name)
                .collect(),
        }
    }

    /// An environment that names nobody — the honest state of a plain shell.
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Add clients the USER named, for the case detection cannot cover.
    ///
    /// Windsurf publishes no environment marker we could find, and a client we
    /// have never heard of publishes nothing by definition — so the general
    /// answer beats the specific one: let the caller say who it is.
    ///
    /// It ADDS rather than replaces, because the claims are evidence and more
    /// evidence is not less. `--client windsurf` in a repository that also has
    /// `.cursor/` configures both, which is the same rule every other signal
    /// here obeys.
    ///
    /// An unrecognised name is refused with the list that works. A flag that is
    /// silently ignored is worse than no flag: the user believes they have
    /// solved the problem and the symptom is unchanged.
    pub fn naming(mut self, clients: &[String]) -> Result<Self> {
        for asked in clients {
            let Some(target) = HOST_TARGETS
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(asked.trim()))
            else {
                let known = HOST_TARGETS
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("unknown client '{asked}'. The clients this can configure are: {known}.");
            };
            if !self.claims.contains(&target.name) {
                self.claims.push(target.name);
            }
        }
        // Table order is what every other consumer expects; pushing appends.
        self.claims
            .sort_by_key(|name| HOST_TARGETS.iter().position(|t| t.name == *name));
        Ok(self)
    }

    /// An environment in which exactly these variables are set. For tests in
    /// other modules that need to drive a caller identity this process does not
    /// have, without touching the process environment every other test shares.
    #[cfg(test)]
    #[must_use]
    pub fn from_env_for_test(vars: &[&str]) -> Self {
        let set: Vec<String> = vars.iter().map(|v| (*v).to_string()).collect();
        Self::from_lookup(|key| set.iter().find(|v| *v == key).map(|_| "1".to_string()))
    }

    fn names(&self, host: &str) -> bool {
        self.claims.contains(&host)
    }
}

/// The single client that RAN this command, when its environment says so.
///
/// Two markers can be set at once — `claude` launched from Cursor's integrated
/// terminal has both — and the innermost one wins: `CLAUDECODE` is set by the
/// process actually executing this command, while Cursor's marker is inherited
/// from the terminal session hosting it. Claude Code is last in
/// [`HOST_TARGETS`], so the last claim is the innermost.
fn identify_caller(env: &CallerEnv) -> Option<&'static HostTarget> {
    HOST_TARGETS.iter().rfind(|t| env.names(t.name))
}

/// EVERY client this project shows positive evidence of: the caller's own
/// environment, plus each host whose marker directory is here.
///
/// A union, never a first match, and never an absence. The old rule returned the
/// FIRST authorable host with a marker dir, which forced a wrong answer whenever
/// two clients existed — the normal case — and could not name Claude Code at all
/// except by everything else being missing.
fn present_hosts_in(cwd: &Path, env: &CallerEnv) -> Vec<&'static HostTarget> {
    HOST_TARGETS
        .iter()
        .filter(|t| env.names(t.name) || cwd.join(t.dir).is_dir())
        .collect()
}

/// The host authored into when nothing is present. See [`DEFAULT_AUTHORING_HOST`].
fn default_authoring_host() -> &'static HostTarget {
    HOST_TARGETS
        .iter()
        .find(|t| t.name == DEFAULT_AUTHORING_HOST)
        .expect("the default authoring host is in HOST_TARGETS")
}

/// Every client `init` / `connect` should author an entry for.
///
/// The present authorable clients — all of them — or the named default when the
/// project shows no evidence of any client at all.
fn authoring_hosts_in(cwd: &Path, env: &CallerEnv) -> Vec<&'static HostTarget> {
    let present: Vec<&'static HostTarget> = present_hosts_in(cwd, env)
        .into_iter()
        .filter(|t| t.authorable)
        .collect();
    if present.is_empty() {
        vec![default_authoring_host()]
    } else {
        present
    }
}

/// Whether [`authoring_hosts_in`] is answering with the DEFAULT rather than with
/// something it detected. Kept separate so every surface can say which of the
/// two it is doing — a default announced as a detection is the lie this chunk
/// exists to remove.
fn authoring_is_default_in(cwd: &Path, env: &CallerEnv) -> bool {
    !present_hosts_in(cwd, env).iter().any(|t| t.authorable)
}

/// First project type whose marker file exists, if any.
fn detect_project(cwd: &Path) -> Option<&'static ProjectType> {
    PROJECT_TYPES.iter().find(|pt| cwd.join(pt.marker).exists())
}

fn read_json_safe(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The XDG data root the server persists under (respects `THINK_AND_SHIP_DATA_DIR`).
fn data_root() -> PathBuf {
    crate::infra::PersistenceConfig::from_env().data_dir
}

fn generate_claude_md(project: Option<&ProjectType>) -> String {
    let verify_block = project
        .map(|p| {
            let cmds = p
                .verify
                .iter()
                .map(|c| format!("- `{c}`"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "\n## Verification\n\nThis is a {} project. Use these commands to verify changes:\n\n{cmds}\n",
                p.name
            )
        })
        .unwrap_or_default();

    format!(
        "{CLAUDE_MD_MARKER}
# think-and-ship

One MCP server is configured: **think-and-ship**, exposing four tool families.

## Tool families

| Family | Purpose | Key tools |
|--------|---------|-----------|
| **think_*** | Reasoning trace: record steps, branch hypotheses, pin conclusions | `think_record_step`, `think_pin_step`, `think_trace_checkpoint` |
| **ship_*** | Execution trace: objectives, tasks, actions, quality gates | `ship_set_objective`, `ship_plan`, `ship_start`, `ship_record`, `ship_check`, `ship_finalize` |
| **roadmap_*** | The long-horizon plan-of-plans driving both | `roadmap_status`, `roadmap_next`, `roadmap_start_chunk`, `roadmap_complete_chunk`, `roadmap_export` |
| **signal_*** | Stakeholder signals: capture, research, surface, promote to the roadmap | `signal_capture`, `signal_pending`, `signal_research`, `signal_promote` |

## Cross-referencing

Link reasoning to execution:
- On `think_record_step`, pass `execution_ref: \"task:<id>\"` to point at a ship_* task.
- On `ship_record`, pass `think_step: <N>` to point back at the motivating think_* step.

Both halves resolve the same project identity from the working directory so
traces auto-correlate.

## Quick-start workflow

1. `ship_set_objective` — define the goal
2. `ship_plan` — break into tasks
3. `think_record_step` — record your reasoning (open)
4. `ship_start` → `ship_record` → `ship_complete` — do the work
5. `ship_check` — record test/lint results
6. `ship_finalize` — finalize the objective
7. `think_record_step` — record outcome (close)
{verify_block}{CLAUDE_MD_END_MARKER}
"
    )
}

/// Where the generated section starts and stops inside an existing CLAUDE.md.
///
/// `Some((start, end))` are byte offsets bounding the whole block, marker to
/// marker, so a caller can splice new text in and keep BOTH sides. A file
/// written before the end marker existed has no closing bound: the block is
/// taken to run to the next top-level heading, because the generated section
/// contains exactly one `# ` heading of its own and anything after a second one
/// is somebody else's writing. No further heading means the section really does
/// run to the end of the file — the shape `init` creates from scratch.
fn claude_md_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(CLAUDE_MD_MARKER)?;
    if let Some(end) = text[start..].find(CLAUDE_MD_END_MARKER) {
        return Some((start, start + end + CLAUDE_MD_END_MARKER.len()));
    }
    // Legacy, unterminated section: bound it at the next top-level heading.
    let body = &text[start..];
    let after_own_heading = body.find("\n# ").map(|i| i + 1).unwrap_or(body.len());
    let next = body[after_own_heading..]
        .find("\n# ")
        .map(|i| after_own_heading + i + 1)
        .unwrap_or(body.len());
    Some((start, start + next))
}

/// What declaring an identity in a given directory would do, as DATA.
///
/// A rule stated as a return value can be tested; a rule stated as a `println!`
/// inside a command can only be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkPlan {
    /// Nothing declares an identity for this directory yet. Writing one records
    /// the id it already resolves to.
    Declare { id: String, at: PathBuf },
    /// This very directory already declares one. Marking twice is not
    /// re-marking: a project that has an identity has one, and a second opinion
    /// is how two clones stop agreeing.
    AlreadyDeclared { id: String, at: PathBuf },
    /// An ANCESTOR declares one. Resolution walks up and takes the NEAREST
    /// declaration, so writing here would shadow the ancestor's for this subtree
    /// — turning one repository back into two projects, which is the exact split
    /// the identity file exists to close.
    DeclaredByAncestor { id: String, at: PathBuf },
}

impl MarkPlan {
    /// The id this directory answers to either way.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Declare { id, .. }
            | Self::AlreadyDeclared { id, .. }
            | Self::DeclaredByAncestor { id, .. } => id,
        }
    }

    /// The file that holds — or would hold — the declaration.
    #[must_use]
    pub fn at(&self) -> &Path {
        match self {
            Self::Declare { at, .. }
            | Self::AlreadyDeclared { at, .. }
            | Self::DeclaredByAncestor { at, .. } => at,
        }
    }
}

/// Decide what marking `root` would do, without touching the filesystem.
///
/// `root` is a parameter rather than a read of the current directory: the
/// process cwd is global, and a test that changed it would race every other test
/// in the binary.
#[must_use]
pub fn plan_mark(root: &Path) -> MarkPlan {
    let (id, _) = crate::infra::resolve_project_id_with(None, Some(root));
    let own = root
        .join(crate::infra::PROJECT_DIR)
        .join(crate::infra::PROJECT_FILE);
    match crate::infra::find_project_file(root) {
        Some(found) if found == own => MarkPlan::AlreadyDeclared { id, at: found },
        Some(found) => MarkPlan::DeclaredByAncestor { id, at: found },
        None => MarkPlan::Declare { id, at: own },
    }
}

/// Declare `root`'s identity and touch nothing else.
///
/// The id is the one `root` ALREADY resolves to, handed straight to
/// [`crate::infra::write_project_file`], which never mints one. Marking an
/// existing project therefore changes nothing about what it is — it only writes
/// down what was previously recomputed from the path.
///
/// The single file this writes is the whole of its effect. No MCP entry, no
/// CLAUDE.md, no client configuration: declaring who a repository is and routing
/// a machine to a workspace are different acts, and wanting the first was never
/// a reason to accept the second.
pub fn mark_in(root: &Path, name: Option<&str>, dry_run: bool) -> Result<MarkPlan> {
    let plan = plan_mark(root);
    if let MarkPlan::Declare { id, .. } = &plan
        && !dry_run
    {
        crate::infra::write_project_file(root, id, name)
            .with_context(|| format!("declaring {id} in {}", root.display()))?;
    }
    Ok(plan)
}

/// `think-and-ship project mark` — declare this repository's identity.
pub fn project_mark(name: Option<&str>, dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let version = env!("CARGO_PKG_VERSION");
    println!("think-and-ship project mark v{version}\n");

    let plan = mark_in(&cwd, name, dry_run)?;
    match &plan {
        MarkPlan::Declare { id, at } => {
            if dry_run {
                println!("  Would declare: {id}");
                println!("           in {}", at.display());
            } else {
                println!("  Project id: {id}");
                println!("           declared in {}", at.display());
            }
            println!(
                "\n  Commit it — it is what keeps this project itself when the directory moves,"
            );
            println!("  and what makes a subdirectory answer the same project as the root.");
            println!("\n  Nothing else was written. `think-and-ship init` configures your editor.");
        }
        MarkPlan::AlreadyDeclared { id, at } => {
            println!("  Project id: {id} (already declared in {})", at.display());
            println!("\n  Nothing to do. An identity is declared once; a second answer is how");
            println!("  two clones of one repository stop agreeing.");
        }
        MarkPlan::DeclaredByAncestor { id, at } => {
            println!("  Project id: {id} (declared in {})", at.display());
            bail!(
                "this directory is already inside a declared project ({id}, from {}).\n\
                 Declaring another identity here would make this subtree a separate project — \
                 which is what the declaration exists to prevent. Nothing was written.",
                at.display(),
            );
        }
    }
    Ok(())
}

/// `think-and-ship init` — write the IDE MCP config (+ optional CLAUDE.md).
pub fn init(with_claude_md: bool, full: bool, dry_run: bool, force: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let with_claude_md = with_claude_md || full;
    let version = env!("CARGO_PKG_VERSION");
    println!("think-and-ship init v{version}\n");

    let env = CallerEnv::from_env();
    let hosts = authoring_hosts_in(&cwd, &env);
    let project = detect_project(&cwd);

    // Mark this repository's identity, with the id it ALREADY has.
    //
    // Nothing is minted here. Every store this project holds — think steps,
    // chunks, signals — is keyed by whatever it resolves to today, and writing
    // down a fresh slug would orphan all of it while looking like tidying up.
    // What the file changes is not WHO this project is but whether that survives
    // being moved, renamed, cloned, or entered from a subdirectory.
    if !dry_run {
        let (id, source) = crate::infra::resolve_project_id_with(None, Some(&cwd));
        match crate::infra::write_project_file(&cwd, &id, None) {
            Ok(path) if source == crate::infra::IdSource::RepoFile => {
                println!(
                    "  Project id: {id} (already declared in {})",
                    path.display()
                );
            }
            Ok(path) => {
                println!("  Project id: {id}");
                println!(
                    "           declared in {} — commit it, so this project stays itself when the directory moves",
                    path.display()
                );
            }
            // Not fatal. A project that cannot write the file is exactly as
            // usable as it was a moment ago.
            Err(e) => println!("  Project id: {id} (could not declare it: {e})"),
        }
    }

    // Every client present, not the first one guessed. Two agents open on the
    // same repository is the normal case, and setting up one of them was never
    // a choice anybody asked for.
    for host in &hosts {
        println!("  IDE:     {} ({})", host.name, host.config_file);
    }
    if authoring_is_default_in(&cwd, &env) {
        println!(
            "           (no client detected here — {DEFAULT_AUTHORING_HOST}'s config is the default)"
        );
    }
    match project {
        Some(p) => {
            println!("  Project: {} ({})", p.name, p.marker);
            println!("  Verify:  {}", p.verify.join(", "));
        }
        None => println!("  Project: unknown"),
    }
    for host in present_hosts_in(&cwd, &env)
        .iter()
        .filter(|t| !t.authorable)
    {
        println!(
            "  Manual:  {} is present; add the entry to {} yourself if you use it",
            host.name, host.config_file
        );
    }
    println!();

    for host in &hosts {
        write_mcp_config(
            &cwd.join(host.config_file),
            host.config_file,
            host.container,
            dry_run,
            force,
        )?;
    }

    if !with_claude_md {
        println!("\nYou're ready! Start a conversation and the server will connect.");
        if let Some(p) = project {
            println!(
                "\nDetected {} project — your agent can verify with:",
                p.name
            );
            for cmd in p.verify {
                println!("  {cmd}");
            }
        }
        println!("\nTip: run with --with-claude-md to also generate a CLAUDE.md tool reference.");
        return Ok(());
    }

    write_claude_md(&cwd, project, dry_run, force)?;
    Ok(())
}

/// Merge-preserving write of the local `init` server entry into the host config.
///
/// `init` KEEPS an existing entry unless forced, and that is deliberate rather
/// than inherited: the entry `init` authors is the local one, so overwriting a
/// cloud entry with it would disconnect a connected user without saying so.
fn write_mcp_config(
    config_path: &Path,
    label: &str,
    container: ServerContainer,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    write_server_config(
        config_path,
        label,
        container,
        mcp_server_config(),
        dry_run,
        if force {
            OnExisting::Rewrite
        } else {
            OnExisting::Keep
        },
    )?;
    Ok(())
}

/// One client whose config now carries the cloud entry, and what the write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientWrite {
    pub host: &'static str,
    pub outcome: WriteOutcome,
}

/// A client we found but may not write for, and the one thing a human must do.
///
/// Reported rather than skipped. A client that is plainly here and silently left
/// out is indistinguishable, from the terminal, from a client that was
/// configured — which is how "Connected" came to mean "one of your agents is
/// connected, possibly not this one".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManualStep {
    /// The entry lives in a config the host's own CLI must edit
    /// (`~/.claude.json`; see [`ServerEntry::is_ours_to_write`]).
    HostCommand {
        host: &'static str,
        at: String,
        command: String,
    },
    /// A client that is present but that we never author for — VS Code, whose
    /// marker directory is not evidence of agent mode.
    Unauthorable {
        host: &'static str,
        config_file: &'static str,
    },
}

/// What `connect` did to this project's MCP configuration, across every client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConnectWrites {
    /// Clients configured, in table order. Never empty on `Ok`.
    pub configured: Vec<ClientWrite>,
    /// Clients that need a human step, each carrying exactly what that step is.
    pub manual: Vec<ManualStep>,
    /// True when no client was detected and the named default was used.
    pub used_default: bool,
}

/// `think-and-ship connect` — merge a cloud-configured `think-and-ship` entry
/// (write-through sync to `cloud_url`, with the token resolved from `profile`)
/// into the config of EVERY client this project has. `pub(crate)` so
/// `cli::connect` can call it after the device flow.
///
/// `profile` is a NAME, not a secret. The token it names lives in the credential
/// store; see [`crate::cloud::credential`].
///
/// TWO RULES, and the second is the one that stopped the guessing. First, every
/// config that ALREADY holds an entry is updated wherever it is — a search, not
/// a guess, because a guess writes a valid cloud configuration into a file the
/// agent never opens and the user is told "Connected" while their agent stays
/// local. Second, every client the project shows positive evidence of is
/// configured too. Picking one forced a wrong answer whenever two clients
/// existed, which is the normal case, and it is also why the old ambiguity
/// error existed: "writing to one would leave the others stale" is answered by
/// writing to all of them, which is the state in which none is stale.
///
/// Unlike `init`, this never declines. A credential has just been minted for
/// this exact `cloud_url` under this exact `profile`; an entry naming anything
/// else, or naming no cloud at all, is stale by construction. `force` only
/// escalates to rewriting an entry that already matches.
/// The user-level Claude config and the caller's environment are PARAMETERS
/// rather than ambient reads, so a test can drive a candidate set and a caller
/// identity this process has never held. There is no wrapper that resolves them
/// for you: the one that existed was the only path by which an ambient
/// environment could still reach this lane, and `connect` resolves the caller
/// once, up front, so a bad `--client` fails before the browser opens rather
/// than after someone has finished signing in.
pub(crate) fn write_cloud_mcp_config_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
    env: &CallerEnv,
    cloud_url: &str,
    profile: &str,
    dry_run: bool,
    force: bool,
) -> Result<ConnectWrites> {
    let server = cloud_server_config(cloud_url, profile);
    let on_existing = if force {
        OnExisting::Rewrite
    } else {
        OnExisting::Replace
    };

    let present = present_hosts_in(cwd, env);
    // The default is reached only when the project shows no authorable client
    // AND nothing anywhere already holds an entry — never because a particular
    // client's marker happened to be missing.
    let used_default = authoring_is_default_in(cwd, env)
        && matching_entries_in(cwd, home_config.clone()).is_empty();
    let targets = connect_targets_in(cwd, home_config, &present);
    if used_default {
        println!(
            "No client detected for this project and no think-and-ship entry to update — \
             writing {DEFAULT_AUTHORING_HOST}'s {} as the default.",
            default_authoring_host().config_file
        );
    }

    let mut writes = ConnectWrites {
        used_default,
        ..ConnectWrites::default()
    };
    for entry in targets {
        match plan_registration(&entry, &server)? {
            Registration::Direct { entry } => {
                let host = entry.host;
                let outcome = write_server_config(
                    &entry.path,
                    &entry.describe(),
                    entry.container,
                    server.clone(),
                    dry_run,
                    on_existing,
                )?;
                writes.configured.push(ClientWrite { host, outcome });
            }
            Registration::AlreadyCurrent { entry } => writes.configured.push(ClientWrite {
                host: entry.host,
                outcome: WriteOutcome::AlreadyCurrent,
            }),
            Registration::HostCommand { entry, command } => {
                writes.manual.push(ManualStep::HostCommand {
                    host: entry.host,
                    at: entry.path.display().to_string(),
                    command,
                });
            }
        }
    }

    // Present, never authored for, and now said out loud.
    for host in present.iter().filter(|t| !t.authorable) {
        let already = writes.configured.iter().any(|c| c.host == host.name)
            || writes.manual.iter().any(|m| match m {
                ManualStep::HostCommand { host: h, .. }
                | ManualStep::Unauthorable { host: h, .. } => *h == host.name,
            });
        if !already {
            writes.manual.push(ManualStep::Unauthorable {
                host: host.name,
                config_file: host.config_file,
            });
        }
    }

    if writes.configured.is_empty() {
        let steps = writes
            .manual
            .iter()
            .map(|m| match m {
                ManualStep::HostCommand { host, at, command } => format!(
                    "this project's think-and-ship entry lives in {at}, which {host} manages and \
                     this tool will not rewrite (doing so reorders and perturbs the whole file). \
                     Run this to finish connecting:\n\n  {command}\n"
                ),
                ManualStep::Unauthorable { host, config_file } => format!(
                    "{host} is present but is never configured automatically. Add the \
                     think-and-ship entry to {config_file} yourself to use it."
                ),
            })
            .collect::<Vec<_>>()
            .join("\n");
        bail!("no MCP config was written for this project.\n\n{steps}");
    }
    Ok(writes)
}

/// Every config `connect` should write for: the ones that already hold an entry,
/// plus one per present authorable client that does not yet have one, plus the
/// named default when there is neither.
fn connect_targets_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
    present: &[&'static HostTarget],
) -> Vec<ServerEntry> {
    let mut targets = matching_entries_in(cwd, home_config);
    for host in present.iter().filter(|t| t.authorable) {
        let path = cwd.join(host.config_file);
        if !targets
            .iter()
            .any(|e| e.project_key.is_none() && e.path == path)
        {
            targets.push(ServerEntry {
                path,
                project_key: None,
                container: host.container,
                host: host.name,
            });
        }
    }
    if targets.is_empty() {
        let host = default_authoring_host();
        targets.push(ServerEntry {
            path: cwd.join(host.config_file),
            project_key: None,
            container: host.container,
            host: host.name,
        });
    }
    targets
}

/// What to do when the host config already holds a `think-and-ship` entry.
///
/// This replaced a bare `force: bool`, and the replacement is the whole point of
/// the fix. `already: bool` answers the wrong question: there are FOUR shapes an
/// existing entry can have relative to the one we want to write — absent, present
/// but different in a way we must fix, present and already identical, present and
/// deliberately not ours to touch — and declining is correct for exactly one of
/// them. Collapsing them lost the distinction, and the loss surfaced two callers
/// later as "Connected" printed over a config that was never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnExisting {
    /// Leave a differing entry alone and report the decline. `init`'s default:
    /// the entry it would author is the LOCAL one, and writing that over a cloud
    /// entry would silently disconnect a connected user.
    Keep,
    /// Bring a differing entry up to date. `connect`'s only correct choice: a
    /// credential has just been minted for this exact `cloud_url` and profile, so
    /// an entry naming anything else — or naming no cloud at all — is stale by
    /// construction, and declining to fix it is declining to connect.
    Replace,
    /// Rewrite even an entry that already matches (`--force` on either lane).
    Rewrite,
}

/// What a write to a host config actually DID.
///
/// Returned rather than discarded so a caller can say something true about it.
/// The old signature returned `Result<()>` for both "wrote the entry" and
/// "declined to write the entry", which is how a success message ended up
/// printed over a config that still held a local-only entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    /// No entry existed; this one was authored.
    Created,
    /// An entry existed, differed, and was replaced.
    Updated,
    /// An entry existed and already equalled the one we would have written, so
    /// the file was not touched. NOT a failure — and, load-bearingly, it means
    /// the entry on disk HAS whatever wiring the entry we built has.
    AlreadyCurrent,
    /// An entry existed, differed, and was left alone because the policy was
    /// [`OnExisting::Keep`]. Nothing was written.
    Declined,
}

/// Merge-preserving write of an arbitrary `think-and-ship` server entry into a
/// host config — shared by `init` (local) and `connect` (cloud). Preserves every
/// unrelated server and every unrelated top-level key.
///
/// Returns what it did; see [`WriteOutcome`]. The comparison against the existing
/// entry is structural (`serde_json::Value` equality over maps), so a config the
/// user has reformatted or whose keys are in another order still compares equal
/// and is left alone.
fn write_server_config(
    config_path: &Path,
    label: &str,
    container: ServerContainer,
    server: Value,
    dry_run: bool,
    on_existing: OnExisting,
) -> Result<WriteOutcome> {
    let key = container.key();
    let existing = read_json_safe(config_path);
    let current = existing
        .as_ref()
        .and_then(|c| c.get(key))
        .and_then(|s| s.get(MCP_SERVER_NAME));

    let outcome = match current {
        None => WriteOutcome::Created,
        Some(c) if *c == server && on_existing != OnExisting::Rewrite => {
            WriteOutcome::AlreadyCurrent
        }
        Some(_) if on_existing == OnExisting::Keep => WriteOutcome::Declined,
        Some(_) => WriteOutcome::Updated,
    };

    match outcome {
        WriteOutcome::AlreadyCurrent => {
            println!("Already configured in {label} — nothing to change.");
            return Ok(outcome);
        }
        WriteOutcome::Declined => {
            println!("Already configured in {label} (use --force to overwrite).");
            return Ok(outcome);
        }
        WriteOutcome::Created | WriteOutcome::Updated => {}
    }

    let mut config = existing.clone().unwrap_or_else(|| json!({}));
    if !config.is_object() {
        config = json!({});
    }
    let obj = config.as_object_mut().expect("config is an object");
    let servers = obj.entry(key).or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    servers
        .as_object_mut()
        .expect("the server map is an object")
        .insert(MCP_SERVER_NAME.to_string(), server);

    let output = format!("{}\n", serde_json::to_string_pretty(&config)?);

    if dry_run {
        println!("Would write to {label}:\n\n{output}");
        return Ok(outcome);
    }

    if let Some(dir) = config_path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(config_path, output).with_context(|| format!("writing {label}"))?;
    println!("Wrote {label}");
    println!("  Added: {MCP_SERVER_NAME} (under {key})");
    if existing.is_some() {
        println!("  Preserved existing servers");
    }
    Ok(outcome)
}

/// Outcome of [`merge_server_env`], so a caller can report honestly instead of
/// claiming it wired something up when it did not.
#[derive(Debug, PartialEq, Eq)]
pub enum EnvMerge {
    /// The variable was added or changed. Carries the previous value, if any.
    Written { previous: Option<String> },
    /// Already exactly this value — nothing written, nothing to reconnect for.
    AlreadySet,
    /// No think-and-ship entry to merge into. `init` has not run here.
    NoServerEntry,
    /// The entry lives in a config this tool will not rewrite. Carries where it
    /// is, so the caller can tell the human exactly what to edit.
    ///
    /// See [`ServerEntry::is_ours_to_write`] for why refusing is the correct
    /// behaviour rather than a missing feature.
    ExternalConfig { at: String },
}

/// Where a think-and-ship MCP entry actually lives.
///
/// The `mcpServers` map is at the document root in a project config, but nested
/// under `projects."<abs path>"` in `~/.claude.json`, which is where
/// `claude mcp add` writes. Carrying the shape with the path means one merge
/// routine serves both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEntry {
    pub path: PathBuf,
    /// The key under `projects` when the entry is nested, else `None`.
    pub project_key: Option<String>,
    /// Which JSON key holds the server map in this particular file.
    pub container: ServerContainer,
    /// The host this location belongs to, so a message can name it.
    pub host: &'static str,
}

impl ServerEntry {
    /// How to describe this location to a human who has to go look at it.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.project_key {
            Some(_) => format!(
                "{} ({} — this project's entry)",
                self.path.display(),
                self.host
            ),
            None => format!("{} ({})", self.path.display(), self.host),
        }
    }

    /// Whether this tool may REWRITE this file, as opposed to merely reading it.
    ///
    /// Only configs `init` authors — the small, project-local `.mcp.json` and
    /// friends — qualify. `~/.claude.json` does not, and the reason is specific
    /// rather than squeamish: rewriting it means round-tripping the WHOLE
    /// document through `serde_json::Value`, and that is not faithful.
    ///
    /// Measured on a real 390 KB config carrying 151 projects:
    ///   * `preserve_order` is not enabled, so `Value` is a `BTreeMap` and every
    ///     object's keys come back ALPHABETICALLY SORTED — the entire file
    ///     reordered to set one variable.
    ///   * 157 floats under `lastSessionMetrics`/`lastCost` shifted by one ULP,
    ///     because the parse/serialize round trip is not bit-exact for them.
    ///
    /// Neither breaks the JSON, and both are unacceptable collateral for editing
    /// one env var in a file that holds every project the user has. So we find
    /// the entry, report precisely where it is, and let the human edit it.
    #[must_use]
    pub fn is_ours_to_write(&self) -> bool {
        self.project_key.is_none()
    }

    /// Borrow the server map this location points at, under whichever key this
    /// host keeps it.
    fn servers_mut<'a>(&self, doc: &'a mut Value) -> Option<&'a mut Value> {
        let container = self.container.key();
        match &self.project_key {
            None => doc.get_mut(container),
            Some(key) => doc
                .get_mut("projects")
                .and_then(|p| p.get_mut(key))
                .and_then(|p| p.get_mut(container)),
        }
    }

    /// The read-only twin of [`Self::servers_mut`] — for inspecting an entry in
    /// a config this tool may not rewrite.
    fn servers<'a>(&self, doc: &'a Value) -> Option<&'a Value> {
        let container = self.container.key();
        match &self.project_key {
            None => doc.get(container),
            Some(key) => doc
                .get("projects")
                .and_then(|p| p.get(key))
                .and_then(|p| p.get(container)),
        }
    }

    /// This entry's `env` block, read from the file on disk.
    fn env_on_disk(&self) -> Option<serde_json::Map<String, Value>> {
        let doc = read_json_safe(&self.path)?;
        self.servers(&doc)?
            .get(MCP_SERVER_NAME)?
            .get("env")?
            .as_object()
            .cloned()
    }
}

/// Every env key that carries the cloud wiring.
///
/// One list, so `disconnect` cannot fall out of step with what `connect` writes
/// — a key added to [`cloud_server_config`] and forgotten here is a key that
/// survives a disconnect.
pub(crate) const CLOUD_ENV_KEYS: &[&str] = &[
    "THINK_AND_SHIP_SYNC_TARGET",
    "THINK_AND_SHIP_CLOUD_URL",
    crate::cloud::credential::TOKEN_ENV,
    crate::cloud::credential::PROFILE_ENV,
];

/// A plaintext token found sitting in a config file, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaintextToken {
    pub token: String,
    /// Human-readable location, for saying what was cleaned up.
    pub at: String,
    /// Whether this tool may rewrite that file. When false the human has to
    /// remove it, and must be told exactly where it is.
    pub ours_to_write: bool,
}

/// Find a plaintext cloud token in the config the agent actually reads.
///
/// Read-only. This is the detection half of migration: a token that predates
/// the credential store is a live secret in a file that gets committed and
/// synced, so `connect` looks for one every time rather than only when the user
/// passes a flag.
pub(crate) fn find_plaintext_token_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
) -> Option<PlaintextToken> {
    let entries = match resolve_host_in(cwd, home_config) {
        HostResolution::Resolved(entry) => vec![entry],
        // Ambiguity blocks WRITING, but a secret sitting in any of the candidate
        // files is still a secret. Reporting them all is what lets the human
        // clean up the ones we will not touch.
        HostResolution::Ambiguous(entries) => entries,
        HostResolution::NoEntry => return None,
    };
    entries.into_iter().find_map(|entry| {
        let token = entry
            .env_on_disk()?
            .get(crate::cloud::credential::TOKEN_ENV)?
            .as_str()?
            .trim()
            .to_string();
        if token.is_empty() {
            return None;
        }
        Some(PlaintextToken {
            token,
            at: entry.describe(),
            ours_to_write: entry.is_ours_to_write(),
        })
    })
}

/// What `remove_server_env_in` did.
#[derive(Debug, PartialEq, Eq)]
pub enum EnvRemoval {
    /// Keys were removed. Carries their names, so the caller can say which.
    Removed(Vec<String>),
    /// The entry exists and held none of them — already clean.
    NothingToRemove,
    /// No think-and-ship entry anywhere for this project.
    NoServerEntry,
    /// The entry is in a config this tool will not rewrite. Carries where.
    ExternalConfig { at: String },
}

/// Remove env keys from the think-and-ship entry, preserving everything else.
///
/// The counterpart to [`merge_server_env`], and it exists for the same reason:
/// `write_server_config` replaces an entry wholesale, which would silently drop
/// unrelated variables a user had set by hand.
pub(crate) fn remove_server_env_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
    keys: &[&str],
    dry_run: bool,
) -> Result<EnvRemoval> {
    let entry_at = match resolve_host_in(cwd, home_config) {
        HostResolution::Resolved(entry) => entry,
        // Removing from one of several would leave the others holding the
        // secret while reporting success.
        HostResolution::Ambiguous(entries) => {
            let list = entries
                .iter()
                .map(|e| format!("  {}", e.describe()))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "more than one MCP config holds a think-and-ship entry for this project:\n\n{list}\
                 \n\nRemoving the cloud settings from one would leave the others still \
                 configured. Remove the entries you do not use, then run this again."
            )
        }
        HostResolution::NoEntry => return Ok(EnvRemoval::NoServerEntry),
    };

    let Some(mut doc) = read_json_safe(&entry_at.path) else {
        return Ok(EnvRemoval::NoServerEntry);
    };
    if !entry_at.is_ours_to_write() {
        return Ok(EnvRemoval::ExternalConfig {
            at: entry_at.describe(),
        });
    }
    let Some(entry) = entry_at
        .servers_mut(&mut doc)
        .and_then(|s| s.get_mut(MCP_SERVER_NAME))
        .filter(|e| e.is_object())
    else {
        return Ok(EnvRemoval::NoServerEntry);
    };
    let Some(env) = entry
        .as_object_mut()
        .expect("filtered to an object")
        .get_mut("env")
        .and_then(Value::as_object_mut)
    else {
        return Ok(EnvRemoval::NothingToRemove);
    };

    let mut removed = Vec::new();
    for key in keys {
        if env.remove(*key).is_some() {
            removed.push((*key).to_string());
        }
    }
    if removed.is_empty() {
        return Ok(EnvRemoval::NothingToRemove);
    }
    if !dry_run {
        write_json_atomic(&entry_at.path, &doc)?;
    }
    Ok(EnvRemoval::Removed(removed))
}

/// The user-level Claude config, where `claude mcp add` records per-project
/// servers.
pub(crate) fn claude_home_config() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude.json"))
}

/// Every place a think-and-ship entry could live for `cwd`, in the order we
/// would prefer to write to one.
///
/// `home_config` is a parameter rather than a read of `HOME` so a test can drive
/// a candidate set the process environment has never held — mutating `HOME`
/// mid-suite is process-global and races every other test.
fn candidate_entries_in(cwd: &Path, home_config: Option<PathBuf>) -> Vec<ServerEntry> {
    let mut out: Vec<ServerEntry> = HOST_TARGETS
        .iter()
        .map(|t| ServerEntry {
            path: cwd.join(t.config_file),
            project_key: None,
            container: t.container,
            host: t.name,
        })
        .collect();
    if let Some(home) = home_config {
        // The absolute path is the key `claude mcp add` uses. Canonicalize so a
        // symlinked or relative cwd still matches what is recorded.
        let key = cwd
            .canonicalize()
            .unwrap_or_else(|_| cwd.to_path_buf())
            .display()
            .to_string();
        out.push(ServerEntry {
            path: home,
            project_key: Some(key),
            container: ServerContainer::McpServers,
            host: "Claude Code",
        });
    }
    out
}

/// Find the config that ACTUALLY holds this project's think-and-ship entry.
///
/// This replaced a path GUESS, and the guess was wrong in the common case.
/// `detect_ide` picks by marker directory, so a repo containing a `.cursor/`
/// folder resolved to `.cursor/mcp.json` — even when the live entry was the one
/// `claude mcp add` had written into `~/.claude.json`, and even when
/// `.cursor/mcp.json` held somebody else's servers and none of ours. The result
/// was "auto-push was skipped, run init here", advice which would have created a
/// FOURTH config rather than touching the one in use.
///
/// So: search, do not guess. Only a location that already contains our entry is
/// returned, because this function exists to UPDATE an entry, never to invent
/// one — authoring is `init`'s job, and it alone knows the command and args.
#[must_use]
pub fn find_server_entry(cwd: &Path) -> Option<ServerEntry> {
    matching_entries_in(cwd, claude_home_config())
        .into_iter()
        .next()
}

/// EVERY candidate location that actually holds this project's entry.
///
/// [`find_server_entry`] wants the first; `connect` needs the count, because two
/// live entries is a different situation from one and must not be silently
/// narrowed to it.
fn matching_entries_in(cwd: &Path, home_config: Option<PathBuf>) -> Vec<ServerEntry> {
    candidate_entries_in(cwd, home_config)
        .into_iter()
        .filter(|c| {
            read_json_safe(&c.path).is_some_and(|mut doc| {
                c.servers_mut(&mut doc)
                    .and_then(|s| s.get(MCP_SERVER_NAME))
                    .is_some_and(Value::is_object)
            })
        })
        .collect()
}

/// A connection this machine already had, recovered from the MCP config that
/// used to be the only place one was written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Adoption {
    /// The record that will be — or has been — written.
    pub connection: crate::cloud::connection::Connection,
    /// Which client's config it was read out of, for saying so.
    pub from: String,
}

/// The connection a pre-record machine still carries in some client's MCP
/// config — READ-ONLY, and only when the credential store agrees.
///
/// WHY THIS EXISTS. Before the connection record, `connect` wrote the cloud url
/// and the profile name into an MCP config's `env` block and nowhere else. An
/// MCP host injects that block into the server it spawns, so the server kept
/// working; a shell has no such channel, so every CLI verb on those machines
/// reports "not connected" while the agent syncs happily. Those machines are
/// otherwise fine — the token is in the store, the url is on disk — they are
/// just missing the object that ties the two together.
///
/// EVERY CLIENT'S CONFIG, not a guessed one. [`matching_entries_in`] returns
/// every location that actually holds this project's entry, and that is the
/// right input precisely because the machines needing this are the ones the old
/// host guess misconfigured: the settings may be sitting in a client nobody
/// would guess. This is also why the search must be handed
/// [`claude_home_config`] — on the machine this was written for, the entry is in
/// `~/.claude.json`, a file we may read and must never rewrite.
///
/// THE STORE HAS A VOTE, and that is the load-bearing half of the rule. A config
/// naming a url and a profile is NOT evidence that this machine ever connected:
/// `.mcp.json` and `.cursor/mcp.json` are committed to repositories, so anyone
/// who clones this project has one. Adopting on that alone would fabricate a
/// connection to a workspace they hold no token for, replacing an honest "not
/// connected" with a false "connected" — a new lie, in the surface this exists
/// to make honest. What cannot be committed is the token, so the token is the
/// evidence: the profile the config names must actually answer from the store.
fn legacy_connection_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
    store: &dyn crate::tracker::credential::CredentialStore,
    now: &str,
) -> Option<Adoption> {
    matching_entries_in(cwd, home_config)
        .into_iter()
        .find_map(|entry| {
            let env = entry.env_on_disk()?;
            let read = |key: &str| {
                env.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
            };
            let cloud_url = read("THINK_AND_SHIP_CLOUD_URL")?;
            let profile = read(crate::cloud::credential::PROFILE_ENV)?;
            crate::cloud::credential::resolve(store, &profile)?;
            Some(Adoption {
                connection: crate::cloud::connection::Connection {
                    cloud_url,
                    profile,
                    connected_at: now.to_string(),
                },
                from: entry.describe(),
            })
        })
}

/// Adopt a pre-record connection ONCE: write the record and report what was
/// taken, or answer `None` when there is nothing to do.
///
/// A MIGRATION, NEVER A FALLBACK, and the distinction is the whole safety
/// argument. This reads an MCP config exactly once in the life of a machine and
/// leaves a record behind; from then on the record is the only thing anyone
/// consults. No resolve path may ever reach for a config file — that coupling is
/// what made the MCP config the connection database, and it is what made writing
/// that one file to the wrong client destroy the connection outright. Which is
/// why this lives here in `cli`, beside the config search, and `cloud::config`
/// keeps its three plain values and no filesystem at all.
///
/// An existing record ends it immediately: adoption never overwrites, so a
/// machine that has connected since is untouched, and running two cloud verbs in
/// a row adopts once and then says nothing.
///
/// Every input is a parameter — `data_dir`, `cwd`, `home_config`, the store, and
/// even `now`. An ambient read here would be inherited by every test in the
/// crate that reaches this lane, and the suite would agree with whoever ran it.
pub(crate) fn adopt_legacy_connection_in(
    data_dir: &Path,
    project_id: &str,
    cwd: &Path,
    home_config: Option<PathBuf>,
    store: &dyn crate::tracker::credential::CredentialStore,
    now: &str,
) -> Option<Adoption> {
    if crate::cloud::connection::load_in(data_dir, project_id).is_some() {
        return None;
    }
    let adoption = legacy_connection_in(cwd, home_config, store, now)?;
    crate::cloud::connection::save_in(data_dir, project_id, &adoption.connection).ok()?;
    Some(adoption)
}

/// What a search for this project's live MCP entry found.
///
/// Named outcomes rather than an `Option`, because the two failure shapes need
/// different words: nothing to update is a reason to author, while several things
/// to update is a reason to stop and ask.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HostResolution {
    /// Exactly one host holds an entry. Update that one, wherever it is.
    Resolved(ServerEntry),
    /// Several hosts hold an entry. Reported, never narrowed: writing to one
    /// leaves the others stale, and which one the agent reads is the host's
    /// decision rather than ours to assume.
    Ambiguous(Vec<ServerEntry>),
    /// Nothing holds an entry yet, so there is nothing to update and authoring is
    /// the correct next act.
    NoEntry,
}

/// Find the ONE config `connect` should update — search, never guess.
///
/// `home_config` is supplied by the caller rather than read from `HOME`, so a test
/// can drive a candidate set the process environment has never held.
fn resolve_host_in(cwd: &Path, home_config: Option<PathBuf>) -> HostResolution {
    let mut found = matching_entries_in(cwd, home_config);
    match found.len() {
        0 => HostResolution::NoEntry,
        1 => HostResolution::Resolved(found.remove(0)),
        _ => HostResolution::Ambiguous(found),
    }
}

/// Which client this machine is connecting, so the minted token can be named
/// for it.
///
/// The CALLER is asked first, because a command run by Claude Code is a Claude
/// Code connection whatever else the repository contains. Only when the
/// environment names nobody does this fall back to the entry that exists, then
/// to the single client `connect` will author for.
///
/// `None` means the answer is genuinely unknown — several candidates and nothing
/// to choose between them. Guessing here would put a false client name on a
/// connection for the rest of its life.
pub(crate) fn client_label_in(
    cwd: &Path,
    home_config: Option<PathBuf>,
    env: &CallerEnv,
) -> Option<&'static str> {
    if let Some(caller) = identify_caller(env) {
        return Some(caller.name);
    }
    match resolve_host_in(cwd, home_config) {
        HostResolution::Resolved(entry) => Some(entry.host),
        HostResolution::NoEntry => match authoring_hosts_in(cwd, env).as_slice() {
            [only] => Some(only.name),
            _ => None,
        },
        HostResolution::Ambiguous(_) => None,
    }
}

/// How a resolved entry will actually be updated.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Registration {
    /// Merge directly into a config this tool owns.
    Direct { entry: ServerEntry },
    /// Not ours to rewrite — the host's own CLI must do it. Carries the exact
    /// command, so the caller can name one corrective action instead of
    /// describing a file format.
    HostCommand { entry: ServerEntry, command: String },
    /// Not ours to rewrite, and it already says exactly what we would say.
    ///
    /// Without this, a host-managed entry that is ALREADY CORRECT is answered
    /// with a corrective command that would change nothing — and, because the
    /// caller treats that as a hard error, `connect` aborts after the human has
    /// completed the browser flow, and the mint is rolled back. The result is a
    /// connect that can never succeed on such a host no matter how many times
    /// it is run, because the work it demands is already done.
    AlreadyCurrent { entry: ServerEntry },
}

/// Decide how to write a resolved entry.
///
/// The only non-writable candidate is `~/.claude.json` (see
/// [`ServerEntry::is_ours_to_write`] for the measured reason), and it has a CLI
/// that edits it safely. `--scope local` is that file's project-keyed section —
/// exactly where the entry was found.
///
/// The command removes before adding, because `claude mcp add-json` has no
/// overwrite flag and refuses an existing name ("MCP server think-and-ship
/// already exists in local config", measured against the real CLI) — and the
/// entry PROVABLY exists there, since finding it is the only way this branch is
/// reached. A bare add-json would fail for every user we print it for.
fn plan_registration(entry: &ServerEntry, server: &Value) -> Result<Registration> {
    if entry.is_ours_to_write() {
        return Ok(Registration::Direct {
            entry: entry.clone(),
        });
    }
    // Ask what the file already says before demanding it be changed. The
    // comparison is structural, so an entry the host wrote with different key
    // order still compares equal. Skipping this check is what made `connect`
    // unable to finish on a host-managed config that was already correct.
    let current = read_json_safe(&entry.path)
        .as_ref()
        .and_then(|doc| entry.servers(doc))
        .and_then(|servers| servers.get(MCP_SERVER_NAME))
        .cloned();
    if current.as_ref() == Some(server) {
        return Ok(Registration::AlreadyCurrent {
            entry: entry.clone(),
        });
    }
    let json = serde_json::to_string(server)?;
    Ok(Registration::HostCommand {
        entry: entry.clone(),
        command: format!(
            "claude mcp remove {MCP_SERVER_NAME} --scope local && \
             claude mcp add-json {MCP_SERVER_NAME} '{json}' --scope local"
        ),
    })
}

/// Set ONE environment variable on the existing think-and-ship MCP entry,
/// preserving everything else about it.
///
/// Deliberately NOT `write_server_config`, and that distinction is the whole
/// reason this exists. That function REPLACES the entry wholesale, which is
/// right for `init` (it is authoring the entry) and catastrophic here: it would
/// silently strip `THINK_AND_SHIP_SYNC_TARGET`, `_CLOUD_URL` and `_CLOUD_TOKEN`
/// from anyone who had run `connect`, turning "I enabled auto-push" into "my
/// cloud sync stopped".
///
/// So this reads the document, walks to the entry's own `env` object, and
/// touches a single key. Absent entry is reported rather than created.
pub fn merge_server_env(
    entry_at: &ServerEntry,
    key: &str,
    value: &str,
    dry_run: bool,
) -> Result<EnvMerge> {
    let Some(mut doc) = read_json_safe(&entry_at.path) else {
        return Ok(EnvMerge::NoServerEntry);
    };
    if !entry_at.is_ours_to_write() {
        return Ok(EnvMerge::ExternalConfig {
            at: entry_at.describe(),
        });
    }
    let Some(entry) = entry_at
        .servers_mut(&mut doc)
        .and_then(|s| s.get_mut(MCP_SERVER_NAME))
        .filter(|e| e.is_object())
    else {
        return Ok(EnvMerge::NoServerEntry);
    };

    let env = entry
        .as_object_mut()
        .expect("filtered to an object")
        .entry("env")
        .or_insert_with(|| json!({}));
    if !env.is_object() {
        *env = json!({});
    }
    let env = env.as_object_mut().expect("env is an object");

    let previous = env.get(key).and_then(|v| v.as_str()).map(str::to_string);
    if previous.as_deref() == Some(value) {
        return Ok(EnvMerge::AlreadySet);
    }
    env.insert(key.to_string(), Value::String(value.to_string()));

    if !dry_run {
        write_json_atomic(&entry_at.path, &doc)?;
    }
    Ok(EnvMerge::Written { previous })
}

/// Write JSON via a temp file and a rename.
///
/// `~/.claude.json` is now a write target, and it is the user's entire Claude
/// configuration — every project, every server, their cloud token. A truncating
/// write that dies halfway leaves that file destroyed. A rename is atomic on
/// POSIX, so the worst case becomes a stray temp file instead.
fn write_json_atomic(path: &Path, doc: &Value) -> Result<()> {
    let body = format!("{}\n", serde_json::to_string_pretty(doc)?);
    let tmp = path.with_extension(format!(
        "ts-tmp-{}",
        std::process::id() // unique per process, so concurrent runs don't collide
    ));
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// The first IDE config path `init` would write in this directory.
///
/// `init` writes one per present client now, so this is the first of several
/// rather than the only one. Anything UPDATING an existing entry wants
/// [`find_server_entry`] instead.
#[must_use]
pub fn ide_config_path(cwd: &Path) -> PathBuf {
    let env = CallerEnv::from_env();
    cwd.join(
        authoring_hosts_in(cwd, &env)
            .first()
            .expect("authoring_hosts_in never returns empty")
            .config_file,
    )
}

/// What `--dry-run` should say it is about to do.
///
/// Derived from the same two facts the write path branches on, so the preview
/// cannot name one operation while the real run performs another. Reading the
/// verb off "does the file exist" alone announced "append" for every
/// force-replace — the preview's one job, done wrong.
fn dry_run_verb(existing: &Option<String>, has_section: bool) -> &'static str {
    match (existing, has_section) {
        (Some(_), true) => "replace the think-and-ship section in",
        (Some(_), false) => "append to",
        (None, _) => "create",
    }
}

/// Marker-guarded append/replace of the CLAUDE.md tool-reference section.
fn write_claude_md(
    cwd: &Path,
    project: Option<&ProjectType>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let path = cwd.join("CLAUDE.md");
    let content = generate_claude_md(project);
    let existing = fs::read_to_string(&path).ok();
    let has_section = existing
        .as_deref()
        .is_some_and(|c| c.contains(CLAUDE_MD_MARKER));

    if has_section && !force {
        println!("\nCLAUDE.md already contains a think-and-ship section (use --force to replace).");
        return Ok(());
    }
    if dry_run {
        let verb = dry_run_verb(&existing, has_section);
        println!("\nWould {verb} CLAUDE.md:\n\n{content}");
        return Ok(());
    }

    match existing {
        Some(text) if has_section => {
            // force-replace: splice over OUR span only, so whatever the user
            // wrote above and below the section both survive.
            let (start, end) = claude_md_span(&text).expect("has_section implies a span");
            let before = text[..start].trim_end();
            let after = text[end..].trim_start_matches('\n');
            let mut next = String::new();
            if !before.is_empty() {
                next.push_str(before);
                next.push_str("\n\n");
            }
            next.push_str(&content);
            if !after.trim().is_empty() {
                next.push('\n');
                next.push_str(after);
            }
            if !next.ends_with('\n') {
                next.push('\n');
            }
            fs::write(&path, next)?;
            println!("\nReplaced think-and-ship section in CLAUDE.md");
        }
        Some(text) => {
            fs::write(&path, format!("{}\n\n{content}\n", text.trim_end()))?;
            println!("\nAppended think-and-ship section to CLAUDE.md");
        }
        None => {
            fs::write(&path, format!("{content}\n"))?;
            println!("\nCreated CLAUDE.md with think-and-ship tool reference");
        }
    }
    Ok(())
}

/// The origin block of a store report, in the four-glyph shape, for ANY family.
/// Written once so no family can quietly lose the "unprovable is kept" line —
/// that line is the user-visible half of the guarantee store_health enforces.
fn report_origin(
    family: &str,
    unit: &str,
    report: &crate::cli::store_health::StoreReport,
    fix: &str,
) -> usize {
    let mut issues = 0;
    if report.foreign.is_empty() {
        // Distinguish "checked, all ours" from "nothing to check against".
        // Claiming the first when every record predates origin tracking would
        // be asserting proof we don't have.
        let stamped = report.total - report.unstamped;
        if stamped == 0 && report.total > 0 {
            println!(
                "  [ -- ] {family} store: {} {unit}(s), none carrying an origin yet",
                report.total
            );
        } else {
            println!(
                "  [ OK ] {family} store: {stamped} of {} {unit}(s) confirmed this project's, none foreign",
                report.total
            );
        }
    } else {
        println!(
            "  [WARN] {family} store: {} of {} {unit}(s) belong to another project",
            report.foreign.len(),
            report.total
        );
        println!(
            "         e.g. {}",
            report
                .foreign
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("         Fix: {fix}   (lists them; --apply removes)");
        issues += 1;
    }

    if report.unstamped > 0 {
        println!(
            "  [ -- ] {family} store: {} {unit}(s) of unprovable origin (kept; never auto-removed)",
            report.unstamped
        );
    }
    issues
}

/// A declared identity that the records on this machine disagree with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityDisagreement {
    /// What the committed file declares this project is.
    pub declared: String,
    /// What this directory would answer if the file were not there — and so the
    /// id everything written here before the declaration is keyed by.
    pub detached: String,
    /// Which families still hold records under `detached`, and how many.
    /// `(unit, count)`, e.g. `("chunk", 503)`.
    pub holding: Vec<(String, usize)>,
}

impl IdentityDisagreement {
    /// "503 chunk(s), 792 step(s)" — what is out of reach, for a human.
    #[must_use]
    pub fn summary(&self) -> String {
        self.holding
            .iter()
            .map(|(unit, n)| format!("{n} {unit}(s)"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Does a declaration disagree with the records this directory already has?
///
/// [`crate::infra::write_project_file`] refuses to overwrite, so no code path
/// can produce this state — but the file is committed and hand-editable, and an
/// edited id silently detaches every reasoning step, chunk and signal written
/// under the old one. Nothing else notices: each store is internally consistent,
/// and the project simply answers to a name none of them were filed under.
///
/// The rule: a declaration disagrees when it differs from the id this directory
/// would otherwise answer AND that other id's stores still hold records. Both
/// halves matter — a difference with nothing behind it detaches nothing.
///
/// `holdings` is a PARAMETER rather than a read of the data directory. A rule
/// that reads its own inputs can only be tested against the families this
/// deployment happens to serve, and then "the answer is derived from the stores"
/// is a restatement rather than a claim.
#[must_use]
pub fn identity_disagreement(
    declared: Option<&str>,
    without_the_file: &str,
    holdings: &[(String, usize)],
) -> Option<IdentityDisagreement> {
    let declared = declared?;
    if declared == without_the_file {
        return None;
    }
    let holding: Vec<(String, usize)> = holdings
        .iter()
        .filter(|(_, count)| *count > 0)
        .cloned()
        .collect();
    if holding.is_empty() {
        return None;
    }
    Some(IdentityDisagreement {
        declared: declared.to_string(),
        detached: without_the_file.to_string(),
        holding,
    })
}

/// How many records each family holds under `project_id`, for the families this
/// deployment actually persists. Empty stores are reported as zero rather than
/// omitted, so "checked and found nothing" is distinguishable from "not checked".
fn records_held_under(project_id: &str) -> Vec<(String, usize)> {
    let cfg = crate::infra::PersistenceConfig::from_env();
    let mut out = Vec::new();

    let roadmap = crate::infra::Persistence::new(&cfg, crate::infra::Domain::Roadmap);
    if let Ok(Some(r)) = roadmap.load::<crate::roadmap::domain::Roadmap>(project_id) {
        out.push(("chunk".to_string(), r.chunks.len()));
    }

    let signal = crate::infra::Persistence::new(&cfg, crate::infra::Domain::Signal);
    if let Ok(Some(s)) = signal.load::<crate::signal::domain::Signals>(project_id) {
        out.push(("signal".to_string(), s.signals.len()));
    }

    let think = crate::think::persistence::Persistence::for_project(
        &crate::infra::PersistenceConfig {
            enabled: cfg.enabled,
            data_dir: cfg.data_dir.clone(),
        },
        project_id,
    );
    if let Some(history) = think.load_default() {
        out.push(("step".to_string(), history.steps.len()));
    }

    out
}

/// Report where this project's identity came from, and whether the records on
/// this machine agree with it. Returns the issue count.
fn report_project_identity(cwd: &Path) -> usize {
    let (id, source) = crate::infra::resolve_project_id_with(None, Some(cwd));
    match source {
        crate::infra::IdSource::Environment => {
            println!("  [ OK ] project identity: {id}, from {}", source.label());
            return 0;
        }
        crate::infra::IdSource::DerivedFromPath => {
            println!("  [ -- ] project identity: {id}, from {}", source.label());
            println!("         Renaming or moving this directory makes it a different project.");
            println!("         Tip: think-and-ship project mark");
            return 0;
        }
        crate::infra::IdSource::RepoFile => {}
    }

    let declared_at = crate::infra::find_project_file(cwd);
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let without_the_file = crate::infra::project_id_for_path(&canonical);
    let holdings = records_held_under(&without_the_file);

    match identity_disagreement(Some(&id), &without_the_file, &holdings) {
        None => {
            match &declared_at {
                Some(path) => println!(
                    "  [ OK ] project identity: {id}, declared in {}",
                    path.display()
                ),
                None => println!("  [ OK ] project identity: {id}"),
            }
            0
        }
        Some(found) => {
            println!(
                "  [WARN] project identity: declared as {}, but records here are keyed by {}",
                found.declared, found.detached
            );
            println!(
                "         {} still holds {} — written before this directory answered {}.",
                found.detached,
                found.summary(),
                found.declared
            );
            println!("         Nothing reaches them under the declared id.");
            match &declared_at {
                Some(path) => println!(
                    "         Fix: set the id back to {} in {}",
                    found.detached,
                    path.display()
                ),
                None => println!("         Fix: set the id back to {}", found.detached),
            }
            1
        }
    }
}

/// Read the think store and report which steps came from another project.
/// Think steps carry no origin stamp — their `cwd` is the signal, and it counts
/// as proof only when our own id is cwd-derived (see
/// [`crate::cli::store_health::cwd_attribution_is_proof`]).
fn report_think_store() -> usize {
    use crate::cli::store_health::{self, ThinkOrigin};

    let project_id = crate::infra::resolve_project_id(None);
    let cfg = crate::infra::PersistenceConfig::from_env();
    let persistence = crate::think::persistence::Persistence::for_project(
        &crate::infra::PersistenceConfig {
            enabled: cfg.enabled,
            data_dir: cfg.data_dir.clone(),
        },
        &project_id,
    );
    let Some(history) = persistence.load_default() else {
        println!("  [ -- ] think store: nothing saved yet for {project_id}");
        return 0;
    };

    let cwd_is_proof = store_health::cwd_attribution_is_proof(&project_id);
    if !cwd_is_proof {
        // A step's only origin signal is the cwd it was recorded in, so the
        // comparison works solely for a project whose id IS that hash. An
        // environment override and a declared identity both break it, and
        // naming one of them as the cause would be a guess — the identity row
        // above has already said which.
        println!(
            "  [ -- ] think store: this project's id is not derived from this directory, \
             so no step's origin can be proven — all {} kept",
            history.steps.len()
        );
        return 0;
    }

    let records: Vec<ThinkOrigin<'_>> = history
        .steps
        .iter()
        .map(|step| ThinkOrigin { step, cwd_is_proof })
        .collect();
    let report = store_health::inspect_records(&records, &project_id);
    report_origin("think", "step", &report, "think-and-ship prune think")
}

/// Read the signal store and report which signals came from another project.
fn report_signal_store() -> usize {
    use crate::cli::store_health;

    let project_id = crate::infra::resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(
        &crate::infra::PersistenceConfig::from_env(),
        crate::infra::Domain::Signal,
    );
    let Ok(Some(signals)) = persistence.load::<crate::signal::domain::Signals>(&project_id) else {
        println!("  [ -- ] signal store: nothing saved yet for {project_id}");
        return 0;
    };
    let report = store_health::inspect_records(&signals.signals, &project_id);
    report_origin("signal", "signal", &report, "think-and-ship prune signal")
}

/// Read the roadmap store and report what doesn't belong: chunks stamped with
/// another project, and deps naming chunks that aren't there. Read-only, in the
/// same four-glyph shape as the checks around it. Returns the number of issues.
fn report_roadmap_store() -> usize {
    use crate::cli::store_health;

    let project_id = crate::infra::resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(
        &crate::infra::PersistenceConfig::from_env(),
        crate::infra::Domain::Roadmap,
    );
    let Ok(Some(roadmap)) = persistence.load::<crate::roadmap::domain::Roadmap>(&project_id) else {
        println!("  [ -- ] roadmap store: nothing saved yet for {project_id}");
        return 0;
    };

    let report = store_health::inspect(&roadmap, &project_id);
    let mut issues = report_origin("roadmap", "chunk", &report, "think-and-ship roadmap prune");

    if !report.dangling_deps.is_empty() {
        println!(
            "  [WARN] roadmap store: {} dependency reference(s) point at chunks that don't exist",
            report.dangling_deps.len()
        );
        for (chunk, dep) in report.dangling_deps.iter().take(3) {
            println!("         {chunk} depends on {dep}");
        }
        println!(
            "         Fix: correct the deps with roadmap_update_chunk, or add the missing chunk"
        );
        issues += 1;
    }

    // Constraint C8, checked against the data rather than the code. The unit
    // tests prove the derivation cannot emit an over-budget name; only a read of
    // the real store can prove no chunk has acquired one another way — a hand
    // edit, a cloud pull, a store written by an older build.
    let unusable: Vec<(String, String)> = roadmap
        .chunks
        .iter()
        .filter_map(|c| crate::roadmap::name::why_unfit(&c.name).map(|why| (c.id.clone(), why)))
        .collect();
    if unusable.is_empty() {
        println!(
            "  [ OK ] roadmap store: all {} chunk(s) carry a node label within {} characters",
            roadmap.chunks.len(),
            crate::roadmap::name::NAME_BUDGET
        );
    } else {
        println!(
            "  [WARN] roadmap store: {} chunk(s) have no usable node label",
            unusable.len()
        );
        for (chunk, why) in unusable.iter().take(3) {
            println!("         {chunk}: {why}");
        }
        println!(
            "         Fix: give it a short one with roadmap_update_chunk(id, name: \"…\"), \
             or send an empty name to re-seed it from the id"
        );
        issues += 1;
    }

    // Constraint C7, and live data is the only place it can be checked. The unit
    // tests own the clauses; whether THIS roadmap's regions are places a person
    // could name is a fact about the store, and a new chunk landing without a
    // region is exactly how the map decayed the first time.
    let regions = crate::roadmap::region::audit(
        roadmap
            .chunks
            .iter()
            .map(|c| (c.id.as_str(), c.group.as_deref())),
    );
    let failures = regions.failures();
    if failures.is_empty() {
        println!(
            "  [ OK ] roadmap store: {} chunk(s) sit in {} named region(s), median {}",
            regions.total,
            regions.regions(),
            regions.median
        );
    } else {
        println!(
            "  [WARN] roadmap store: the region map breaks {} of its constraints",
            failures.len()
        );
        for why in failures.iter().take(3) {
            println!("         {why}");
        }
        println!(
            "         Fix: put the chunk somewhere with roadmap_set_group(id, group: \"…\"), \
             or re-author the map with `think-and-ship roadmap regions --file MAP --apply`"
        );
        issues += 1;
    }
    issues
}

/// What one authenticated call to the backend said about the stored credential.
///
/// Three outcomes, not two, and keeping them apart is the whole point.
/// "Rejected" is a fault the user must act on; "could not ask" is the network
/// having a bad day and must never be counted as one, or `doctor` becomes a
/// command that fails on an aeroplane.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CredentialHealth {
    /// The backend accepted it.
    Accepted,
    /// The backend answered and refused it. This is the state that was
    /// previously invisible: every offline signal says connected — a record
    /// exists, a token resolves — and then every operation fails.
    Rejected { status: u16 },
    /// The backend could not be asked. Not a fault.
    Unreachable,
    /// Nothing recorded or nothing stored; the offline surfaces already said so.
    NotConnected,
}

/// Spend one authenticated request to find out whether the stored credential
/// still works, and report it. Returns the issue count.
///
/// `doctor` is the one surface allowed to be slow. Every other consumer of the
/// credential discovers a dead one by failing, which tells the user that THIS
/// operation broke rather than that their connection is over.
fn report_cloud_credential() -> usize {
    let stored = crate::cloud::connection::load();
    let env = crate::cloud::config::EnvOverrides::from_env();
    let store = crate::cloud::credential::store_for(&data_root());
    let health = check_cloud_credential(
        crate::cloud::config::resolve_url(&env, stored.as_ref()).map(|(u, _)| u),
        crate::cloud::config::resolve_token(store.as_ref(), &env, stored.as_ref()).map(|(t, _)| t),
    );
    match health {
        CredentialHealth::NotConnected => {
            println!("  [ -- ] cloud credential: this project is not connected");
            0
        }
        CredentialHealth::Accepted => {
            println!("  [ OK ] cloud credential: the backend accepted it");
            0
        }
        CredentialHealth::Rejected { status } => {
            println!("  [FAIL] cloud credential: the backend REFUSED it ({status})");
            println!(
                "         Fix: this connection is over, not merely misconfigured. Run \
                 `think-and-ship connect` to replace it."
            );
            1
        }
        CredentialHealth::Unreachable => {
            println!("  [ -- ] cloud credential: could not reach the backend to check");
            0
        }
    }
}

/// The rule behind [`report_cloud_credential`], with the url and token supplied
/// so it can be driven without a keychain — and, more usefully, so the
/// "unreachable is not a fault" decision is testable without a network.
fn check_cloud_credential(url: Option<String>, token: Option<String>) -> CredentialHealth {
    let (Some(url), Some(token)) = (url, token) else {
        return CredentialHealth::NotConnected;
    };
    let probe = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(probe_cloud_credential(&url, &token)));
    match probe {
        Ok(health) => health,
        // No runtime is not a statement about the credential.
        Err(_) => CredentialHealth::Unreachable,
    }
}

/// One `GET /v1/records` with the Bearer, `since=now` so proving a credential
/// does not drag the tenant's history back.
async fn probe_cloud_credential(url: &str, token: &str) -> CredentialHealth {
    let http = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(http) => http,
        Err(_) => return CredentialHealth::Unreachable,
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let resp = http
        .get(format!("{}/v1/records", url.trim_end_matches('/')))
        .query(&[("since", now.as_str())])
        .bearer_auth(token)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => CredentialHealth::Accepted,
        Ok(r) if matches!(r.status().as_u16(), 401 | 403) => CredentialHealth::Rejected {
            status: r.status().as_u16(),
        },
        // A 5xx is the backend's problem, not the credential's. Saying "your
        // connection is over" on a deploy blip would send someone to re-run an
        // interactive sign-in for nothing.
        Ok(_) | Err(_) => CredentialHealth::Unreachable,
    }
}

/// One line of doctor's MCP-configuration report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IdeRow {
    glyph: &'static str,
    line: String,
    fix: Option<String>,
}

impl IdeRow {
    fn is_issue(&self) -> bool {
        matches!(self.glyph, "WARN" | "FAIL")
    }
}

/// doctor's view of this project's MCP configuration, as DATA.
///
/// Returned rather than printed so the rule itself can be tested, and the rule
/// is what changed. doctor used to inspect ONE guessed client's config file and
/// warn when that file did not exist. In a repository holding a stray `.cursor/`
/// that produced a standing warning about `.cursor/mcp.json` — a file nothing
/// needs — while the live entry sat in `.mcp.json` and worked perfectly. A
/// diagnostic whose only complaint is about a client you do not use teaches
/// people to ignore it.
///
/// Now: every config that HOLDS an entry is checked, and a present client
/// without one is reported as information, not as a fault. The single warning
/// left is the true one — nothing anywhere holds an entry.
fn ide_rows_in(cwd: &Path, home_config: Option<PathBuf>, env: &CallerEnv) -> Vec<IdeRow> {
    let mut rows = Vec::new();
    let present = present_hosts_in(cwd, env);

    // A config file that exists but cannot be parsed hides whatever is in it,
    // including an entry that would otherwise have been found.
    for host in &present {
        let path = cwd.join(host.config_file);
        if path.exists() && read_json_safe(&path).is_none() {
            rows.push(IdeRow {
                glyph: "FAIL",
                line: format!("{}: exists but invalid JSON", host.config_file),
                fix: Some("check syntax or run: think-and-ship init --force".into()),
            });
        }
    }

    let entries = matching_entries_in(cwd, home_config);
    for entry in &entries {
        let persist = entry
            .env_on_disk()
            .and_then(|env| {
                env.get("THINK_AND_SHIP_PERSIST")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .as_deref()
            == Some("true");
        if persist {
            rows.push(IdeRow {
                glyph: " OK ",
                line: format!("{}: configured (persistence on)", entry.describe()),
                fix: None,
            });
        } else {
            rows.push(IdeRow {
                glyph: "WARN",
                line: format!(
                    "{}: configured but THINK_AND_SHIP_PERSIST is not \"true\"",
                    entry.describe()
                ),
                fix: Some(
                    "state will not survive between sessions. Fix: think-and-ship init --force"
                        .into(),
                ),
            });
        }
    }

    // Present, no entry. Worth knowing, not worth a warning — the client may be
    // one the user has no intention of pointing at this project.
    //
    // Matched by HOST as well as by path, because a client's entry can live
    // somewhere other than its project config: Claude Code's commonly sits in
    // `~/.claude.json`, and reporting "Claude Code: present, no entry" directly
    // under "Claude Code: configured" is a contradiction on one screen.
    for host in &present {
        let path = cwd.join(host.config_file);
        if entries
            .iter()
            .any(|e| e.path == path || e.host == host.name)
        {
            continue;
        }
        rows.push(IdeRow {
            glyph: " -- ",
            line: format!("{}: present, no think-and-ship entry", host.name),
            fix: None,
        });
    }

    if entries.is_empty() {
        rows.push(IdeRow {
            glyph: "WARN",
            line: "no MCP config holds a think-and-ship entry for this project".into(),
            fix: Some("think-and-ship init".into()),
        });
    }
    rows
}

/// `think-and-ship doctor` — diagnose setup. Returns the issue count via exit
/// semantics in the printed summary (non-fatal: always `Ok`).
pub fn doctor() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let version = env!("CARGO_PKG_VERSION");
    println!("think-and-ship doctor v{version}\n");
    let mut issues = 0;

    // Binary: the running exe + whether the bare `think-and-ship` resolves on
    // PATH (the MCP config invokes it by bare name).
    match std::env::current_exe() {
        Ok(exe) => println!("  [ OK ] binary: v{version} ({})", exe.display()),
        Err(e) => {
            println!("  [WARN] could not resolve current executable: {e}");
            issues += 1;
        }
    }
    if let Some(on_path) = which_on_path("think-and-ship") {
        println!("  [ OK ] `think-and-ship` on PATH ({})", on_path.display());
    } else {
        println!("  [WARN] `think-and-ship` not found on PATH");
        println!("         The MCP config invokes it by bare name; add the install dir to PATH,");
        println!("         or use an absolute `command` in your .mcp.json.");
        issues += 1;
    }

    // Which tool families this deployment exposes. A narrowed surface is a
    // legitimate choice, but "my tool disappeared" must be diagnosable here
    // rather than by reading the environment by hand.
    match crate::mcp::unified::FamilySelection::from_env() {
        Ok(sel) if sel.is_all() => {
            println!("  [ OK ] tool families: all ({})", sel.summary());
        }
        Ok(sel) => {
            println!("  [ OK ] tool families: {} only", sel.summary());
            println!(
                "         Narrowed by {}. Unset it to expose every family.",
                crate::mcp::unified::FAMILIES_ENV
            );
        }
        Err(e) => {
            println!("  [FAIL] tool families: {e}");
            println!("         The server refuses to start with this value.");
            issues += 1;
        }
    }
    println!();

    // MCP configuration, across every client this project actually has.
    for row in ide_rows_in(&cwd, claude_home_config(), &CallerEnv::from_env()) {
        println!("  [{}] {}", row.glyph, row.line);
        if let Some(fix) = &row.fix {
            println!("         Fix: {fix}");
        }
        if row.is_issue() {
            issues += 1;
        }
    }
    println!();

    // Data partitions. Every family the server serves, from the family list
    // itself — hand-listing them is how `signal` went unchecked for its whole
    // life.
    let root = data_root();
    for family in crate::mcp::UnifiedFamily::ALL.map(|f| f.prefix()) {
        let dir = root.join(family).join("sessions");
        if dir.exists() {
            // Writability: a coarse check via metadata permissions.
            let writable = fs::metadata(&dir)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false);
            if writable {
                println!("  [ OK ] {family} sessions: {}", dir.display());
            } else {
                println!(
                    "  [FAIL] {family} sessions: {} (not writable)",
                    dir.display()
                );
                issues += 1;
            }
        } else {
            println!(
                "  [ -- ] {family} sessions: {} (created on first use)",
                dir.display()
            );
        }
    }
    println!();

    // WHICH project these stores belong to, before reporting what is in them.
    // Every count below is read from a store keyed by this id, so an identity
    // nothing agrees with makes the rest of the section describe the wrong
    // history — confidently, and in full.
    issues += report_project_identity(&cwd);

    // The roadmap store's contents, not just its directory. A store can be
    // perfectly writable and still hold another project's chunks — which is
    // exactly what happened, while doctor reported everything fine.
    issues += report_roadmap_store();
    issues += report_think_store();
    issues += report_signal_store();
    println!();

    // The cloud credential, SPENT rather than merely found.
    issues += report_cloud_credential();
    println!();

    // CLAUDE.md.
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.exists() {
        let has = fs::read_to_string(&claude_md)
            .map(|c| c.contains(CLAUDE_MD_MARKER))
            .unwrap_or(false);
        if has {
            println!("  [ OK ] CLAUDE.md: think-and-ship section present");
        } else {
            println!("  [ -- ] CLAUDE.md: exists, no think-and-ship section");
            println!("         Tip: think-and-ship init --with-claude-md");
        }
    } else {
        println!("  [ -- ] CLAUDE.md: not found");
        println!("         Tip: think-and-ship init --full");
    }
    println!();

    if issues > 0 {
        // Non-zero, so a gate can be a gate. Everything above prints its own
        // [WARN] line and its own Fix, and a caller that only reads stdout has
        // to match on a sentence; the exit code is the part a CI step, a hook or
        // a `&&` chain can act on without parsing prose.
        anyhow::bail!(
            "Found {issues} issue{}. See Fix suggestions above.",
            if issues > 1 { "s" } else { "" }
        );
    }
    println!("No issues found. Everything looks good.");
    Ok(())
}

/// `think-and-ship status` — project + config snapshot.
pub fn status() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let version = env!("CARGO_PKG_VERSION");
    println!("think-and-ship v{version}\n");

    let env = CallerEnv::from_env();
    let present = present_hosts_in(&cwd, &env);
    let project = detect_project(&cwd);
    let (project_id, id_source) = crate::infra::resolve_project_id_with(None, Some(&cwd));

    println!(
        "  Project:    {}",
        cwd.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    // WHERE the identity came from, not just what it is. "This is the project"
    // and "this is the project because a file says so" are different facts, and
    // the difference is exactly what a user needs when one repository answers
    // two ways from two directories.
    println!("  Project id: {project_id} (from {})", id_source.label());
    if id_source == crate::infra::IdSource::DerivedFromPath {
        println!(
            "              undeclared — this id changes if the directory moves. \
             Run `think-and-ship init` to declare it."
        );
    }
    println!("  Dir:        {}", cwd.display());
    // Every client here, not one guessed from whichever dotdir sorted first.
    if present.is_empty() {
        println!("  Clients:    none detected ({DEFAULT_AUTHORING_HOST} is the default)");
    } else {
        for (i, host) in present.iter().enumerate() {
            let label = if i == 0 { "Clients:   " } else { "           " };
            let caller = if identify_caller(&env).is_some_and(|c| c.name == host.name) {
                " — running this command"
            } else {
                ""
            };
            println!("  {label} {} ({}){caller}", host.name, host.config_file);
        }
    }
    match project {
        Some(p) => {
            println!("  Type:       {} ({})", p.name, p.marker);
            println!("  Verify:     {}", p.verify.join(", "));
        }
        None => println!("  Type:       unknown"),
    }
    println!();

    let entries = matching_entries_in(&cwd, claude_home_config());
    if entries.is_empty() {
        println!("  MCP config: no think-and-ship entry (run: think-and-ship init)");
    } else {
        for entry in &entries {
            let names: Vec<String> = read_json_safe(&entry.path)
                .and_then(|doc| {
                    entry
                        .servers(&doc)
                        .and_then(|s| s.as_object())
                        .map(|m| m.keys().cloned().collect())
                })
                .unwrap_or_default();
            println!(
                "  MCP servers in {}: {}",
                entry.describe(),
                names.join(", ")
            );
        }
    }

    let persist = crate::infra::PersistenceConfig::from_env().enabled;
    println!(
        "  Persistence: {} ({})",
        if persist { "on" } else { "off" },
        data_root().display()
    );
    print_cloud_status();
    Ok(())
}

/// Report the cloud connection, the way `gh auth status` does.
///
/// This surface did not exist, and could not have: before the connection was an
/// object there was nothing to read. A user could not answer "am I connected, to
/// what, as whom?" without decoding a keychain blob by hand.
///
/// It names WHERE each half resolved from, which is the part that earns its
/// place. "Connected" and "connected because this environment variable is set"
/// are different facts, and the difference is exactly what a user needs when a
/// machine behaves one way under an MCP host and another way in their own shell.
fn print_cloud_status() {
    let stored = crate::cloud::connection::load();
    let env = crate::cloud::config::EnvOverrides::from_env();
    let Some((url, url_source)) = crate::cloud::config::resolve_url(&env, stored.as_ref()) else {
        println!("  Cloud:      not connected (run: think-and-ship connect)");
        return;
    };

    println!("  Cloud:      {url} (from {})", url_source.label());
    if let Some(conn) = &stored {
        println!("              connected {}", conn.connected_at);
    }

    // Reading the token is the point of the check — a recorded connection whose
    // credential has gone missing looks connected from every other angle, and
    // that is the state a user most needs named. Failures are reported, never
    // fatal: `status` must still answer on a machine with no keychain.
    let store = crate::cloud::credential::store_for(&data_root());
    match crate::cloud::config::resolve_token(store.as_ref(), &env, stored.as_ref()) {
        Some((_, token_source)) => {
            let profile = crate::cloud::config::resolve_profile(&env, stored.as_ref())
                .map(|(p, _)| p)
                .unwrap_or_else(|| "-".to_string());
            println!(
                "              token: found in {} (profile {profile})",
                token_source.label()
            );
        }
        None => println!(
            "              token: MISSING — this project is recorded as connected but no \
             credential answers.\n              Run `think-and-ship connect` to restore it."
        ),
    }
}

/// Minimal PATH lookup for an executable named `name` (no external `which`).
fn which_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// An environment naming the given variables, built as a VALUE — no process
    /// environment is touched, so these run alongside every other test.
    fn env_naming(vars: &[&str]) -> CallerEnv {
        let set: Vec<String> = vars.iter().map(|v| (*v).to_string()).collect();
        CallerEnv::from_lookup(|key| set.iter().find(|v| *v == key).map(|_| "1".to_string()))
    }

    fn names_of(hosts: &[&'static HostTarget]) -> Vec<&'static str> {
        hosts.iter().map(|h| h.name).collect()
    }

    /// The single client configured. Asserts the count first, so a test written
    /// for one client can never quietly pass while a second is being written.
    fn only_write(writes: &ConnectWrites) -> &ClientWrite {
        assert_eq!(
            writes.configured.len(),
            1,
            "expected exactly one configured client: {:?}",
            writes.configured,
        );
        &writes.configured[0]
    }

    /// THE REPRODUCTION, exactly as reported. This repository holds `.cursor/`,
    /// `.vscode/`, `.windsurf/` AND `.mcp.json`, and `connect` run from a Claude
    /// Code session picked CURSOR — because Claude Code had no positive marker
    /// and was reachable only when every other marker was absent.
    ///
    /// The caller's own environment says who it is. Asking the filesystem
    /// instead is how the one client that is definitely running became the one
    /// client that could never be chosen.
    #[test]
    fn the_caller_names_itself_before_the_repository_is_asked_to_guess() {
        let tmp = TempDir::new().unwrap();
        for dir in [".cursor", ".vscode", ".windsurf"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        let claude = env_naming(&["CLAUDECODE"]);

        assert_eq!(
            identify_caller(&claude).map(|h| h.name),
            Some("Claude Code"),
            "CLAUDECODE in the environment IS the answer",
        );
        assert!(
            names_of(&authoring_hosts_in(tmp.path(), &claude)).contains(&"Claude Code"),
            "the client running the command must be configured: {:?}",
            names_of(&authoring_hosts_in(tmp.path(), &claude)),
        );

        // And the write follows the rule, not just the rule's return value.
        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &claude,
            "https://api.example",
            "acme-claude",
            false,
            false,
        )
        .unwrap();
        let authored: Value =
            serde_json::from_str(&fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            authored["mcpServers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-claude",
            "Claude Code's own config carries the wiring: {authored}",
        );
    }

    /// Cursor identifies itself the same way, and the innermost claim wins: an
    /// agent launched from another client's terminal inherits that client's
    /// marker, and the process actually running the command is the truer answer.
    #[test]
    fn a_nested_caller_resolves_to_the_process_actually_running() {
        assert_eq!(
            identify_caller(&env_naming(&["CURSOR_TRACE_ID"])).map(|h| h.name),
            Some("Cursor"),
        );
        assert_eq!(
            identify_caller(&env_naming(&["CURSOR_TRACE_ID", "CLAUDECODE"])).map(|h| h.name),
            Some("Claude Code"),
            "claude launched inside Cursor's terminal is a Claude Code connection",
        );
        assert_eq!(identify_caller(&CallerEnv::unknown()).map(|h| h.name), None);
    }

    /// No client may be selected by the ABSENCE of the others. An empty
    /// directory with an anonymous environment shows evidence of nobody, and the
    /// authoring target that follows is the NAMED DEFAULT — a different claim
    /// from "we detected Claude Code".
    #[test]
    fn no_client_is_selected_by_the_absence_of_the_others() {
        let tmp = TempDir::new().unwrap();
        let nobody = CallerEnv::unknown();

        assert!(
            present_hosts_in(tmp.path(), &nobody).is_empty(),
            "nothing here identifies any client",
        );
        assert!(authoring_is_default_in(tmp.path(), &nobody));
        assert_eq!(
            names_of(&authoring_hosts_in(tmp.path(), &nobody)),
            vec![DEFAULT_AUTHORING_HOST],
        );

        // Claude Code becomes PRESENT for a positive reason, never a negative
        // one: its own directory, or its own environment.
        fs::create_dir(tmp.path().join(".claude")).unwrap();
        assert_eq!(
            names_of(&present_hosts_in(tmp.path(), &nobody)),
            vec!["Claude Code"]
        );
        assert!(!authoring_is_default_in(tmp.path(), &nobody));
    }

    /// `.vscode/` is present in a huge share of repositories that never use VS
    /// Code's agent mode, so it must not become an authoring target. It is now
    /// REPORTED instead of silently dropped — present and unconfigured is a fact
    /// the user needs, and a client that is quietly skipped looks, from the
    /// terminal, exactly like a client that was set up.
    #[test]
    fn a_present_but_unauthorable_client_is_named_rather_than_skipped() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".vscode")).unwrap();
        let nobody = CallerEnv::unknown();

        assert_eq!(
            names_of(&present_hosts_in(tmp.path(), &nobody)),
            vec!["VS Code"]
        );
        assert_eq!(
            names_of(&authoring_hosts_in(tmp.path(), &nobody)),
            vec![DEFAULT_AUTHORING_HOST],
            ".vscode/ is still not an authoring signal",
        );

        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &nobody,
            "https://api.example",
            "acme-vs",
            false,
            false,
        )
        .unwrap();
        assert!(
            writes.manual.contains(&ManualStep::Unauthorable {
                host: "VS Code",
                config_file: ".vscode/mcp.json",
            }),
            "VS Code must be named as a manual step: {:?}",
            writes.manual,
        );
        assert!(
            !tmp.path().join(".vscode/mcp.json").exists(),
            "naming it must not mean writing it",
        );
    }

    /// A client that publishes nothing can still be named, and naming it is
    /// evidence like any other — it ADDS to the present set rather than
    /// replacing it, so `--client windsurf` in a Cursor repository configures
    /// both. Windsurf is the motivating case; every client we have never heard
    /// of is the reason the fix is a flag rather than another table row.
    #[test]
    fn a_client_that_cannot_be_detected_can_name_itself() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        let nobody = CallerEnv::unknown();

        assert_eq!(
            names_of(&present_hosts_in(tmp.path(), &nobody)),
            vec!["Cursor"],
            "precondition: nothing here says Windsurf",
        );

        let named = nobody.naming(&["windsurf".to_string()]).unwrap();
        assert_eq!(
            names_of(&present_hosts_in(tmp.path(), &named)),
            vec!["Cursor", "Windsurf"],
            "the named client joins the set in table order, it does not replace it",
        );

        // ...and it reaches the write, not just the rule.
        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &named,
            "https://api.example",
            "acme-named",
            false,
            false,
        )
        .unwrap();
        let configured: Vec<&str> = writes.configured.iter().map(|c| c.host).collect();
        assert!(configured.contains(&"Windsurf"), "{configured:?}");
        assert!(
            tmp.path().join(".windsurf/mcp.json").exists(),
            "a client that named itself must actually be configured",
        );
    }

    /// A flag that is silently ignored is worse than no flag: the user believes
    /// they solved the problem and the symptom does not change.
    #[test]
    fn an_unknown_client_name_is_refused_with_the_names_that_work() {
        let err = CallerEnv::unknown()
            .naming(&["emacs".to_string()])
            .expect_err("an unknown client must not be accepted in silence");
        let msg = err.to_string();
        assert!(msg.contains("emacs"), "names what was asked for: {msg}");
        for known in ["Claude Code", "Cursor", "Windsurf", "VS Code"] {
            assert!(msg.contains(known), "lists {known}: {msg}");
        }
    }

    /// doctor spends one authenticated call, and the three outcomes stay three.
    /// Collapsing "could not ask" into "rejected" would make `doctor` a command
    /// that reports a dead connection on an aeroplane; collapsing it the other
    /// way would hide the state this check exists to surface.
    #[test]
    fn an_unreachable_backend_is_not_a_dead_credential() {
        assert_eq!(
            check_cloud_credential(None, Some("tok".into())),
            CredentialHealth::NotConnected,
            "no url is not a credential verdict",
        );
        assert_eq!(
            check_cloud_credential(Some("https://api.example".into()), None),
            CredentialHealth::NotConnected,
            "no token is not a credential verdict",
        );
        // A port nothing is listening on: the request cannot complete, and that
        // is emphatically not the backend refusing the credential.
        assert_eq!(
            check_cloud_credential(
                Some("http://127.0.0.1:1".into()),
                Some("cloud_tok_whatever".into())
            ),
            CredentialHealth::Unreachable,
            "a transport failure must never read as a rejection",
        );
    }

    /// THE FALSE WARN, as it stood in this very repository. `.cursor/` exists,
    /// the live entry is in `.mcp.json` and works, and doctor's only complaint
    /// was that `.cursor/mcp.json` — a file nothing needs — was missing.
    ///
    /// A diagnostic whose one warning is about a client you do not use teaches
    /// people to ignore the diagnostic. The entry does not move; the complaint
    /// goes away.
    #[test]
    fn doctor_stops_warning_about_a_client_it_merely_guessed() {
        let tmp = TempDir::new().unwrap();
        for dir in [".cursor", ".vscode", ".windsurf"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        seed_entry(&tmp.path().join(".mcp.json"), ServerContainer::McpServers);
        let claude = env_naming(&["CLAUDECODE"]);

        let rows = ide_rows_in(tmp.path(), None, &claude);
        let issues: Vec<&IdeRow> = rows.iter().filter(|r| r.is_issue()).collect();
        assert!(
            issues.is_empty(),
            "a project whose live entry is configured has no issue to report: {issues:?}",
        );
        assert!(
            rows.iter()
                .any(|r| r.glyph == " OK " && r.line.contains(".mcp.json")),
            "the entry that actually exists is the one reported OK: {rows:?}",
        );
        assert!(
            rows.iter()
                .any(|r| r.glyph == " -- " && r.line.contains("Cursor")),
            "a present client without an entry is information, not a fault: {rows:?}",
        );

        // A client whose entry lives somewhere other than its project config —
        // Claude Code's commonly sits in `~/.claude.json` — must not be reported
        // as configured AND as having no entry, two lines apart, on one screen.
        let home = TempDir::new().unwrap();
        let home_config = seed_home_entry(home.path());
        let home_rows = ide_rows_in(home.path(), Some(home_config), &claude);
        assert!(
            !home_rows
                .iter()
                .any(|r| r.line.starts_with("Claude Code: present")),
            "a client reported configured must not also be reported entry-less: {home_rows:?}",
        );

        // The true warning survives: nothing anywhere holds an entry.
        let bare = TempDir::new().unwrap();
        let bare_rows = ide_rows_in(bare.path(), None, &CallerEnv::unknown());
        assert!(
            bare_rows.iter().any(|r| r.is_issue()),
            "no entry anywhere is still worth warning about: {bare_rows:?}",
        );
    }

    /// STOP PICKING ONE. Two clients on one repository is the normal case, and
    /// the old rule was forced to answer wrongly for one of them every time.
    #[test]
    fn every_present_client_is_configured_not_the_first_one_in_the_table() {
        let tmp = TempDir::new().unwrap();
        for dir in [".cursor", ".windsurf", ".claude"] {
            fs::create_dir(tmp.path().join(dir)).unwrap();
        }

        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example",
            "acme-all",
            false,
            false,
        )
        .unwrap();

        let configured: Vec<&str> = writes.configured.iter().map(|c| c.host).collect();
        for host in ["Cursor", "Windsurf", "Claude Code"] {
            assert!(
                configured.contains(&host),
                "{host} is present and must be configured: {configured:?}",
            );
        }
        for rel in [".cursor/mcp.json", ".windsurf/mcp.json", ".mcp.json"] {
            let written: Value =
                serde_json::from_str(&fs::read_to_string(tmp.path().join(rel)).unwrap()).unwrap();
            assert_eq!(
                written["mcpServers"][MCP_SERVER_NAME]["env"]
                    [crate::cloud::credential::PROFILE_ENV],
                "acme-all",
                "{rel} must carry the same wiring as every other client",
            );
        }
    }

    #[test]
    fn detect_project_matches_markers() {
        let tmp = TempDir::new().unwrap();
        assert!(detect_project(tmp.path()).is_none());
        fs::write(tmp.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(detect_project(tmp.path()).unwrap().name, "Rust");
    }

    // ── connect writes where the agent actually reads ────────────────────────

    /// Seed a think-and-ship entry at `path` under `container`, always alongside a
    /// foreign server AND an unrelated sibling top-level key — so every write
    /// assertion below doubles as a preservation assertion.
    fn seed_entry(path: &std::path::Path, container: ServerContainer) {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).unwrap();
        }
        let mut servers = serde_json::Map::new();
        servers.insert(MCP_SERVER_NAME.to_string(), mcp_server_config());
        servers.insert(
            "someone-elses-server".to_string(),
            json!({ "command": "other" }),
        );
        let mut doc = serde_json::Map::new();
        doc.insert(container.key().to_string(), Value::Object(servers));
        doc.insert("aaa_unrelated_key".to_string(), json!({ "kept": true }));
        fs::write(
            path,
            serde_json::to_string_pretty(&Value::Object(doc)).unwrap(),
        )
        .unwrap();
    }

    /// Seed the shape `claude mcp add` writes: nested under
    /// `projects."<canonical cwd>".mcpServers` in a user-level config.
    fn seed_home_entry(cwd: &std::path::Path) -> PathBuf {
        let home = cwd.join("home.claude.json");
        let key = cwd.canonicalize().unwrap().display().to_string();
        let mut servers = serde_json::Map::new();
        servers.insert(MCP_SERVER_NAME.to_string(), mcp_server_config());
        let mut project = serde_json::Map::new();
        project.insert("mcpServers".to_string(), Value::Object(servers));
        let mut projects = serde_json::Map::new();
        projects.insert(key, Value::Object(project));
        let mut doc = serde_json::Map::new();
        doc.insert("projects".to_string(), Value::Object(projects));
        fs::write(
            &home,
            serde_json::to_string_pretty(&Value::Object(doc)).unwrap(),
        )
        .unwrap();
        home
    }

    /// Each host's own config resolves to itself, carrying the container key and
    /// the host name that config actually uses.
    #[test]
    fn resolution_finds_the_host_that_actually_holds_the_entry() {
        for (rel, container, host) in [
            (".mcp.json", ServerContainer::McpServers, "Claude Code"),
            (".cursor/mcp.json", ServerContainer::McpServers, "Cursor"),
            (".vscode/mcp.json", ServerContainer::Servers, "VS Code"),
            (
                ".windsurf/mcp.json",
                ServerContainer::McpServers,
                "Windsurf",
            ),
        ] {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join(rel);
            seed_entry(&path, container);
            match resolve_host_in(tmp.path(), None) {
                HostResolution::Resolved(e) => {
                    assert_eq!(e.path, path, "{rel} must resolve to itself");
                    assert_eq!(e.container, container, "{rel} container key");
                    assert_eq!(e.host, host, "{rel} host name");
                }
                other => panic!("{rel}: expected Resolved, got {other:?}"),
            }
        }
    }

    /// THE REGRESSION THIS TEST EXISTS TO PREVENT. A repository containing a
    /// `.cursor/` directory made the marker-dir guess resolve to
    /// `.cursor/mcp.json`, so `connect` wrote the cloud credential there while the
    /// live entry — the one the agent actually reads — sat in the root
    /// `.mcp.json`. The user was told "Connected" and their agent stayed local.
    ///
    /// The live entry receiving the wiring is the load-bearing half and it is
    /// unchanged. What is no longer asserted is that `.cursor/mcp.json` goes
    /// unwritten: Cursor is present here, so it gets the SAME entry rather than
    /// the live one's entry, and no client is left holding a stale answer.
    #[test]
    fn connect_updates_the_live_entry_wherever_it_lives() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".cursor")).unwrap();
        let live = tmp.path().join(".mcp.json");
        seed_entry(&live, ServerContainer::McpServers);

        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example",
            "acme-live",
            false,
            true,
        )
        .unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&live).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-live",
            "the LIVE entry received the cloud wiring"
        );
        let cursor: Value =
            serde_json::from_str(&fs::read_to_string(tmp.path().join(".cursor/mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            cursor["mcpServers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-live",
            "the other present client gets the same entry, never a divergent one",
        );
        assert_eq!(
            after["mcpServers"]["someone-elses-server"]["command"], "other",
            "unrelated server preserved"
        );
        assert_eq!(
            after["aaa_unrelated_key"]["kept"], true,
            "unrelated top-level key preserved"
        );
    }

    /// VS Code keeps its servers under `servers`, not `mcpServers`. An entry
    /// written under the wrong key is valid JSON the host never sees.
    #[test]
    fn connect_writes_the_vscode_entry_under_the_servers_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".vscode/mcp.json");
        seed_entry(&path, ServerContainer::Servers);

        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example",
            "acme-vs",
            false,
            true,
        )
        .unwrap();

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            after["servers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-vs",
            "the cloud wiring lands under `servers`"
        );
        assert!(
            after.get("mcpServers").is_none(),
            "no `mcpServers` key may be invented in a VS Code config"
        );
        assert_eq!(
            after["servers"]["someone-elses-server"]["command"], "other",
            "unrelated server preserved"
        );
        assert_eq!(after["aaa_unrelated_key"]["kept"], true, "sibling key kept");
    }

    /// Nothing to update and no authorable client present is a reason to author
    /// at the NAMED DEFAULT. A `.vscode/` marker must not become the authoring
    /// target — see `HostTarget::authorable`.
    #[test]
    fn no_entry_authors_at_the_authoring_default_and_nowhere_else() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(".vscode")).unwrap();
        assert_eq!(resolve_host_in(tmp.path(), None), HostResolution::NoEntry);

        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example",
            "acme-new",
            false,
            false,
        )
        .unwrap();
        assert!(
            writes.used_default,
            "reaching the default must be reported as a default, not as a detection",
        );

        let authored = tmp.path().join(".mcp.json");
        assert!(authored.exists(), "authoring falls back to .mcp.json");
        assert!(
            !tmp.path().join(".vscode/mcp.json").exists(),
            "a .vscode/ marker must not become an authoring target"
        );
        let after: Value = serde_json::from_str(&fs::read_to_string(&authored).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-new"
        );
    }

    /// The client name the minted token carries. The CALLER answers first — a
    /// command run by Claude Code is a Claude Code connection whatever else the
    /// repository holds — and `None` survives for the case with no single
    /// answer, because a guessed client name is stamped onto the connection
    /// object in the app for the life of the token.
    #[test]
    fn the_client_label_follows_the_caller_then_the_live_entry() {
        // A live entry names its own host when nothing else does.
        let live = TempDir::new().unwrap();
        seed_entry(
            &live.path().join(".cursor/mcp.json"),
            ServerContainer::McpServers,
        );
        assert_eq!(
            client_label_in(live.path(), None, &CallerEnv::unknown()),
            Some("Cursor")
        );

        // ...and the caller overrides it, because the caller is not a guess.
        assert_eq!(
            client_label_in(live.path(), None, &env_naming(&["CLAUDE_CODE_SESSION_ID"])),
            Some("Claude Code"),
        );

        // Nothing to update and nothing present: the named default is the single
        // authoring target, so it IS the answer rather than an unknown.
        let fresh = TempDir::new().unwrap();
        assert_eq!(
            client_label_in(fresh.path(), None, &CallerEnv::unknown()),
            Some("Claude Code")
        );

        // Two live entries and an anonymous caller: no single client to name.
        let both = TempDir::new().unwrap();
        seed_entry(&both.path().join(".mcp.json"), ServerContainer::McpServers);
        seed_entry(
            &both.path().join(".cursor/mcp.json"),
            ServerContainer::McpServers,
        );
        assert_eq!(
            client_label_in(both.path(), None, &CallerEnv::unknown()),
            None
        );
    }

    /// Two live entries used to be a hard error, because writing to one would
    /// leave the other stale and which one the agent reads is the host's
    /// decision. Writing to BOTH answers that on its own terms: neither is
    /// stale, and the host's decision no longer changes the outcome.
    #[test]
    fn two_live_entries_are_both_brought_up_to_date() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".mcp.json");
        let cursor = tmp.path().join(".cursor/mcp.json");
        seed_entry(&root, ServerContainer::McpServers);
        seed_entry(&cursor, ServerContainer::McpServers);

        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example",
            "tok-x",
            false,
            true,
        )
        .expect("two entries is not a reason to refuse");
        let configured: Vec<&str> = writes.configured.iter().map(|c| c.host).collect();
        assert!(
            configured.contains(&"Claude Code") && configured.contains(&"Cursor"),
            "both live entries configured: {configured:?}",
        );

        for p in [&root, &cursor] {
            let after: Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
            assert_eq!(
                after["mcpServers"][MCP_SERVER_NAME]["env"][crate::cloud::credential::PROFILE_ENV],
                "tok-x",
                "{} must carry the new wiring rather than be left stale",
                p.display()
            );
            assert!(
                after["mcpServers"][MCP_SERVER_NAME]["env"]
                    .get("THINK_AND_SHIP_CLOUD_TOKEN")
                    .is_none(),
                "{} must still hold no secret",
                p.display()
            );
        }
    }

    /// The commonest Claude Code setup: `claude mcp add` put the entry in the
    /// user-level config, which `is_ours_to_write` deliberately refuses to rewrite.
    /// Refusing is not a dead end — that file has a CLI that edits it safely, so
    /// the outcome is one named command rather than a silent write elsewhere.
    #[test]
    fn an_entry_in_the_user_level_config_is_delegated_to_the_host_command() {
        let tmp = TempDir::new().unwrap();
        let home = seed_home_entry(tmp.path());

        let entry = match resolve_host_in(tmp.path(), Some(home.clone())) {
            HostResolution::Resolved(e) => e,
            other => panic!("expected Resolved, got {other:?}"),
        };
        assert!(
            !entry.is_ours_to_write(),
            "precondition: not ours to rewrite"
        );

        let server = cloud_server_config("https://api.example", "tok-home");
        match plan_registration(&entry, &server).unwrap() {
            Registration::HostCommand { command, .. } => {
                assert!(
                    command.starts_with(&format!(
                        "claude mcp remove {MCP_SERVER_NAME} --scope local"
                    )),
                    "removes first — add-json refuses an existing name, and the entry \
                     provably exists or we would not be here: {command}"
                );
                assert!(
                    command.contains(&format!("claude mcp add-json {MCP_SERVER_NAME} '")),
                    "uses the host's own registration command: {command}"
                );
                assert!(
                    command.ends_with("' --scope local"),
                    "the project-keyed section IS local scope: {command}"
                );
                assert!(
                    command.contains("tok-home"),
                    "carries the credential so nothing must be retyped: {command}"
                );
            }
            other => panic!("expected HostCommand, got {other:?}"),
        }

        // With no other client present there is nothing left to configure, so
        // the corrective command is the whole answer and the command fails.
        let before = fs::read_to_string(&home).unwrap();
        let err = write_cloud_mcp_config_in(
            tmp.path(),
            Some(home.clone()),
            &CallerEnv::unknown(),
            "https://api.example",
            "tok-home",
            false,
            true,
        )
        .expect_err("this file must not be rewritten by us");
        assert!(
            err.to_string().contains("claude mcp add-json"),
            "names the single corrective action: {err}"
        );
        assert_eq!(
            fs::read_to_string(&home).unwrap(),
            before,
            "the user-level config is left byte-identical"
        );
    }

    /// Seed a user-level config whose entry ALREADY carries the exact cloud
    /// wiring `connect` would write, so the "nothing to do" path can be tested.
    fn seed_home_entry_with(cwd: &std::path::Path, server: Value) -> PathBuf {
        let home = cwd.join("home.claude.json");
        let key = cwd.canonicalize().unwrap().display().to_string();
        let mut servers = serde_json::Map::new();
        servers.insert(MCP_SERVER_NAME.to_string(), server);
        let mut project = serde_json::Map::new();
        project.insert("mcpServers".to_string(), Value::Object(servers));
        let mut projects = serde_json::Map::new();
        projects.insert(key, Value::Object(project));
        let mut doc = serde_json::Map::new();
        doc.insert("projects".to_string(), Value::Object(projects));
        fs::write(
            &home,
            serde_json::to_string_pretty(&Value::Object(doc)).unwrap(),
        )
        .unwrap();
        home
    }

    /// A host-managed entry that ALREADY says exactly what we would say is not a
    /// reason to fail.
    ///
    /// This is the regression that made `connect` impossible to finish under
    /// Claude Code. `plan_registration` never read the file — it saw only that
    /// the config was not ours to rewrite and returned a corrective command, so
    /// the caller bailed. But `connect` reaches config-writing only AFTER the
    /// browser device flow, and a failure there rolls the mint back. So the user
    /// completed a full sign-in, was handed a command that would change nothing,
    /// and ended with no credential — repeatably, forever, because running the
    /// command could not make an already-correct entry any more correct.
    #[test]
    fn a_host_managed_entry_that_is_already_correct_is_success_not_a_command() {
        let tmp = TempDir::new().unwrap();
        let server = cloud_server_config("https://api.example", "profile-x");
        let home = seed_home_entry_with(tmp.path(), server.clone());

        let entry = match resolve_host_in(tmp.path(), Some(home.clone())) {
            HostResolution::Resolved(e) => e,
            other => panic!("expected Resolved, got {other:?}"),
        };
        assert!(
            !entry.is_ours_to_write(),
            "precondition: the file is still one we refuse to rewrite"
        );

        match plan_registration(&entry, &server).unwrap() {
            Registration::AlreadyCurrent { .. } => {}
            other => panic!("an already-correct entry needs no command, got {other:?}"),
        }

        let before = fs::read_to_string(&home).unwrap();
        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            Some(home.clone()),
            &CallerEnv::unknown(),
            "https://api.example",
            "profile-x",
            false,
            true,
        )
        .expect("an already-correct entry must not abort connect");
        assert_eq!(
            writes.configured,
            vec![ClientWrite {
                host: "Claude Code",
                outcome: WriteOutcome::AlreadyCurrent,
            }]
        );
        assert_eq!(
            fs::read_to_string(&home).unwrap(),
            before,
            "still byte-identical — succeeding must not mean we rewrote it"
        );
    }

    /// The refusal must SURVIVE the fix above: an entry that genuinely differs
    /// still gets the corrective command, not a silent rewrite of a file holding
    /// every project the user has.
    #[test]
    fn a_host_managed_entry_that_differs_is_still_refused() {
        let tmp = TempDir::new().unwrap();
        let home = seed_home_entry_with(
            tmp.path(),
            cloud_server_config("https://api.example", "STALE-profile"),
        );

        let entry = match resolve_host_in(tmp.path(), Some(home.clone())) {
            HostResolution::Resolved(e) => e,
            other => panic!("expected Resolved, got {other:?}"),
        };
        let server = cloud_server_config("https://api.example", "profile-x");
        match plan_registration(&entry, &server).unwrap() {
            Registration::HostCommand { command, .. } => {
                assert!(command.contains("profile-x"), "carries the NEW value");
            }
            other => panic!("a differing entry must still be delegated, got {other:?}"),
        }
    }

    /// Build a ServerEntry for a plain project config, the shape most tests want.
    fn at(path: &std::path::Path) -> ServerEntry {
        ServerEntry {
            path: path.to_path_buf(),
            project_key: None,
            container: ServerContainer::McpServers,
            host: "Claude Code",
        }
    }

    /// THE reason `merge_server_env` exists rather than reusing
    /// `write_server_config`: that one REPLACES the entry, which would have
    /// silently deleted the cloud credentials of everyone who had run `connect`.
    /// "I turned on auto-push" must never mean "my cloud sync stopped".
    #[test]
    fn merging_one_env_var_preserves_a_cloud_configured_entry() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        let original = json!({
            "mcpServers": {
                MCP_SERVER_NAME: cloud_server_config("https://api.example", "acme-123"),
                "some-other-server": { "command": "other" }
            }
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        let merged =
            merge_server_env(&at(&path), "THINK_AND_SHIP_TRACKER_PUSH_SECS", "300", false).unwrap();
        assert_eq!(merged, EnvMerge::Written { previous: None });

        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let env = &after["mcpServers"][MCP_SERVER_NAME]["env"];
        assert_eq!(
            env["THINK_AND_SHIP_TRACKER_PUSH_SECS"], "300",
            "the new var"
        );
        // The disaster this guards did not change when the secret moved: losing
        // the PROFILE name breaks the agent's cloud sync exactly as losing the
        // token did, because the name is now what resolves the credential.
        assert_eq!(
            env[crate::cloud::credential::PROFILE_ENV],
            "acme-123",
            "the cloud profile MUST survive — losing it is the disaster this guards"
        );
        assert_eq!(env["THINK_AND_SHIP_SYNC_TARGET"], "cloud");
        assert_eq!(env["THINK_AND_SHIP_PERSIST"], "true");
        assert_eq!(
            after["mcpServers"]["some-other-server"]["command"], "other",
            "other servers are untouched"
        );
        assert_eq!(
            after["mcpServers"][MCP_SERVER_NAME]["args"],
            json!(["serve"]),
            "command and args are untouched"
        );
    }

    #[test]
    fn merging_reports_an_unchanged_value_rather_than_rewriting_it() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "mcpServers": { MCP_SERVER_NAME: { "env": { "K": "5" } } }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            merge_server_env(&at(&path), "K", "5", false).unwrap(),
            EnvMerge::AlreadySet,
            "an unchanged value must not be reported as work done"
        );
        assert_eq!(
            merge_server_env(&at(&path), "K", "9", false).unwrap(),
            EnvMerge::Written {
                previous: Some("5".into())
            },
            "and a change reports what it replaced"
        );
    }

    #[test]
    fn merging_refuses_to_invent_a_server_entry() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nothing.json");
        assert_eq!(
            merge_server_env(&at(&missing), "K", "1", false).unwrap(),
            EnvMerge::NoServerEntry,
            "no file means no entry — creating one would duplicate `init`"
        );
        assert!(!missing.exists(), "and it must not create the file either");

        let foreign = tmp.path().join("foreign.json");
        fs::write(&foreign, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();
        assert_eq!(
            merge_server_env(&at(&foreign), "K", "1", false).unwrap(),
            EnvMerge::NoServerEntry,
            "somebody else's config is not ours to add ourselves to"
        );
    }

    #[test]
    fn a_dry_run_merge_touches_no_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        let before = format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "mcpServers": { MCP_SERVER_NAME: { "env": { "THINK_AND_SHIP_PERSIST": "true" } } }
            }))
            .unwrap()
        );
        fs::write(&path, &before).unwrap();

        let merged = merge_server_env(&at(&path), "K", "1", true).unwrap();
        assert_eq!(
            merged,
            EnvMerge::Written { previous: None },
            "a dry run still REPORTS what it would do"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before,
            "but the bytes on disk are identical"
        );
    }

    /// THE BUG THIS FIXES, reproduced from the real repo that exposed it.
    ///
    /// think-and-ship's own checkout has a `.cursor/` directory holding somebody
    /// else's servers, a `.mcp.json` likewise, and the LIVE think-and-ship entry
    /// in `~/.claude.json`. `detect_ide` picks by marker directory, so the old
    /// path GUESS resolved to `.cursor/mcp.json`, found no entry, and told the
    /// user to run `init` — which would have created a fourth config instead of
    /// touching the one actually in use.
    #[test]
    fn the_entry_is_found_by_searching_not_by_guessing_the_ide() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path();
        // A .cursor dir exists and wins `detect_ide`...
        fs::create_dir_all(cwd.join(".cursor")).unwrap();
        fs::write(
            cwd.join(".cursor/mcp.json"),
            r#"{"mcpServers":{"ministr":{"command":"ministr"}}}"#,
        )
        .unwrap();
        // ...but OUR entry is in .mcp.json.
        fs::write(
            cwd.join(".mcp.json"),
            serde_json::to_string(&json!({
                "mcpServers": { MCP_SERVER_NAME: { "env": { "THINK_AND_SHIP_PERSIST": "true" } } }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            ide_config_path(cwd),
            cwd.join(".cursor/mcp.json"),
            "the guess still points at Cursor — that is what made this a bug"
        );
        let found = find_server_entry(cwd).expect("our entry must be FOUND, not guessed at");
        assert_eq!(
            found.path,
            cwd.join(".mcp.json"),
            "the search must land on the config that actually holds our entry, \
             not on the first IDE marker that happens to exist"
        );
    }

    /// THE REFUSAL, and the measurement behind it.
    ///
    /// A nested `~/.claude.json` hit is FOUND (so we can tell the human exactly
    /// where to look) but never REWRITTEN. This test also demonstrates why, on a
    /// document shaped like the real one: a full `Value` round trip sorts keys
    /// and shifts floats, neither of which is acceptable collateral for setting
    /// one variable in a file holding every project a user has.
    #[test]
    fn an_external_config_is_found_but_never_rewritten() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join(".claude.json");
        // Key order chosen so a BTreeMap round trip would visibly re-sort it,
        // and a float that does not survive parse/serialize bit-exactly.
        let original = r#"{
  "zzz_last": 1,
  "projects": {
    "/p": {
      "lastCost": 1.6952739999999997,
      "mcpServers": { "think-and-ship": { "env": { "THINK_AND_SHIP_PERSIST": "true" } } }
    }
  },
  "aaa_first": 2
}"#;
        fs::write(&home, original).unwrap();

        let entry = ServerEntry {
            path: home.clone(),
            project_key: Some("/p".to_string()),
            container: ServerContainer::McpServers,
            host: "Claude Code",
        };
        assert!(
            !entry.is_ours_to_write(),
            "a nested per-project entry is somebody else's file"
        );

        let merged = merge_server_env(&entry, "THINK_AND_SHIP_TRACKER_PUSH_SECS", "300", false)
            .expect("refusing is not an error");
        match merged {
            EnvMerge::ExternalConfig { at } => assert!(
                at.contains(".claude.json"),
                "the refusal must name the file the human has to edit: {at}"
            ),
            other => panic!("must refuse to rewrite an external config, got {other:?}"),
        }
        assert_eq!(
            fs::read_to_string(&home).unwrap(),
            original,
            "THE POINT: not one byte of the user's global config may change"
        );

        // And here is the damage that refusal avoids, measured rather than
        // asserted from memory.
        let round_tripped =
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(original).unwrap())
                .unwrap();
        assert!(
            round_tripped.find("aaa_first") < round_tripped.find("zzz_last"),
            "a Value round trip re-sorts object keys — this is what we refuse to \
             inflict on 151 projects"
        );
        assert!(
            !round_tripped.contains("1.6952739999999997"),
            "a Value round trip also shifts this float — measured on the real \
             config as 157 such changes"
        );
    }

    /// The write target is now the user's whole Claude configuration, so a
    /// half-written file is unacceptable. Rename is atomic; truncate-in-place is
    /// not.
    #[test]
    fn writes_go_through_a_rename_and_leave_no_temp_behind() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "mcpServers": { MCP_SERVER_NAME: { "env": {} } }
            }))
            .unwrap(),
        )
        .unwrap();

        merge_server_env(&at(&path), "K", "1", false).unwrap();

        let strays: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .filter(|n| n.contains("ts-tmp"))
            .collect();
        assert!(strays.is_empty(), "temp file left behind: {strays:?}");
        let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["mcpServers"][MCP_SERVER_NAME]["env"]["K"], "1");
    }

    #[test]
    fn server_config_bakes_in_persistence() {
        let cfg = mcp_server_config();
        assert_eq!(cfg["command"], "think-and-ship");
        assert_eq!(cfg["args"][0], "serve");
        assert_eq!(cfg["env"]["THINK_AND_SHIP_PERSIST"], "true");
    }

    #[test]
    fn cloud_server_config_bakes_in_write_through_sync() {
        let cfg = cloud_server_config("https://api.example.workers.dev", "acme-profile");
        assert_eq!(cfg["command"], "think-and-ship");
        assert_eq!(cfg["args"][0], "serve");
        let env = &cfg["env"];
        assert_eq!(env["THINK_AND_SHIP_PERSIST"], "true");
        assert_eq!(env["THINK_AND_SHIP_SYNC_TARGET"], "cloud");
        assert_eq!(
            env["THINK_AND_SHIP_CLOUD_URL"],
            "https://api.example.workers.dev"
        );
        assert_eq!(
            env[crate::cloud::credential::PROFILE_ENV],
            "acme-profile",
            "the entry names the profile the server resolves"
        );
    }

    /// ACCEPTANCE: the written entry carries no secret, by construction.
    ///
    /// Asserted over the WHOLE serialized document rather than by naming the key
    /// that used to hold it, because the failure being guarded against is a token
    /// reaching the config under ANY key — including one added later by someone
    /// who did not read this test.
    #[test]
    fn the_written_cloud_entry_contains_no_secret_anywhere() {
        let secret = "cloud_tok_this_must_never_be_written";
        let cfg = cloud_server_config("https://api.example.workers.dev", "acme-profile");
        let serialized = serde_json::to_string(&cfg).unwrap();

        assert!(
            !serialized.contains(secret),
            "the config must not carry the token: {serialized}"
        );
        assert!(
            !serialized.contains(crate::cloud::credential::TOKEN_ENV),
            "the plaintext token KEY must not appear either — an empty \
             THINK_AND_SHIP_CLOUD_TOKEN is still an invitation to paste one in: \
             {serialized}"
        );
        // And the positive half, so this cannot pass against an entry that simply
        // failed to configure anything: the wiring that arms sync is present.
        assert_eq!(cfg["env"]["THINK_AND_SHIP_SYNC_TARGET"], "cloud");
        assert_eq!(
            cfg["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-profile"
        );
    }

    /// THE REPORTED DEFECT, DRIVEN THE WAY A USER MEETS IT. `init` authors a
    /// LOCAL entry; the very next thing the docs tell that user to run is
    /// `connect`, with no flag. The old code found the entry, saw a bool that
    /// only said "something is there", declined the write and returned Ok — and
    /// the caller announced a connection over a config that could never sync.
    ///
    /// Asserted on the WRITTEN JSON rather than on the printed message, because
    /// the printed message is precisely the thing that was lying.
    #[test]
    fn init_then_connect_upgrades_the_local_entry_instead_of_declining_it() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");

        // Step one, exactly as `init` does it.
        write_mcp_config(
            &path,
            ".mcp.json",
            ServerContainer::McpServers,
            false,
            false,
        )
        .unwrap();
        let after_init: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let init_env = &after_init["mcpServers"]["think-and-ship"]["env"];
        assert!(
            init_env.get("THINK_AND_SHIP_CLOUD_URL").is_none(),
            "precondition: the entry init writes has no cloud wiring, which is \
             what makes the upgrade necessary: {after_init}",
        );

        // Step two, exactly as `connect` does it — NO force.
        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example.workers.dev",
            "acme-profile",
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            only_write(&writes),
            &ClientWrite {
                host: "Claude Code",
                outcome: WriteOutcome::Updated,
            }
        );
        let after_connect: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let env = &after_connect["mcpServers"]["think-and-ship"]["env"];
        assert_eq!(
            env["THINK_AND_SHIP_CLOUD_URL"], "https://api.example.workers.dev",
            "the upgraded entry must name the backend: {after_connect}",
        );
        assert_eq!(
            env["THINK_AND_SHIP_SYNC_TARGET"], "cloud",
            "an entry without write-through sync syncs nothing: {after_connect}",
        );
        assert_eq!(
            env[crate::cloud::credential::PROFILE_ENV],
            "acme-profile",
            "without the profile the server cannot resolve the token: {after_connect}",
        );
    }

    /// The one shape where declining IS correct, and the reason the fix is a
    /// four-way comparison rather than "always overwrite": a reconnect that
    /// would write exactly what is already there must not touch the file, and
    /// must not claim it changed anything.
    #[test]
    fn an_entry_that_already_matches_is_left_byte_for_byte_alone() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        let args = ("https://api.example.workers.dev", "acme-profile");

        let anon = CallerEnv::unknown();
        let first =
            write_cloud_mcp_config_in(tmp.path(), None, &anon, args.0, args.1, false, false)
                .unwrap();
        assert_eq!(only_write(&first).outcome, WriteOutcome::Created);
        let bytes = fs::read_to_string(&path).unwrap();

        let second =
            write_cloud_mcp_config_in(tmp.path(), None, &anon, args.0, args.1, false, false)
                .unwrap();
        assert_eq!(
            only_write(&second).outcome,
            WriteOutcome::AlreadyCurrent,
            "a reconnect that changes nothing must say so rather than claim an update",
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            bytes,
            "the file must not be rewritten when the entry already matches",
        );

        // --force escalates past the match, which is the flag's only remaining job.
        let forced =
            write_cloud_mcp_config_in(tmp.path(), None, &anon, args.0, args.1, false, true)
                .unwrap();
        assert_eq!(only_write(&forced).outcome, WriteOutcome::Updated);
    }

    /// The shape a bool could never see: an entry that IS a cloud entry but names
    /// a different backend or profile. The token connect just minted belongs to
    /// the new pair and to nothing else, so leaving the old one in place would
    /// point the agent at a workspace its credential cannot open.
    #[test]
    fn a_cloud_entry_naming_another_workspace_is_rewritten_not_declined() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://old.example.workers.dev",
            "previous-workspace",
            false,
            false,
        )
        .unwrap();

        let writes = write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://new.example.workers.dev",
            "current-workspace",
            false,
            false,
        )
        .unwrap();

        assert_eq!(only_write(&writes).outcome, WriteOutcome::Updated);
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let env = &written["mcpServers"]["think-and-ship"]["env"];
        assert_eq!(
            env["THINK_AND_SHIP_CLOUD_URL"],
            "https://new.example.workers.dev"
        );
        assert_eq!(
            env[crate::cloud::credential::PROFILE_ENV],
            "current-workspace"
        );
    }

    /// `init` keeps its decline, and that asymmetry is deliberate rather than
    /// left over: the entry `init` authors is the LOCAL one, so overwriting a
    /// cloud entry with it would disconnect a connected user silently. Connect
    /// upgrades; init does not downgrade.
    #[test]
    fn init_still_declines_an_existing_entry_without_force() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example.workers.dev",
            "acme-profile",
            false,
            false,
        )
        .unwrap();
        let connected = fs::read_to_string(&path).unwrap();

        write_mcp_config(
            &path,
            ".mcp.json",
            ServerContainer::McpServers,
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            connected,
            "init without --force must not strip a cloud connection back to local",
        );
    }

    #[test]
    fn write_cloud_mcp_config_merges_cloud_entry_and_preserves_others() {
        let tmp = TempDir::new().unwrap();
        // A config that exists but holds no think-and-ship entry: nothing to
        // update, so this exercises the AUTHORING branch at the fallback host.
        // `None` for the user-level config keeps the candidate set off the real
        // `$HOME` — resolution consults `~/.claude.json`, and a test must not.
        fs::write(
            tmp.path().join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"x"}}}"#,
        )
        .unwrap();

        write_cloud_mcp_config_in(
            tmp.path(),
            None,
            &CallerEnv::unknown(),
            "https://api.example.workers.dev",
            "acme-profile",
            false,
            false,
        )
        .unwrap();

        let written: Value =
            serde_json::from_str(&fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        // The cloud entry is present with write-through sync armed...
        assert_eq!(
            written["mcpServers"]["think-and-ship"]["env"]["THINK_AND_SHIP_SYNC_TARGET"],
            "cloud"
        );
        assert_eq!(
            written["mcpServers"]["think-and-ship"]["env"][crate::cloud::credential::PROFILE_ENV],
            "acme-profile"
        );
        // ...and the unrelated server is preserved.
        assert_eq!(written["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn write_mcp_config_creates_and_preserves() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        // Pre-existing config with an unrelated server.
        fs::write(&path, r#"{"mcpServers":{"other":{"command":"x"}}}"#).unwrap();

        write_mcp_config(
            &path,
            ".mcp.json",
            ServerContainer::McpServers,
            false,
            false,
        )
        .unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // Our entry is present with persistence on...
        assert_eq!(
            written["mcpServers"]["think-and-ship"]["env"]["THINK_AND_SHIP_PERSIST"],
            "true"
        );
        // ...and the unrelated server is preserved.
        assert_eq!(written["mcpServers"]["other"]["command"], "x");
    }

    #[test]
    fn write_mcp_config_dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        write_mcp_config(&path, ".mcp.json", ServerContainer::McpServers, true, false).unwrap();
        assert!(!path.exists(), "dry-run must not create the config file");
    }

    #[test]
    fn write_mcp_config_skips_existing_without_force() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".mcp.json");
        fs::write(
            &path,
            r#"{"mcpServers":{"think-and-ship":{"command":"OLD"}}}"#,
        )
        .unwrap();
        // Without force: left untouched.
        write_mcp_config(
            &path,
            ".mcp.json",
            ServerContainer::McpServers,
            false,
            false,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["think-and-ship"]["command"], "OLD");
        // With force: overwritten to the canonical entry.
        write_mcp_config(&path, ".mcp.json", ServerContainer::McpServers, false, true).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["mcpServers"]["think-and-ship"]["command"],
            "think-and-ship"
        );
    }

    #[test]
    /// The generated CLAUDE.md is the tool reference a project keeps in its repo,
    /// so a family missing from it is a family the agent never learns about —
    /// `signal_*` was absent for its whole life. Bind the doc to the family list
    /// rather than trusting prose: every family the server serves must appear.
    fn claude_md_covers_every_served_family_and_marks_itself() {
        let md = generate_claude_md(None);
        assert!(md.starts_with(CLAUDE_MD_MARKER));
        for family in crate::mcp::UnifiedFamily::ALL {
            let prefix = format!("{}_*", family.prefix());
            assert!(
                md.contains(&prefix),
                "generated CLAUDE.md never mentions the {prefix} family"
            );
        }
    }

    // ---- adopting a machine that connected before the record existed ----

    /// A project whose only think-and-ship entry is a legacy cloud-configured
    /// one, in the host config — the shape of every machine that connected
    /// before the record, and the shape of the machine this was written for.
    fn legacy_home_config(tmp: &Path, url: &str, profile: &str) -> Option<PathBuf> {
        let home = tmp.join("home.json");
        let key = tmp
            .canonicalize()
            .unwrap_or_else(|_| tmp.to_path_buf())
            .display()
            .to_string();
        let mut doc = json!({ "projects": { key.clone(): { "mcpServers": {} } } });
        doc["projects"][&key]["mcpServers"][MCP_SERVER_NAME] = cloud_server_config(url, profile);
        fs::write(&home, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        Some(home)
    }

    /// A store holding a token for `profile`, the way a machine that really
    /// connected does.
    fn store_holding(
        dir: &Path,
        profile: &str,
    ) -> std::sync::Arc<crate::tracker::credential::FileCredentialStore> {
        let store = std::sync::Arc::new(crate::tracker::credential::FileCredentialStore::new(dir));
        let resolver = crate::tracker::credential::Resolver::new(store.clone());
        crate::cloud::credential::adopt(&resolver, profile, "tok-legacy", "2026-07-30T00:00:00Z")
            .unwrap();
        store
    }

    /// ACCEPTANCE, and the ABSENCE this chunk exists to prove: with a legacy MCP
    /// config on disk and a token in the store, the RESOLVER still answers
    /// nothing — the config is not a source — and only after the migration
    /// writes a record does the same composition answer.
    ///
    /// Both halves have to be in one test. Proving only that adoption works
    /// would leave the actual hazard untested: a resolve path quietly learning
    /// to read a config file is precisely the coupling that made the MCP config
    /// the connection database, and made writing it to the wrong client destroy
    /// the connection rather than misroute it. So the first assertion is that
    /// the config alone is worth NOTHING to anyone resolving.
    #[test]
    fn a_config_alone_resolves_to_nothing_and_only_the_migration_changes_that() {
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let project = "proj-legacy";
        let home = legacy_home_config(tmp.path(), "https://api.example", "acme");
        let store = store_holding(&data.path().join("creds"), "acme");
        let empty = crate::cloud::config::EnvOverrides::default();

        // THE ABSENCE. Everything a legacy machine has is on disk, and every
        // resolver still says no, because none of them may read a config.
        assert_eq!(
            crate::cloud::connection::load_in(data.path(), project),
            None,
            "no record yet — that IS the reported state",
        );
        assert!(
            crate::cloud::config::client_with(store.as_ref(), &empty, None).is_none(),
            "a config on disk plus a token in the store must not build a client: \
             resolution reads the record, never a config file",
        );

        // THE MIGRATION, once.
        let adopted = adopt_legacy_connection_in(
            data.path(),
            project,
            tmp.path(),
            home.clone(),
            store.as_ref(),
            "2026-08-01T00:00:00Z",
        )
        .expect("a legacy config plus a matching token is an adoptable connection");
        assert_eq!(adopted.connection.cloud_url, "https://api.example");
        assert_eq!(adopted.connection.profile, "acme");
        assert!(
            adopted.from.contains("home.json"),
            "adoption must say where it read from: {}",
            adopted.from,
        );

        // THE PRESENCE. The record is what the resolver now answers from.
        let stored = crate::cloud::connection::load_in(data.path(), project)
            .expect("the migration left a record behind");
        let client = crate::cloud::config::client_with(store.as_ref(), &empty, Some(&stored))
            .expect("the adopted record is a usable connection");
        assert_eq!(client.base_url(), "https://api.example");

        // ONCE. A second cloud verb finds a record and does nothing.
        assert_eq!(
            adopt_legacy_connection_in(
                data.path(),
                project,
                tmp.path(),
                home,
                store.as_ref(),
                "2026-08-02T00:00:00Z",
            ),
            None,
            "adoption is a migration, not a fallback — it must not run twice",
        );
        assert_eq!(
            crate::cloud::connection::load_in(data.path(), project)
                .unwrap()
                .connected_at,
            "2026-08-01T00:00:00Z",
            "and it must not overwrite the record it already wrote",
        );
    }

    /// A committed config is not proof this machine connected.
    ///
    /// `.mcp.json` and `.cursor/mcp.json` live in the repository, so everyone who
    /// clones a connected project gets a file naming a cloud url and a profile.
    /// The token is the only half that cannot be committed, so it is the half
    /// that decides. Without it, adoption must decline — inventing a connection
    /// to a workspace the user holds no credential for would replace an honest
    /// "not connected" with a false "connected", in the surface this whole chunk
    /// exists to make honest.
    #[test]
    fn a_cloned_repository_is_not_a_connected_machine() {
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        fs::write(
            tmp.path().join(".mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": { MCP_SERVER_NAME: cloud_server_config("https://api.example", "acme") }
            }))
            .unwrap(),
        )
        .unwrap();
        // A real store — it simply has never held this profile.
        let empty_store =
            crate::tracker::credential::FileCredentialStore::new(&data.path().join("creds"));

        assert_eq!(
            adopt_legacy_connection_in(
                data.path(),
                "proj-cloned",
                tmp.path(),
                None,
                &empty_store,
                "2026-08-01T00:00:00Z",
            ),
            None,
            "a committed config with no token behind it is somebody else's connection",
        );
        assert_eq!(
            crate::cloud::connection::load_in(data.path(), "proj-cloned"),
            None,
            "and nothing was written",
        );
    }

    /// The search covers EVERY client's config, not a guessed one.
    ///
    /// This is not a hypothetical: the machines that need adopting are exactly
    /// the ones the old host guess misconfigured, so their settings are most
    /// likely sitting in a client nobody would pick. Here the project looks like
    /// a Cursor project — `.cursor/` is present and holds an entry — while the
    /// cloud wiring is in Windsurf's config. A guess loses it; a search does not.
    #[test]
    fn adoption_searches_every_client_rather_than_the_one_a_guess_would_pick() {
        let tmp = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".cursor")).unwrap();
        fs::write(
            tmp.path().join(".cursor/mcp.json"),
            serde_json::to_string_pretty(&json!({
                "mcpServers": { MCP_SERVER_NAME: mcp_server_config() }
            }))
            .unwrap(),
        )
        .unwrap();
        let windsurf = HOST_TARGETS
            .iter()
            .find(|t| t.name == "Windsurf")
            .expect("Windsurf is a known client");
        fs::create_dir_all(tmp.path().join(windsurf.dir)).unwrap();
        let mut doc = json!({});
        doc[windsurf.container.key()] =
            json!({ MCP_SERVER_NAME: cloud_server_config("https://api.example", "acme") });
        fs::write(
            tmp.path().join(windsurf.config_file),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
        let store = store_holding(&data.path().join("creds"), "acme");

        let adopted = adopt_legacy_connection_in(
            data.path(),
            "proj-wrong-client",
            tmp.path(),
            None,
            store.as_ref(),
            "2026-08-01T00:00:00Z",
        )
        .expect("the wiring is on disk in SOME client's config, so it is adoptable");
        assert_eq!(adopted.connection.cloud_url, "https://api.example");
        assert!(
            adopted.from.contains("Windsurf"),
            "the entry that actually carries the wiring is the one adopted: {}",
            adopted.from,
        );
    }

    /// Every CLI verb that consults the connection adopts first — and the two
    /// that must NOT are named here with the reason, so removing a call or
    /// adding a consumer without one is a failing test rather than a silent
    /// regression on the machines this chunk is for.
    ///
    /// A behaviour test cannot reach these: they resolve the data dir and the
    /// cwd ambiently, which is the whole reason the rule beneath them takes
    /// every input as a parameter. So this reads the wiring instead. The needles
    /// carry their leading indentation, so a call left dead inside a comment or
    /// a doc line cannot satisfy them.
    #[test]
    fn every_cloud_verb_adopts_before_it_answers() {
        let cli = include_str!("mod.rs");
        for (verb, needle) in [
            // `status` is the verb that was lying: "Cloud: not connected" on a
            // machine whose agent syncs fine.
            (
                "status",
                "    adopt_legacy_connection();\n    setup::status()",
            ),
            // `sync push` is the verb that FAILED, and it adopts after the
            // dry-run exit so the flag keeps its promise to write nothing.
            (
                "sync push",
                "    adopt_legacy_connection();\n\n    let Some(client) = \
                 crate::cloud::config::client_from_env()",
            ),
        ] {
            assert!(
                cli.contains(needle),
                "`{verb}` no longer adopts before it reads the connection",
            );
        }

        // The spawned server must NOT adopt. It is handed the `env` block by the
        // MCP host, so it was never the broken half — and `serve` is the one
        // process that has to stay a pure resolver, or the config becomes a
        // source again for the consumer that reads it most.
        let serve = cli
            .split("fn build_unified()")
            .nth(1)
            .expect("build_unified is where the spawned server resolves its client");
        assert!(
            !serve[..serve.find("\npub fn ").unwrap_or(serve.len())]
                .contains("adopt_legacy_connection("),
            "the spawned MCP server must never adopt — it resolves, and a resolver \
             that reads an MCP config is the coupling this chunk removed",
        );

        // `disconnect` must NOT adopt either, and the reason is that it already
        // handles this machine: it forgets the credential AND strips the cloud
        // keys from the config, so a legacy machine is fully cleaned without a
        // record ever existing. Adopting first would write a record purely to
        // delete it, and print "adopted your connection" one line above
        // "disconnected".
        let disconnect = include_str!("connect.rs");
        assert!(
            !disconnect.contains("    adopt_legacy_connection("),
            "disconnect cleans both halves already — adopting first is churn",
        );
    }

    #[test]
    fn write_claude_md_appends_then_guards_against_duplicate() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("CLAUDE.md"),
            "# My project\n\nExisting notes.\n",
        )
        .unwrap();
        write_claude_md(tmp.path(), None, false, false).unwrap();
        let after = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(after.contains("Existing notes."));
        assert!(after.contains(CLAUDE_MD_MARKER));
        // Second run without force must not duplicate the section.
        write_claude_md(tmp.path(), None, false, false).unwrap();
        let after2 = fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert_eq!(after2.matches(CLAUDE_MD_MARKER).count(), 1);
    }

    /// The rules a user wrote BELOW the generated section must survive a
    /// force-replace. They did not: "replace" kept only the text above the
    /// start marker, so re-running `init --with-claude-md --force` deleted the
    /// tail of the file — and CLAUDE.md is precisely the file people append
    /// their own rules to.
    #[test]
    fn force_replace_keeps_what_the_user_wrote_on_both_sides() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("CLAUDE.md");
        fs::write(&path, "# House rules\n\nAbove the section.\n").unwrap();
        write_claude_md(tmp.path(), None, false, false).unwrap();

        // The user then appends their own section underneath ours.
        let mut with_tail = fs::read_to_string(&path).unwrap();
        with_tail.push_str("\n# My own rules\n\nNever force-push.\n");
        fs::write(&path, &with_tail).unwrap();

        write_claude_md(tmp.path(), None, false, true).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("Above the section."), "lost the head");
        assert!(after.contains("Never force-push."), "lost the tail");
        assert_eq!(after.matches(CLAUDE_MD_MARKER).count(), 1);
        assert_eq!(after.matches(CLAUDE_MD_END_MARKER).count(), 1);
    }

    /// A section written before the end marker existed still has to be replaced
    /// without eating the file. The only bound available is the next top-level
    /// heading, and this pins that it is actually used.
    #[test]
    fn a_legacy_unterminated_section_is_bounded_by_the_next_heading() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("CLAUDE.md");
        fs::write(
            &path,
            format!(
                "{CLAUDE_MD_MARKER}\n# think-and-ship\n\nStale v0.1 text.\n\n\
                 # My own rules\n\nNever force-push.\n"
            ),
        )
        .unwrap();

        write_claude_md(tmp.path(), None, false, true).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("Stale v0.1 text."), "stale text survived");
        assert!(after.contains("Never force-push."), "lost the tail");
        assert!(after.contains(CLAUDE_MD_END_MARKER), "not terminated");
    }

    /// A preview whose verb does not match the branch that would run is worse
    /// than no preview: `--dry-run --force` over an existing section announced
    /// "append", which is the one thing it would not do.
    #[test]
    fn the_dry_run_verb_names_the_branch_that_would_actually_run() {
        assert_eq!(dry_run_verb(&None, false), "create");
        assert_eq!(dry_run_verb(&Some(String::new()), false), "append to");
        assert_eq!(
            dry_run_verb(&Some(String::new()), true),
            "replace the think-and-ship section in"
        );
    }

    /// This repository's own CLAUDE.md is the tool reference every agent working
    /// here reads first, and it drifted a whole major behind the generator: it
    /// still described two servers and named tools
    /// removed in v0.3.0. Nothing detected that, because nothing compared the
    /// artifact to the source it is generated from. This does.
    ///
    /// The file is gitignored — a working-copy artifact, not repository content
    /// — so ABSENCE is the normal state on a fresh clone and in CI, and is not a
    /// finding. Only a CLAUDE.md that exists is held to being current; a gate
    /// that panicked on the missing file would fail every checkout instead.
    #[test]
    fn a_generated_claude_md_in_this_repository_is_kept_current() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/think-and-ship has a workspace root")
            .to_path_buf();
        let path = root.join("CLAUDE.md");
        let Ok(actual) = fs::read_to_string(&path) else {
            return;
        };
        // Someone else's CLAUDE.md, with no generated section in it, is also
        // nothing to check — this gate covers drift, not adoption.
        if !actual.contains(CLAUDE_MD_MARKER) {
            return;
        }
        let expected = generate_claude_md(detect_project(&root));
        assert!(
            actual.contains(&expected),
            "{} has drifted from generate_claude_md — regenerate it with\n  \
             think-and-ship init --with-claude-md --force",
            path.display(),
        );
    }
}
