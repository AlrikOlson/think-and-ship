//! `think-and-ship skills install` / `skills list` — install the bundled agent
//! skills into a coding agent's skills directory, so the workflow is available
//! in every project rather than copied per-repo by hand.
//!
//! # One canonical body, many harnesses
//!
//! A skill's PROCEDURE is written once, in `crates/think-and-ship/skills/`.
//! Nothing here ever copies or forks it. What genuinely differs per harness is
//! handled three ways, in increasing order of intrusiveness:
//!
//! * **Frontmatter overlay** — extra YAML keys a harness documents and others
//!   do not, e.g. Claude Code's `argument-hint`. See [`Harness::metadata`].
//! * **Auxiliary files** — a sidecar manifest a harness reads, e.g. Codex's
//!   `agents/openai.yaml`. These live in `skill-clients/<key>/<skill>/`.
//! * **Token substitution** — `{{SKILLS_DIR}}` and friends, for the LEGACY
//!   skills that still address a specific client in prose. The two core skills
//!   deliberately use none, which is why their bodies are byte-identical
//!   everywhere.
//!
//! # The shared-directory rule
//!
//! `.agents/skills` is not one harness's directory. It is native to Codex, Amp
//! and Goose, and also read by Copilot, Cursor, Gemini, Windsurf, Roo Code and
//! OpenCode. So **anything written to a `.agents` destination carries only the
//! six Agent Skills spec fields** — a Claude-flavoured render dropped there
//! would be handed to seven agents that never agreed to those keys.
//!
//! [`Harness::metadata`] is therefore required to be empty whenever the
//! destination is shared, and `shared_destination_renders_are_portable` fails
//! the build if that is ever violated.
//!
//! # Where the paths come from
//!
//! Every destination below is quoted from a first-party page in `docs/HARNESSES.md`,
//! with the date it was read. **Do not add or edit a destination here without
//! updating that file** — the previous generation of this module asserted
//! `~/.codex/skills` from a comment, and that path had ceased to exist.
//!
//! Paths are built with `Path::join`, never string concatenation, so the same
//! code writes correct paths on every platform.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

include!(concat!(env!("OUT_DIR"), "/skill_files.rs"));

/// How well a harness is actually supported, as decided by its own docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportTier {
    /// First-party documentation describes Agent Skills discovery.
    Native,
    /// Agent Skills work, but the harness's own guidance makes a DIFFERENT
    /// primitive the right home for an explicitly-triggered procedure — so a
    /// thin wrapper in that primitive is shipped alongside. Labelled as an
    /// adaptation, never as plain compatibility.
    NativeWithWrapper,
}

impl SupportTier {
    const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::NativeWithWrapper => "native+equiv",
        }
    }
}

/// A thin manual-invocation wrapper in a harness's own non-skill primitive.
///
/// Exists for exactly one documented situation: in Windsurf a Skill is reached
/// as `@skill-name` and only a Workflow gets a `/slash-command`. So a wrapper is
/// what gives a Windsurf user the same `/switch-work` every other harness has —
/// it ADDS an entry point rather than closing one, which is why it survives the
/// removal of the manual-only keys (see `ARGUMENT_HINT_ONLY`). The skill itself
/// stays model-invocable there, as everywhere.
///
/// The PROCEDURE still lives in the skill. The wrapper is a pointer, never a
/// second copy.
pub struct Wrapper {
    /// Destination directory components, user scope, under the home dir.
    pub user_dir: &'static [&'static str],
    /// Destination directory components, project scope, under the repo root.
    pub project_dir: &'static [&'static str],
    /// Hard size limit the harness documents, in characters.
    pub max_chars: usize,
    /// What this primitive is called, for messages.
    pub kind: &'static str,
}

/// User-level (every project) or project-level (this repository).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" | "global" => Ok(Self::User),
            "project" | "repo" | "workspace" => Ok(Self::Project),
            other => anyhow::bail!("unknown --scope {other:?} (expected: user | project)"),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// Which set of skills to install.
///
/// The default is deliberately two skills. A catalog is a budget: every skill
/// a harness discovers costs metadata in the model's context at startup, and
/// eleven overlapping workflow skills spent that budget on choosing between
/// them rather than on doing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `switch-work` + `advance-work`. The default.
    Core,
    /// The pre-Iteration-3 workflow skills, kept installable for anyone who
    /// depends on them. Never installed unless asked for by name.
    Legacy,
    /// Both.
    All,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "core" => Ok(Self::Core),
            "legacy" | "extended" => Ok(Self::Legacy),
            "all" => Ok(Self::All),
            other => anyhow::bail!("unknown --profile {other:?} (expected: core | legacy | all)"),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Legacy => "legacy",
            Self::All => "all",
        }
    }

    /// Whether a skill in `skill_profile` is installed under this profile.
    const fn includes(self, skill_profile: Self) -> bool {
        matches!(
            (self, skill_profile),
            (Self::All, _) | (Self::Core, Self::Core) | (Self::Legacy, Self::Legacy)
        )
    }
}

/// A frontmatter key/value a specific harness documents.
pub struct MetaField {
    pub key: &'static str,
    /// The literal YAML value. `{{ARGUMENT_HINT}}` is replaced per skill, and
    /// the whole field is dropped when that skill has no hint.
    pub value: &'static str,
}

/// One coding agent, and everything the installer must know to write for it.
pub struct Harness {
    /// `--client` value.
    pub key: &'static str,
    /// Other accepted spellings.
    pub aliases: &'static [&'static str],
    pub name: &'static str,
    pub tier: SupportTier,
    /// Directory under the home dir whose existence means this agent is
    /// installed. `None` where no first-party page documents a stable config
    /// directory — such a harness is never auto-detected and must be named
    /// explicitly, which is the conservative answer rather than a guess.
    pub marker: Option<&'static [&'static str]>,
    /// User-scope skills directory, as components under the home dir.
    pub user_dir: &'static [&'static str],
    /// Project-scope skills directory, as components under the project root.
    pub project_dir: &'static [&'static str],
    /// How a person invokes a skill here.
    pub invocation: &'static str,
    /// Extra frontmatter this harness documents. MUST be empty when either
    /// destination is a shared `.agents` directory.
    pub metadata: &'static [MetaField],
    /// What to tell the user so the skills are picked up.
    pub reload: &'static str,
    /// How this harness prefers reusable distribution, for `skills list`.
    pub distribution: &'static str,
    /// A manual-invocation wrapper in this harness's own non-skill primitive,
    /// where its documentation makes that the right home.
    pub wrapper: Option<Wrapper>,
    /// Client-specific phrases substituted into LEGACY skill bodies.
    pub tokens: Tokens,
}

/// The client-specific phrases substituted into legacy bundled markdown.
///
/// Only the legacy profile needs these. `switch-work` and `advance-work` carry
/// no tokens at all, which is what makes their rendered bodies identical
/// across all twelve harnesses and the one-canonical-source claim checkable.
pub struct Tokens {
    pub skills_dir: &'static str,
    pub tool_search: &'static str,
    pub ask_user_tool: &'static str,
    pub coauthor: &'static str,
}

/// Tokens for a harness with no special deferred-tool or question primitive.
const GENERIC_TOKENS: Tokens = Tokens {
    skills_dir: "your agent's skills directory",
    tool_search: "**one** tool-search call (your client's deferred-tool discovery)",
    ask_user_tool: "your client's structured user-question tool",
    coauthor: "",
};

/// A hint at the arguments a skill takes, for the harnesses that document one.
///
/// # Why there is no manual-only key beside it
///
/// This overlay used to carry `disable-model-invocation: true` as well, and
/// three other harnesses carried the same suppression in their own dialect.
/// That was wrong, and it failed in the field: an agent asked in prose to
/// "advance the current work" could not, because the only route left was the
/// user typing the literal slash command.
///
/// It contradicted the skills themselves. Both descriptions enumerate
/// natural-language triggers — *"advance the current work"*, *"do the next
/// unit"*, *"switch to \<workstream\>"*, *"focus billing"* — and a harness that
/// suppresses model invocation turns every one of them into a dead letter while
/// still advertising them. A skill that cannot be reached the way its own
/// description says it is reached is a broken artifact, not a careful one.
///
/// What bounds these two is not the invocation route. It is their doctrine —
/// one unit per invocation, the project's real gates, an honest receipt when
/// there is no work — and the explicit *Not this skill* table each one carries.
/// Those hold however the skill was entered.
const ARGUMENT_HINT_ONLY: &[MetaField] = &[MetaField {
    // QUOTED, and it must stay quoted. An argument hint reads naturally as
    // `[workstream] [mode]`, and unquoted that is a YAML flow sequence
    // followed by a second one — which does not parse at all. The official
    // validator caught this on the rendered variant; nothing in the
    // canonical source could have.
    key: "argument-hint",
    value: "\"{{ARGUMENT_HINT}}\"",
}];

/// Every harness the installer knows how to write for.
///
/// Sourced from `docs/HARNESSES.md`, verified 2026-08-08. Ordered by the
/// minimum-support list, then the extras.
pub const HARNESSES: &[Harness] = &[
    Harness {
        key: "claude-code",
        aliases: &["claude", "claudecode"],
        name: "Claude Code",
        tier: SupportTier::Native,
        marker: Some(&[".claude"]),
        user_dir: &[".claude", "skills"],
        project_dir: &[".claude", "skills"],
        invocation: "/<skill>",
        metadata: ARGUMENT_HINT_ONLY,
        reload: "picked up live; restart only if the skills directory is new",
        distribution: "plugin",
        wrapper: None,
        tokens: Tokens {
            skills_dir: "~/.claude/skills",
            tool_search: "**one** ToolSearch call",
            ask_user_tool: "AskUserQuestion",
            coauthor: "Claude <noreply@anthropic.com>",
        },
    },
    Harness {
        key: "codex",
        aliases: &["openai"],
        name: "Codex",
        tier: SupportTier::Native,
        // Codex keeps its config in ~/.codex but reads skills from
        // ~/.agents/skills. Marker and destination differ on purpose.
        marker: Some(&[".codex"]),
        user_dir: &[".agents", "skills"],
        project_dir: &[".agents", "skills"],
        invocation: "$<skill>",
        // SHARED destination: portable render only.
        metadata: &[],
        reload: "restart Codex",
        distribution: "plugin",
        wrapper: None,
        tokens: Tokens {
            skills_dir: "~/.agents/skills",
            tool_search: "**one** tool-search call (your client's deferred-tool discovery)",
            ask_user_tool: "request_user_input",
            coauthor: "Codex <noreply@openai.com>",
        },
    },
    Harness {
        key: "copilot",
        aliases: &["github", "github-copilot"],
        name: "GitHub Copilot",
        tier: SupportTier::Native,
        marker: Some(&[".copilot"]),
        user_dir: &[".copilot", "skills"],
        project_dir: &[".github", "skills"],
        invocation: "/<skill> (named in the prompt)",
        metadata: &[],
        reload: "/skills reload",
        distribution: "copilot skill add",
        wrapper: None,
        tokens: Tokens {
            skills_dir: "~/.copilot/skills",
            tool_search: "**one** tool-search call (your client's deferred-tool discovery)",
            ask_user_tool: "your client's structured user-question tool",
            coauthor: "GitHub Copilot <noreply@github.com>",
        },
    },
    Harness {
        key: "cursor",
        aliases: &[],
        name: "Cursor",
        tier: SupportTier::Native,
        marker: Some(&[".cursor"]),
        user_dir: &[".cursor", "skills"],
        project_dir: &[".cursor", "skills"],
        invocation: "/<skill>",
        metadata: &[],
        reload: "discovered at Cursor start",
        distribution: "CLI installer",
        wrapper: None,
        tokens: Tokens {
            skills_dir: "~/.cursor/skills",
            tool_search: "**one** tool-search call (your client's deferred-tool discovery)",
            ask_user_tool: "your client's structured user-question tool",
            coauthor: "Cursor <noreply@cursor.com>",
        },
    },
    Harness {
        key: "gemini",
        aliases: &["gemini-cli"],
        name: "Gemini CLI",
        tier: SupportTier::Native,
        marker: Some(&[".gemini"]),
        user_dir: &[".gemini", "skills"],
        project_dir: &[".gemini", "skills"],
        invocation: "consent-gated on activation; /skills list",
        // No frontmatter beyond the spec is documented for Gemini CLI, so none
        // is emitted. Adding an unsupported key to emulate another harness is
        // exactly what the matrix forbids.
        metadata: &[],
        reload: "/skills reload",
        distribution: "gemini skills install / extension",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "windsurf",
        aliases: &["cascade", "codeium"],
        name: "Windsurf Cascade",
        // Its own docs say Skills have no manual-only control and Workflows
        // do, so the skill ships natively AND a manual-only workflow points
        // at it. Neither copies the other's procedure.
        tier: SupportTier::NativeWithWrapper,
        marker: Some(&[".codeium"]),
        user_dir: &[".codeium", "windsurf", "skills"],
        project_dir: &[".windsurf", "skills"],
        invocation: "@<skill>, or /<skill> via the workflow",
        metadata: &[],
        reload: "restart Cascade",
        distribution: "workflow wrapper",
        wrapper: Some(Wrapper {
            user_dir: &[".codeium", "windsurf", "global_workflows"],
            project_dir: &[".windsurf", "workflows"],
            max_chars: 12_000,
            kind: "workflow",
        }),
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "opencode",
        aliases: &[],
        name: "OpenCode",
        tier: SupportTier::Native,
        marker: Some(&[".config", "opencode"]),
        user_dir: &[".config", "opencode", "skills"],
        project_dir: &[".opencode", "skills"],
        invocation: "/<skill>",
        metadata: &[],
        reload: "restart OpenCode",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "cline",
        aliases: &[],
        name: "Cline",
        tier: SupportTier::Native,
        marker: Some(&[".cline"]),
        user_dir: &[".cline", "skills"],
        project_dir: &[".cline", "skills"],
        invocation: "/<skill>",
        // Cline documents NO manual-only control. The mitigation is in the
        // skill body's first-step guard, and the limitation is recorded in
        // docs/HARNESSES.md rather than papered over with an invented key.
        metadata: &[],
        reload: "restart Cline",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "roo",
        aliases: &["roo-code", "roocode"],
        name: "Roo Code",
        tier: SupportTier::Native,
        marker: Some(&[".roo"]),
        // Generic, NOT skills-code/: these skills span shape, build and listen,
        // so scoping them to Code mode would hide them from the modes that
        // need them most.
        user_dir: &[".roo", "skills"],
        project_dir: &[".roo", "skills"],
        invocation: "matched from the description",
        metadata: &[],
        reload: "file watchers pick up changes",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "amp",
        aliases: &["ampcode"],
        name: "Amp",
        tier: SupportTier::Native,
        // No first-party page in the 2026-08-08 sweep documents a stable Amp
        // config directory, so it is never auto-detected.
        marker: None,
        user_dir: &[".config", "agents", "skills"],
        project_dir: &[".agents", "skills"],
        invocation: "command palette -> skill: invoke",
        metadata: &[],
        reload: "amp skills list",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "goose",
        aliases: &[],
        name: "Goose",
        tier: SupportTier::Native,
        marker: None,
        user_dir: &[".config", "agents", "skills"],
        project_dir: &[".agents", "skills"],
        invocation: "matched from the description",
        metadata: &[],
        reload: "restart Goose",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
    Harness {
        key: "kiro",
        aliases: &[],
        name: "Kiro",
        tier: SupportTier::Native,
        marker: Some(&[".kiro"]),
        user_dir: &[".kiro", "skills"],
        project_dir: &[".kiro", "skills"],
        invocation: "/<skill>",
        metadata: &[],
        reload: "restart Kiro",
        distribution: "CLI installer",
        wrapper: None,
        tokens: GENERIC_TOKENS,
    },
];

impl Harness {
    /// This harness's skills root for `scope`.
    fn root(&self, scope: Scope, home: &Path, project: &Path) -> PathBuf {
        let (base, parts) = match scope {
            Scope::User => (home, self.user_dir),
            Scope::Project => (project, self.project_dir),
        };
        parts.iter().fold(base.to_path_buf(), |p, c| p.join(c))
    }

    /// `~`-style display form of the destination, for messages.
    fn display_dir(&self, scope: Scope) -> String {
        match scope {
            Scope::User => format!("~/{}", self.user_dir.join("/")),
            Scope::Project => format!("./{}", self.project_dir.join("/")),
        }
    }

    /// Whether either destination is a directory other harnesses also read.
    ///
    /// Consulted by the renderer, not just by documentation: a shared
    /// destination may only ever receive the portable render.
    #[must_use]
    pub fn writes_to_shared_directory(&self) -> bool {
        self.user_dir.first() == Some(&".agents")
            || self.project_dir.first() == Some(&".agents")
            || self.user_dir.contains(&"agents")
    }

    fn matches(&self, key: &str) -> bool {
        self.key.eq_ignore_ascii_case(key)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(key))
    }

    /// Substitute the legacy client tokens into one bundled file.
    ///
    /// A harness writing a SHARED destination gets the GENERIC phrasing, not
    /// its own. This is the shared-directory rule applied to prose rather than
    /// to frontmatter, and it is not a nicety: Codex, Amp and Goose all write
    /// `.agents/skills`, so harness-specific wording there means whichever
    /// installs last silently redefines the other's skill. Nobody would see it
    /// except a user with two of them installed.
    fn substitute(&self, contents: &str) -> String {
        let t = if self.writes_to_shared_directory() {
            &GENERIC_TOKENS
        } else {
            &self.tokens
        };
        contents
            .replace("{{SKILLS_DIR}}", t.skills_dir)
            .replace("{{TOOL_SEARCH}}", t.tool_search)
            .replace("{{ASK_USER_TOOL}}", t.ask_user_tool)
            .replace("{{COAUTHOR}}", t.coauthor)
    }
}

/// Facts about a bundled skill that the canonical body does not carry.
struct SkillFacts {
    profile: Profile,
    /// Claude Code's autocomplete hint, where the skill takes arguments.
    argument_hint: Option<&'static str>,
}

/// The two core skills, and what the renderer needs to know about each.
///
/// A skill absent from this table is legacy. That default is deliberate: a new
/// skill joins the lean surface only by an explicit edit here, so the core
/// profile cannot grow by accident.
fn facts_for(skill: &str) -> SkillFacts {
    match skill {
        "switch-work" => SkillFacts {
            profile: Profile::Core,
            argument_hint: Some("[workstream] [shape|build|listen]"),
        },
        "advance-work" => SkillFacts {
            profile: Profile::Core,
            argument_hint: None,
        },
        _ => SkillFacts {
            profile: Profile::Legacy,
            argument_hint: None,
        },
    }
}

/// One bundled skill: its directory name and every file it ships, keyed by the
/// path relative to the skill's own directory. Paths always use `/`.
pub struct BundledSkill {
    pub name: &'static str,
    pub files: Vec<(&'static str, &'static str)>,
}

/// A skill resolved for one harness: overlay applied, frontmatter extended,
/// tokens substituted. Exactly what lands on disk.
struct RenderedSkill {
    files: Vec<(String, String)>,
}

impl BundledSkill {
    /// First `description:` value of the SKILL.md front matter, flattened to a
    /// single line and truncated for listing.
    fn summary(&self) -> String {
        let Some((_, body)) = self.files.iter().find(|(p, _)| *p == "SKILL.md") else {
            return String::new();
        };
        let mut lines = body
            .lines()
            .skip_while(|l| !l.trim_start().starts_with("description:"));
        let first = lines.next().unwrap_or("");
        let inline = first.trim().trim_start_matches("description:").trim();
        let mut text = if inline.is_empty() || inline == ">-" || inline == ">" || inline == "|" {
            lines
                .take_while(|l| l.starts_with("  ") || l.trim().is_empty())
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            inline.to_string()
        };
        text = text.trim().to_string();
        let mut out: String = text.chars().take(72).collect();
        if text.chars().count() > 72 {
            out.push('…');
        }
        out
    }

    fn profile(&self) -> Profile {
        facts_for(self.name).profile
    }
}

/// Group the flat build-time file table into skills, preserving path order.
pub fn bundled_skills() -> Vec<BundledSkill> {
    let mut grouped: BTreeMap<&'static str, Vec<(&'static str, &'static str)>> = BTreeMap::new();
    for (path, contents) in SKILL_FILES {
        let Some((skill, rest)) = path.split_once('/') else {
            continue;
        };
        grouped.entry(skill).or_default().push((rest, contents));
    }
    grouped
        .into_iter()
        .map(|(name, files)| BundledSkill { name, files })
        .collect()
}

/// Overlay files for `harness` + `skill`, as (path within the skill, contents).
fn overlay_files(harness: &str, skill: &str) -> Vec<(&'static str, &'static str)> {
    SKILL_CLIENT_FILES
        .iter()
        .filter_map(|(path, contents)| {
            let (c, rest) = path.split_once('/')?;
            let (s, within) = rest.split_once('/')?;
            (c == harness && s == skill).then_some((within, *contents))
        })
        .collect()
}

/// Add a harness's documented frontmatter keys to a canonical `SKILL.md`.
///
/// Appends before the closing `---` rather than rewriting the block, so the
/// canonical `name` and `description` survive byte-identical — including the
/// block-scalar formatting, which a naive YAML round-trip would reflow.
///
/// A field whose value still contains an unfilled `{{ARGUMENT_HINT}}` is
/// DROPPED, because a hint is per-skill and most skills take no arguments.
fn extend_frontmatter(body: &str, fields: &[MetaField], hint: Option<&str>) -> String {
    if fields.is_empty() {
        return body.to_string();
    }
    let Some(rest) = body.strip_prefix("---\n") else {
        // No frontmatter to extend — leave the file exactly as it is rather
        // than inventing a block. Validation catches a skill with none.
        return body.to_string();
    };
    let Some(end) = rest.find("\n---\n") else {
        return body.to_string();
    };
    let mut extra = String::new();
    for f in fields {
        let value = match (f.value.contains("{{ARGUMENT_HINT}}"), hint) {
            (true, Some(h)) => f.value.replace("{{ARGUMENT_HINT}}", h),
            (true, None) => continue,
            (false, _) => f.value.to_string(),
        };
        extra.push_str(&format!("\n{}: {value}", f.key));
    }
    format!("---\n{}{extra}{}", &rest[..end], &rest[end..])
}

/// The body of a manual-invocation wrapper for `skill`.
///
/// A POINTER, not a copy. It names the skill and tells the agent to follow it;
/// duplicating the procedure here would create a second source that drifts,
/// which is the one thing this whole module exists to prevent.
fn wrapper_body(skill: &BundledSkill, harness: &Harness, kind: &str) -> String {
    format!(
        "---\ndescription: {}\n---\n\n\
         # {name}\n\n\
         Manual-only {kind}. Cascade never triggers this by itself.\n\n\
         **Follow the `{name}` skill's instructions exactly.** Its `SKILL.md` is the \
         single source of this procedure; this file only makes it manually invocable, \
         because a {kind} is the manual-only primitive here and a skill is not.\n\n\
         Read `{name}/SKILL.md` from the skills directory and do what it says. Do not \
         improvise a shorter version of it from this file.\n",
        skill.summary(),
        name = skill.name,
        kind = kind,
    )
    .chars()
    // The documented hard limit is the harness's, not ours; truncating is
    // better than writing a file it will reject outright, and the pointer's
    // first sentence is the load-bearing part.
    .take(harness.wrapper.as_ref().map_or(usize::MAX, |w| w.max_chars))
    .collect()
}

/// Resolve a skill for one harness. Overlay wins over base on path collision;
/// then frontmatter is extended and tokens are substituted.
fn render_skill(skill: &BundledSkill, harness: &Harness) -> RenderedSkill {
    let overlay = overlay_files(harness.key, skill.name);
    let hint = facts_for(skill.name).argument_hint;
    // The shared-directory rule, enforced in the product rather than only in a
    // test: a destination other harnesses read gets the portable render, full
    // stop — even if a future edit adds metadata to such a harness by mistake.
    let fields: &[MetaField] = if harness.writes_to_shared_directory() {
        &[]
    } else {
        harness.metadata
    };
    let mut files: Vec<(String, String)> = skill
        .files
        .iter()
        .filter(|(rel, _)| !overlay.iter().any(|(o, _)| o == rel))
        .chain(overlay.iter())
        .map(|(rel, contents)| {
            let mut text = harness.substitute(contents);
            if *rel == "SKILL.md" {
                text = extend_frontmatter(&text, fields, hint);
            }
            ((*rel).to_string(), text)
        })
        .collect();
    files.sort();
    RenderedSkill { files }
}

/// Turn a `/`-separated bundle path into a native path under `base`.
fn join_rel(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, c| p.join(c))
}

/// The user's home directory. `HOME` everywhere; `USERPROFILE` (then
/// `HOMEDRIVE` + `HOMEPATH`) on Windows, where `HOME` is usually unset.
fn home_dir() -> Result<PathBuf> {
    resolve_home(|key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    })
}

/// The lookup order, split out from the environment so it can be tested
/// without mutating process-global env vars.
fn resolve_home(from: impl Fn(&str) -> Option<PathBuf>) -> Result<PathBuf> {
    from("HOME")
        .or_else(|| from("USERPROFILE"))
        .or_else(|| {
            // `%HOMEDRIVE%%HOMEPATH%` is a *concatenation* ("C:" + "\Users\dev"),
            // not a join — HOMEPATH is rooted, so joining would drop the drive.
            let mut drive = from("HOMEDRIVE")?.into_os_string();
            drive.push(from("HOMEPATH")?);
            Some(PathBuf::from(drive))
        })
        .context(
            "cannot resolve your home directory — set HOME (or USERPROFILE on Windows) \
             so the user-level skills directory can be found",
        )
}

/// Resolve `--client` into the harnesses to write.
///
/// - `Some("all")` → every known harness, whether or not it is installed.
/// - `Some(key)` → exactly that harness, by key or alias.
/// - `None` → every harness whose marker directory exists under the home dir.
///   A harness with no documented marker is never auto-selected.
fn select_harnesses(home: &Path, client: Option<&str>) -> Result<Vec<&'static Harness>> {
    let keys = || {
        HARNESSES
            .iter()
            .map(|h| h.key)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match client {
        Some("all") => Ok(HARNESSES.iter().collect()),
        Some(key) => HARNESSES
            .iter()
            .find(|h| h.matches(key))
            .map(|h| vec![h])
            .with_context(|| {
                format!(
                    "unknown --client {key:?} (expected one of: {}, all)",
                    keys()
                )
            }),
        None => {
            let detected: Vec<&'static Harness> = HARNESSES
                .iter()
                .filter(|h| {
                    h.marker.is_some_and(|m| {
                        m.iter().fold(home.to_path_buf(), |p, c| p.join(c)).is_dir()
                    })
                })
                .collect();
            if detected.is_empty() {
                anyhow::bail!(
                    "no supported coding agent detected under {} — pass --client <name|all>. \
                     Known: {}",
                    home.display(),
                    keys()
                );
            }
            Ok(detected)
        }
    }
}

/// What installing one skill for one harness would do (or did).
#[derive(Debug, PartialEq, Eq)]
pub enum SkillOutcome {
    Installed,
    UpToDate,
    /// Present but different, and `--force` was not passed: left untouched.
    Differs,
    /// Present and different; `--force` overwrote the bundled files. Any extra
    /// files the user added are listed, not deleted.
    Overwritten {
        extra: Vec<String>,
    },
}

impl SkillOutcome {
    fn label(&self, dry_run: bool) -> String {
        match self {
            Self::Installed if dry_run => "would install".into(),
            Self::Installed => "installed".into(),
            Self::UpToDate => "up to date".into(),
            Self::Differs => "differs — pass --force to overwrite".into(),
            Self::Overwritten { extra } if dry_run => format!("would overwrite{}", kept(extra)),
            Self::Overwritten { extra } => format!("overwritten{}", kept(extra)),
        }
    }
}

fn kept(extra: &[String]) -> String {
    if extra.is_empty() {
        String::new()
    } else {
        format!(
            " (kept {} unbundled file(s): {})",
            extra.len(),
            extra.join(", ")
        )
    }
}

/// Install one rendered skill into `dest` (the skill's own directory).
///
/// `--force` overwrites the bundled files but never deletes anything the user
/// added alongside them.
fn install_skill(
    skill: &RenderedSkill,
    dest: &Path,
    dry_run: bool,
    force: bool,
) -> Result<SkillOutcome> {
    if !dest.exists() {
        if !dry_run {
            write_files(skill, dest)?;
        }
        return Ok(SkillOutcome::Installed);
    }

    let identical = skill.files.iter().all(|(rel, contents)| {
        // Compare on normalized newlines: a checkout or editor that rewrote
        // CRLF shouldn't read as a local edit.
        fs::read_to_string(join_rel(dest, rel))
            .is_ok_and(|on_disk| on_disk.replace("\r\n", "\n") == contents.replace("\r\n", "\n"))
    });
    if identical {
        return Ok(SkillOutcome::UpToDate);
    }
    if !force {
        return Ok(SkillOutcome::Differs);
    }

    let extra = existing_files(dest)
        .into_iter()
        .filter(|rel| !skill.files.iter().any(|(b, _)| b == rel))
        .collect();
    if !dry_run {
        write_files(skill, dest)?;
    }
    Ok(SkillOutcome::Overwritten { extra })
}

fn write_files(skill: &RenderedSkill, dest: &Path) -> Result<()> {
    for (rel, contents) in &skill.files {
        let path = join_rel(dest, rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

/// Slash-separated relative paths of every file already under `dir`.
fn existing_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Which skills to install, given the profile and an optional `--only` name.
fn select_skills<'a>(
    all: &'a [BundledSkill],
    profile: Profile,
    only: Option<&str>,
) -> Result<Vec<&'a BundledSkill>> {
    if let Some(name) = only {
        // `--only` names a skill explicitly, so it overrides the profile: a
        // user asking for one skill by name has already made the choice the
        // profile would make for them.
        let picked: Vec<&BundledSkill> = all.iter().filter(|s| s.name == name).collect();
        if picked.is_empty() {
            anyhow::bail!(
                "unknown skill {name:?} — bundled skills: {}",
                all.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
            );
        }
        return Ok(picked);
    }
    let picked: Vec<&BundledSkill> = all
        .iter()
        .filter(|s| profile.includes(s.profile()))
        .collect();
    if picked.is_empty() {
        anyhow::bail!(
            "no bundled skill belongs to the {} profile",
            profile.label()
        );
    }
    Ok(picked)
}

/// `think-and-ship skills install [--client X] [--scope S] [--profile P] [--only N] [--dry-run] [--force]`
pub fn install(
    client: Option<&str>,
    scope: Option<&str>,
    profile: Option<&str>,
    only: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let home = home_dir()?;
    let project = std::env::current_dir().context("resolving the current directory")?;
    let scope = scope.map_or(Ok(Scope::User), Scope::parse)?;
    let profile = profile.map_or(Ok(Profile::Core), Profile::parse)?;
    let targets = select_harnesses(&home, client)?;

    let all = bundled_skills();
    let skills = select_skills(&all, profile, only)?;

    println!(
        "{} {} skill(s) [{} profile, {} scope] for {} harness(es){}",
        if dry_run { "Previewing" } else { "Installing" },
        skills.len(),
        profile.label(),
        scope.label(),
        targets.len(),
        if dry_run { " — no files written" } else { "" }
    );
    if client.is_none() {
        println!(
            "Detected from your home directory: {}. Pass --client all to write every known harness.",
            targets
                .iter()
                .map(|h| h.name)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let mut blocked = 0usize;
    let width = name_column(&skills);

    for target in &targets {
        let root = target.root(scope, &home, &project);
        println!(
            "\n{} [{}] → {}",
            target.name,
            target.tier.label(),
            root.display()
        );
        if !dry_run {
            fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        }
        for skill in &skills {
            let rendered = render_skill(skill, target);
            let outcome = install_skill(&rendered, &root.join(skill.name), dry_run, force)?;
            if outcome == SkillOutcome::Differs {
                blocked += 1;
            }
            println!("  {:<width$} {}", skill.name, outcome.label(dry_run));
        }
        // The manual-only wrapper, where the harness's own docs make one the
        // right home. Core skills only: a legacy skill has no such contract to
        // protect, and wrapping eleven of them would flood the command menu.
        if let Some(w) = &target.wrapper {
            let dir = match scope {
                Scope::User => w.user_dir.iter().fold(home.clone(), |p, c| p.join(c)),
                Scope::Project => w.project_dir.iter().fold(project.clone(), |p, c| p.join(c)),
            };
            let wrapped: Vec<&&BundledSkill> = skills
                .iter()
                .filter(|s| s.profile() == Profile::Core)
                .collect();
            if !wrapped.is_empty() {
                if !dry_run {
                    fs::create_dir_all(&dir)
                        .with_context(|| format!("creating {}", dir.display()))?;
                }
                for s in &wrapped {
                    let path = dir.join(format!("{}.md", s.name));
                    if !dry_run {
                        fs::write(&path, wrapper_body(s, target, w.kind))
                            .with_context(|| format!("writing {}", path.display()))?;
                    }
                }
                println!("  {} {}(s) → {}", wrapped.len(), w.kind, dir.display());
            }
        }
        println!(
            "  invoke with {} — {} — distributed via {}",
            target.invocation, target.reload, target.distribution
        );
    }

    if blocked > 0 {
        println!(
            "\n{blocked} skill(s) already exist with local edits and were left alone. \
             Re-run with --force to overwrite them (files you added are kept)."
        );
    }
    Ok(())
}

/// Destinations this installer USED to write and no longer does.
///
/// `~/.codex/skills` was asserted from a module comment and is not a path any
/// first-party OpenAI page documents; Codex reads `~/.agents/skills`. Leaving
/// the old tree in place is not harmless — Cursor's documented legacy
/// compatibility list includes `~/.codex/skills`, so a stale copy there is
/// still DISCOVERED, and a user would be running last version's skill without
/// knowing which one answered.
const RETIRED_DESTINATIONS: &[(&[&str], &str)] = &[(
    &[".codex", "skills"],
    "Codex reads ~/.agents/skills; this path is discovered by Cursor's legacy \
     compatibility list, so a stale copy here still shadows the current one",
)];

/// What migration found at one skill directory.
#[derive(Debug, PartialEq, Eq)]
enum Disposition {
    /// Byte-identical to what this binary renders — provably a managed copy,
    /// so removing it loses nothing.
    Unchanged,
    /// Present and different. Either locally edited or written by an older
    /// version, and **the two are indistinguishable from here** — so it is
    /// never removed without an explicit force.
    Differs,
}

/// Classify a skill directory against what this binary would render for it.
fn classify(rendered: &RenderedSkill, dest: &Path) -> Option<Disposition> {
    if !dest.exists() {
        return None;
    }
    let identical = rendered.files.iter().all(|(rel, contents)| {
        fs::read_to_string(join_rel(dest, rel))
            .is_ok_and(|on_disk| on_disk.replace("\r\n", "\n") == contents.replace("\r\n", "\n"))
    });
    Some(if identical {
        Disposition::Unchanged
    } else {
        Disposition::Differs
    })
}

/// `think-and-ship skills migrate` — retire what this installer no longer writes.
///
/// **Dry-run by default.** Nothing is removed unless `--apply` is passed, and
/// even then only directories proven byte-identical to this binary's own render.
/// A directory that differs is REPORTED and kept, because a local edit and an
/// older version's copy look exactly alike from here and deleting someone's
/// customization is not recoverable.
pub fn migrate(scope: Option<&str>, apply: bool, force: bool) -> Result<()> {
    let home = home_dir()?;
    let project = std::env::current_dir().context("resolving the current directory")?;
    let scope = scope.map_or(Ok(Scope::User), Scope::parse)?;
    let all = bundled_skills();

    println!(
        "{} migration ({} scope){}",
        if apply { "Applying" } else { "Previewing" },
        scope.label(),
        if apply { "" } else { " — no files removed" }
    );

    let mut removable = 0usize;
    let mut kept = 0usize;
    let mut removed = 0usize;

    // 1. Destinations this installer has retired.
    for (parts, why) in RETIRED_DESTINATIONS {
        let root = parts.iter().fold(home.clone(), |p, c| p.join(c));
        if !root.is_dir() {
            continue;
        }
        println!("\nRetired destination {} — {why}", root.display());
        for skill in &all {
            let dest = root.join(skill.name);
            // Compared against the render for EVERY harness: an old install
            // could have come from any of them, and matching any one of them
            // is proof enough that nothing local was added.
            let disposition = HARNESSES
                .iter()
                .filter_map(|h| classify(&render_skill(skill, h), &dest))
                .min_by_key(|d| match d {
                    Disposition::Unchanged => 0,
                    Disposition::Differs => 1,
                });
            match disposition {
                None => {}
                Some(Disposition::Unchanged) => {
                    removable += 1;
                    println!(
                        "  {:<20} unchanged managed copy — safe to remove",
                        skill.name
                    );
                    if apply {
                        fs::remove_dir_all(&dest)
                            .with_context(|| format!("removing {}", dest.display()))?;
                        removed += 1;
                    }
                }
                Some(Disposition::Differs) => {
                    if apply && force {
                        println!("  {:<20} differs — removed (--force)", skill.name);
                        fs::remove_dir_all(&dest)
                            .with_context(|| format!("removing {}", dest.display()))?;
                        removed += 1;
                    } else {
                        kept += 1;
                        println!(
                            "  {:<20} differs from every known render — KEPT. It is either \
                             your edit or an older version, and nothing here can tell those \
                             apart. Remove it yourself, or re-run with --apply --force.",
                            skill.name
                        );
                    }
                }
            }
        }
    }

    // 2. Legacy skills sitting in a CURRENT destination. These are not stale —
    // they are simply no longer part of the default surface — so they are only
    // reported, never removed. Someone may still be using them.
    for h in HARNESSES {
        let root = h.root(scope, &home, &project);
        let present: Vec<&str> = all
            .iter()
            .filter(|s| s.profile() == Profile::Legacy)
            .filter(|s| root.join(s.name).join("SKILL.md").is_file())
            .map(|s| s.name)
            .collect();
        if !present.is_empty() {
            println!(
                "\n{} has {} legacy skill(s) installed at {}: {}",
                h.name,
                present.len(),
                root.display(),
                present.join(", ")
            );
            println!(
                "  Kept. They still work. Re-install them with --profile legacy, or remove \
                 the directories yourself when you no longer want them."
            );
        }
    }

    println!("\n{removable} removable, {kept} kept, {removed} removed.");
    if !apply && removable > 0 {
        println!("Re-run with --apply to remove the unchanged copies.");
    }
    Ok(())
}

/// Build a harness's own plugin package into `out`, from the canonical source.
///
/// GENERATED, never checked in. A plugin directory committed to this repository
/// would be a second copy of both skills, and the whole point of this module is
/// that there is exactly one. `skills package` reproduces it on demand from the
/// same render the installer writes, so the two cannot disagree.
///
/// This builds and validates. It does NOT publish, and there is deliberately no
/// verb here that does.
pub fn package(client: &str, out: &Path, dry_run: bool) -> Result<()> {
    let harness = HARNESSES
        .iter()
        .find(|h| h.matches(client))
        .with_context(|| format!("unknown --client {client:?}"))?;
    let manifest = plugin_manifest(harness).with_context(|| {
        format!(
            "{} has no first-party plugin format documented — install it with \
             `skills install --client {}` instead",
            harness.name, harness.key
        )
    })?;

    let skills = bundled_skills();
    let core: Vec<&BundledSkill> = skills
        .iter()
        .filter(|s| s.profile() == Profile::Core)
        .collect();

    println!(
        "{} a {} plugin with {} skill(s) → {}",
        if dry_run { "Previewing" } else { "Building" },
        harness.name,
        core.len(),
        out.display()
    );
    for (rel, contents) in plugin_files(harness, &core, &manifest) {
        let path = join_rel(out, &rel);
        println!("  {rel}");
        if !dry_run {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        }
    }
    if !dry_run {
        println!(
            "\nBuilt, not published. Load it with {}'s own plugin mechanism, or use \
             `skills install` instead.",
            harness.name
        );
    }
    Ok(())
}

/// The plugin manifest path and contents for a harness, if it documents one.
fn plugin_manifest(harness: &Harness) -> Option<(String, String)> {
    let version = env!("CARGO_PKG_VERSION");
    match harness.key {
        "claude-code" => Some((
            ".claude-plugin/plugin.json".to_string(),
            format!(
                "{{\n  \"name\": \"think-and-ship\",\n  \"version\": \"{version}\",\n  \
                 \"description\": \"Choose a workstream, then do one unit of work in it.\"\n}}\n"
            ),
        )),
        "codex" => Some((
            "plugin.json".to_string(),
            format!(
                "{{\n  \"name\": \"think-and-ship\",\n  \"version\": \"{version}\",\n  \
                 \"description\": \"Choose a workstream, then do one unit of work in it.\",\n  \
                 \"skills\": \"./skills/\"\n}}\n"
            ),
        )),
        _ => None,
    }
}

/// Every file of a harness's plugin package, as (relative path, contents).
fn plugin_files(
    harness: &Harness,
    skills: &[&BundledSkill],
    manifest: &(String, String),
) -> Vec<(String, String)> {
    let mut files = vec![manifest.clone()];
    for s in skills {
        for (rel, contents) in render_skill(s, harness).files {
            files.push((format!("skills/{}/{rel}", s.name), contents));
        }
    }
    files.sort();
    files
}

/// Width of the skill-name column: the longest name, with a two-space gutter.
fn name_column<S: std::borrow::Borrow<BundledSkill>>(skills: &[S]) -> usize {
    skills
        .iter()
        .map(|s| s.borrow().name.chars().count())
        .max()
        .unwrap_or(0)
        + 2
}

/// `think-and-ship skills list` — what's bundled, and where each harness stands.
pub fn list(scope: Option<&str>) -> Result<()> {
    let home = home_dir()?;
    let project = std::env::current_dir().context("resolving the current directory")?;
    let scope = scope.map_or(Ok(Scope::User), Scope::parse)?;
    let skills = bundled_skills();

    println!("Bundled skills ({}):\n", skills.len());
    let width = name_column(&skills);
    for skill in &skills {
        println!(
            "  {:<width$} [{}] {}",
            skill.name,
            skill.profile().label(),
            skill.summary()
        );
    }
    println!("\nThe core profile installs by default. --profile legacy|all installs the rest.");

    let core = skills
        .iter()
        .filter(|s| s.profile() == Profile::Core)
        .count();
    println!("\nHarnesses ({} scope):", scope.label());
    for h in HARNESSES {
        let root = h.root(scope, &home, &project);
        let installed = skills
            .iter()
            .filter(|s| root.join(s.name).join("SKILL.md").is_file())
            .count();
        let state = if h
            .marker
            .is_some_and(|m| m.iter().fold(home.to_path_buf(), |p, c| p.join(c)).is_dir())
        {
            format!("detected, {installed}/{core} core installed")
        } else if installed > 0 {
            format!("not detected, {installed} installed")
        } else {
            "not detected".to_string()
        };
        println!(
            "  {:<18} {:<13} {:<34} {:<24} {state}",
            h.name,
            h.tier.label(),
            h.display_dir(scope),
            h.distribution
        );
        if let Some(w) = &h.wrapper {
            let dir = match scope {
                Scope::User => format!("~/{}", w.user_dir.join("/")),
                Scope::Project => format!("./{}", w.project_dir.join("/")),
            };
            println!("  {:<18} {:<13} {dir}", "", format!("+ {}", w.kind));
        }
    }
    println!("\nInstall with: think-and-ship skills install [--client all] [--scope project]");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn harness(key: &str) -> &'static Harness {
        HARNESSES
            .iter()
            .find(|h| h.key == key)
            .expect("known harness")
    }

    fn rendered(files: &[(&str, &str)]) -> RenderedSkill {
        RenderedSkill {
            files: files
                .iter()
                .map(|(p, c)| ((*p).to_string(), (*c).to_string()))
                .collect(),
        }
    }

    fn skill(name: &str) -> BundledSkill {
        bundled_skills()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("bundled skill {name}"))
    }

    fn skill_md(r: &RenderedSkill) -> &str {
        &r.files.iter().find(|(p, _)| p == "SKILL.md").unwrap().1
    }

    /// THE SHARED-DIRECTORY RULE, as a build failure rather than a convention.
    ///
    /// `.agents/skills` is read by eight harnesses. A harness-specific
    /// frontmatter key written there is handed to seven agents that never
    /// documented it. This is the one invariant that cannot be left to review,
    /// because the damage is invisible locally — the render looks fine, and it
    /// is another vendor's parser that rejects it.
    #[test]
    fn shared_destination_renders_are_portable() {
        for h in HARNESSES {
            if h.writes_to_shared_directory() {
                assert!(
                    h.metadata.is_empty(),
                    "{} writes to a shared .agents directory but adds frontmatter {:?} — \
                     that render would be handed to every other harness reading it",
                    h.key,
                    h.metadata.iter().map(|m| m.key).collect::<Vec<_>>()
                );
            }
        }
    }

    /// Every destination is one `docs/HARNESSES.md` records. The doc is the
    /// evidence; this test is what stops the code drifting away from it.
    #[test]
    fn every_destination_appears_in_the_verified_matrix() {
        let matrix = include_str!("../../../../docs/HARNESSES.md");
        for h in HARNESSES {
            for (scope, parts) in [("user", h.user_dir), ("project", h.project_dir)] {
                let path = parts.join("/");
                assert!(
                    matrix.contains(&path),
                    "{} {scope} destination {path} is not in docs/HARNESSES.md — \
                     verify it against first-party documentation before writing it",
                    h.key
                );
            }
        }
    }

    #[test]
    fn the_core_profile_is_exactly_the_two_new_skills() {
        let core: Vec<&str> = bundled_skills()
            .iter()
            .filter(|s| s.profile() == Profile::Core)
            .map(|s| s.name)
            .collect();
        assert_eq!(core, ["advance-work", "switch-work"]);
    }

    #[test]
    fn every_bundled_skill_has_a_skill_md_and_a_description() {
        for s in bundled_skills() {
            assert!(
                s.files.iter().any(|(p, _)| *p == "SKILL.md"),
                "skill {} has no SKILL.md",
                s.name
            );
            assert!(
                !s.summary().is_empty(),
                "skill {} has no description",
                s.name
            );
        }
    }

    /// The one-canonical-source claim, made checkable.
    ///
    /// The two core skills carry no tokens, so their rendered BODIES must be
    /// byte-identical across every harness — only frontmatter may differ. A
    /// copied or forked procedure fails here immediately.
    #[test]
    fn the_core_procedure_is_byte_identical_across_every_harness() {
        for name in ["switch-work", "advance-work"] {
            let s = skill(name);
            let body_of = |h: &Harness| {
                let md = skill_md(&render_skill(&s, h)).to_string();
                // Everything after the frontmatter block.
                let rest = md.strip_prefix("---\n").expect("frontmatter");
                let end = rest.find("\n---\n").expect("frontmatter ends");
                rest[end..].to_string()
            };
            let reference = body_of(harness("claude-code"));
            for h in HARNESSES {
                assert_eq!(
                    body_of(h),
                    reference,
                    "{}'s {name} body differs from Claude Code's — the procedure was forked",
                    h.key
                );
            }
        }
    }

    /// Rendering is deterministic: same inputs, same bytes, every time.
    #[test]
    fn rendering_is_deterministic() {
        for name in ["switch-work", "advance-work"] {
            let s = skill(name);
            for h in HARNESSES {
                assert_eq!(render_skill(&s, h).files, render_skill(&s, h).files);
            }
        }
    }

    /// A harness gets its own documented keys, and nobody else's.
    #[test]
    fn frontmatter_overlays_are_harness_specific() {
        let s = skill("switch-work");

        // Quoted: unquoted, `[workstream] [shape|build|listen]` is a YAML flow
        // sequence followed by a second one and the frontmatter fails to parse.
        let claude = skill_md(&render_skill(&s, harness("claude-code"))).to_string();
        assert!(claude.contains("argument-hint: \"[workstream] [shape|build|listen]\""));

        let cursor = skill_md(&render_skill(&s, harness("cursor"))).to_string();
        assert!(
            !cursor.contains("argument-hint"),
            "Cursor does not document argument-hint; it must not be emitted"
        );
    }

    /// NO render suppresses model invocation, in any harness's dialect.
    ///
    /// Every core skill's description advertises natural-language triggers —
    /// "advance the current work", "switch to \<workstream\>". A suppression key
    /// makes those unreachable while the description still promises them, and
    /// the failure is silent: the harness refuses the invocation and the user is
    /// told to type the slash command instead. What bounds these skills is their
    /// own doctrine, not the route in.
    #[test]
    fn no_render_suppresses_model_invocation() {
        for s in bundled_skills() {
            for h in HARNESSES {
                for (path, contents) in &render_skill(&s, h).files {
                    for forbidden in [
                        "disable-model-invocation",
                        "opencode/autoinvoke",
                        "allow_implicit_invocation: false",
                    ] {
                        assert!(
                            !contents.contains(forbidden),
                            "{}'s render of {} ({}) carries {forbidden:?}, which makes the \
                             natural-language triggers in its own description dead letters",
                            h.key,
                            s.name,
                            path
                        );
                    }
                }
            }
        }
    }

    /// A field carrying an argument hint is dropped for a skill with none,
    /// rather than emitted empty.
    #[test]
    fn an_argument_hint_is_omitted_for_a_skill_that_takes_no_arguments() {
        let md = skill_md(&render_skill(
            &skill("advance-work"),
            harness("claude-code"),
        ))
        .to_string();
        assert!(
            !md.contains("argument-hint"),
            "advance-work takes no arguments, so the hint must be omitted entirely"
        );
    }

    /// Extending frontmatter must not disturb the canonical name/description,
    /// including block-scalar formatting.
    #[test]
    fn extending_frontmatter_preserves_the_canonical_block() {
        let body = "---\nname: x\ndescription: >-\n  one\n  two\n---\n\n# Body\n";
        let out = extend_frontmatter(
            body,
            &[MetaField {
                key: "argument-hint",
                value: "\"[workstream]\"",
            }],
            None,
        );
        assert_eq!(
            out,
            "---\nname: x\ndescription: >-\n  one\n  two\nargument-hint: \"[workstream]\"\n---\n\n# Body\n"
        );
        // With no fields, the body is returned untouched.
        assert_eq!(extend_frontmatter(body, &[], None), body);
    }

    #[test]
    fn legacy_skills_still_get_their_client_tokens_substituted() {
        for s in bundled_skills()
            .iter()
            .filter(|s| s.profile() == Profile::Legacy)
        {
            for h in HARNESSES {
                for (path, contents) in &render_skill(s, h).files {
                    assert!(
                        !contents.contains("{{"),
                        "unsubstituted token in {}/{} for {}",
                        s.name,
                        path,
                        h.key
                    );
                }
            }
        }
    }

    #[test]
    fn install_writes_nested_files_then_reports_up_to_date() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("advance-work");
        let s = rendered(&[("SKILL.md", "body"), ("references/build.md", "ref")]);

        assert_eq!(
            install_skill(&s, &dest, false, false).unwrap(),
            SkillOutcome::Installed
        );
        assert_eq!(fs::read_to_string(dest.join("SKILL.md")).unwrap(), "body");
        assert_eq!(
            fs::read_to_string(dest.join("references").join("build.md")).unwrap(),
            "ref"
        );
        assert_eq!(
            install_skill(&s, &dest, false, false).unwrap(),
            SkillOutcome::UpToDate
        );
    }

    #[test]
    fn crlf_on_disk_is_not_mistaken_for_a_local_edit() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("switch-work");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("SKILL.md"), "line one\r\nline two\r\n").unwrap();
        let s = rendered(&[("SKILL.md", "line one\nline two\n")]);

        assert_eq!(
            install_skill(&s, &dest, false, false).unwrap(),
            SkillOutcome::UpToDate
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("switch-work");
        let s = rendered(&[("SKILL.md", "body")]);

        assert_eq!(
            install_skill(&s, &dest, true, false).unwrap(),
            SkillOutcome::Installed
        );
        assert!(!dest.exists(), "dry run created {}", dest.display());
    }

    #[test]
    fn a_locally_edited_skill_is_left_alone_without_force() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("switch-work");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("SKILL.md"), "mine").unwrap();
        let s = rendered(&[("SKILL.md", "bundled")]);

        assert_eq!(
            install_skill(&s, &dest, false, false).unwrap(),
            SkillOutcome::Differs
        );
        assert_eq!(fs::read_to_string(dest.join("SKILL.md")).unwrap(), "mine");
    }

    #[test]
    fn force_overwrites_bundled_files_but_keeps_user_additions() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("switch-work");
        fs::create_dir_all(dest.join("notes")).unwrap();
        fs::write(dest.join("SKILL.md"), "mine").unwrap();
        fs::write(dest.join("notes").join("local.md"), "keep me").unwrap();
        let s = rendered(&[("SKILL.md", "bundled")]);

        assert_eq!(
            install_skill(&s, &dest, false, true).unwrap(),
            SkillOutcome::Overwritten {
                extra: vec!["notes/local.md".to_string()]
            }
        );
        assert_eq!(
            fs::read_to_string(dest.join("SKILL.md")).unwrap(),
            "bundled"
        );
        assert_eq!(
            fs::read_to_string(dest.join("notes").join("local.md")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn harness_selection_detects_installed_agents_and_accepts_aliases() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        assert!(select_harnesses(home, None).is_err());

        fs::create_dir_all(home.join(".cursor")).unwrap();
        assert_eq!(
            select_harnesses(home, None)
                .unwrap()
                .iter()
                .map(|h| h.key)
                .collect::<Vec<_>>(),
            ["cursor"]
        );

        // A nested marker resolves too.
        fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        let detected = select_harnesses(home, None).unwrap();
        assert!(detected.iter().any(|h| h.key == "opencode"));

        assert_eq!(
            select_harnesses(home, Some("all")).unwrap().len(),
            HARNESSES.len()
        );
        // Aliases resolve to the canonical harness.
        for (alias, key) in [
            ("claude", "claude-code"),
            ("openai", "codex"),
            ("roo-code", "roo"),
            ("cascade", "windsurf"),
        ] {
            assert_eq!(select_harnesses(home, Some(alias)).unwrap()[0].key, key);
        }
        assert!(select_harnesses(home, Some("emacs")).is_err());
    }

    /// A harness with no documented marker is never auto-selected — the
    /// conservative answer where evidence is missing.
    #[test]
    fn a_harness_with_no_documented_marker_is_never_auto_detected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        fs::create_dir_all(home.join(".claude")).unwrap();
        // Create the directory Amp would write into; it still must not be
        // auto-selected, because that directory is not a detection marker.
        fs::create_dir_all(home.join(".config").join("agents").join("skills")).unwrap();

        let detected = select_harnesses(home, None).unwrap();
        assert!(!detected.iter().any(|h| h.key == "amp"));
        assert!(!detected.iter().any(|h| h.key == "goose"));
        // But naming it explicitly works.
        assert_eq!(select_harnesses(home, Some("amp")).unwrap()[0].key, "amp");
    }

    /// `--client all` covers every harness exactly once — no duplicate target
    /// can silently write the same destination twice.
    #[test]
    fn client_all_covers_every_harness_exactly_once() {
        let tmp = TempDir::new().unwrap();
        let all = select_harnesses(tmp.path(), Some("all")).unwrap();
        let mut keys: Vec<&str> = all.iter().map(|h| h.key).collect();
        keys.sort_unstable();
        let mut deduped = keys.clone();
        deduped.dedup();
        assert_eq!(keys, deduped, "a harness key is duplicated");
        assert_eq!(keys.len(), HARNESSES.len());
    }

    #[test]
    fn profile_selection_defaults_to_core_and_only_overrides_it() {
        let all = bundled_skills();

        let core = select_skills(&all, Profile::Core, None).unwrap();
        assert_eq!(
            core.iter().map(|s| s.name).collect::<Vec<_>>(),
            ["advance-work", "switch-work"]
        );

        let legacy = select_skills(&all, Profile::Legacy, None).unwrap();
        assert!(legacy.iter().any(|s| s.name == "roadmap"));
        assert!(!legacy.iter().any(|s| s.name == "switch-work"));

        assert_eq!(
            select_skills(&all, Profile::All, None).unwrap().len(),
            all.len()
        );

        // `--only` names a skill explicitly and so overrides the profile.
        let only = select_skills(&all, Profile::Core, Some("roadmap")).unwrap();
        assert_eq!(only.iter().map(|s| s.name).collect::<Vec<_>>(), ["roadmap"]);
        assert!(select_skills(&all, Profile::Core, Some("nope")).is_err());
    }

    #[test]
    fn scopes_resolve_to_the_documented_destinations() {
        let home = Path::new("/home/dev");
        let project = Path::new("/work/repo");

        let claude = harness("claude-code");
        assert_eq!(
            claude.root(Scope::User, home, project),
            home.join(".claude").join("skills")
        );
        assert_eq!(
            claude.root(Scope::Project, home, project),
            project.join(".claude").join("skills")
        );

        // Copilot's two scopes genuinely differ.
        let copilot = harness("copilot");
        assert_eq!(
            copilot.root(Scope::User, home, project),
            home.join(".copilot").join("skills")
        );
        assert_eq!(
            copilot.root(Scope::Project, home, project),
            project.join(".github").join("skills")
        );

        // Codex reads .agents, NOT the .codex path this installer once used.
        let codex = harness("codex");
        assert_eq!(
            codex.root(Scope::User, home, project),
            home.join(".agents").join("skills")
        );
        assert!(!codex.user_dir.contains(&".codex"));

        assert_eq!(Scope::parse("project").unwrap(), Scope::Project);
        assert_eq!(Scope::parse("USER").unwrap(), Scope::User);
        assert!(Scope::parse("machine").is_err());
    }

    /// The six fields the Agent Skills spec defines. Anything else in a render
    /// must be a key that render's own harness documents.
    const SPEC_FIELDS: &[&str] = &[
        "name",
        "description",
        "license",
        "compatibility",
        "metadata",
        "allowed-tools",
    ];

    /// Top-level frontmatter keys of a rendered `SKILL.md`.
    fn frontmatter_keys(md: &str) -> Vec<String> {
        let rest = md.strip_prefix("---\n").expect("frontmatter");
        let end = rest.find("\n---\n").expect("frontmatter ends");
        rest[..end]
            .lines()
            // Continuation lines of a block scalar or nested map are indented.
            .filter(|l| !l.starts_with(' ') && !l.trim().is_empty())
            .filter_map(|l| l.split_once(':').map(|(k, _)| k.trim().to_string()))
            .collect()
    }

    /// EVERY rendered variant's frontmatter is valid YAML.
    ///
    /// This test exists because the renderer shipped a version that was not.
    /// `argument-hint: [workstream] [shape|build|listen]` reads as a YAML flow
    /// sequence followed by a second one and fails to parse — and NOTHING
    /// caught it: the canonical source was fine, the Rust compiled, the
    /// installer wrote the file, and only running the official validator over
    /// a RENDERED variant surfaced it. A generated document is not covered by
    /// tests of its inputs.
    #[test]
    fn every_rendered_frontmatter_is_parseable_yaml() {
        for s in bundled_skills() {
            for h in HARNESSES {
                let md = skill_md(&render_skill(&s, h)).to_string();
                let rest = md.strip_prefix("---\n").expect("frontmatter");
                let end = rest.find("\n---\n").expect("frontmatter ends");
                let parsed: Result<serde_yaml::Value, _> = serde_yaml::from_str(&rest[..end]);
                let value = parsed.unwrap_or_else(|e| {
                    panic!(
                        "{}'s render of {} has unparseable frontmatter: {e}\n---\n{}\n---",
                        h.key,
                        s.name,
                        &rest[..end]
                    )
                });
                let map = value.as_mapping().expect("frontmatter is a mapping");
                // The two required fields survive rendering with their values
                // intact — an overlay must extend the block, never damage it.
                assert_eq!(
                    map.get(serde_yaml::Value::from("name"))
                        .and_then(|v| v.as_str()),
                    Some(s.name),
                    "{}'s render of {} lost or changed its name",
                    h.key,
                    s.name
                );
                assert!(
                    map.get(serde_yaml::Value::from("description"))
                        .and_then(|v| v.as_str())
                        .is_some_and(|d| !d.trim().is_empty() && d.len() <= 1024),
                    "{}'s render of {} has a missing, empty or over-long description",
                    h.key,
                    s.name
                );
            }
        }
    }

    /// EVERY rendered variant carries the spec's fields plus, at most, the keys
    /// its own harness documents.
    ///
    /// This is the rule the official validator cannot express. `skills-ref`
    /// checks spec purity, which is correct for the canonical sources and for
    /// any harness that adds nothing — but Claude Code DOCUMENTS `argument-hint`,
    /// and a render carrying it is deliberately not spec-pure. The gate that
    /// matters per-harness is this one: no key appears in a render unless that
    /// harness's row in docs/HARNESSES.md lists it.
    #[test]
    fn a_render_carries_only_the_keys_its_own_harness_documents() {
        for s in bundled_skills() {
            for h in HARNESSES {
                let md = skill_md(&render_skill(&s, h)).to_string();
                let allowed: Vec<&str> = SPEC_FIELDS
                    .iter()
                    .copied()
                    .chain(h.metadata.iter().map(|m| m.key))
                    .collect();
                for key in frontmatter_keys(&md) {
                    assert!(
                        allowed.contains(&key.as_str()),
                        "{}'s render of {} carries frontmatter key {key:?}, which that \
                         harness does not document (allowed: {allowed:?})",
                        h.key,
                        s.name
                    );
                }
            }
        }
    }

    /// Two harnesses that write the SAME directory must produce byte-identical
    /// files, or the second install silently redefines the first's skill.
    ///
    /// Amp and Goose share `~/.config/agents/skills`; Codex, Amp and Goose share
    /// the project `.agents/skills`. Nothing stops a future edit giving one of
    /// them metadata, and the damage would appear only on a machine that has
    /// both installed.
    #[test]
    fn harnesses_sharing_a_destination_render_identically() {
        /// The first harness to claim a destination, and what it wrote there.
        type FirstClaim = (&'static str, Vec<(String, String)>);

        let home = Path::new("/home/dev");
        let project = Path::new("/work/repo");
        for s in bundled_skills() {
            for scope in [Scope::User, Scope::Project] {
                let mut by_dest: BTreeMap<PathBuf, FirstClaim> = BTreeMap::new();
                for h in HARNESSES {
                    let dest = h.root(scope, home, project);
                    let files = render_skill(&s, h).files;
                    match by_dest.get(&dest) {
                        Some((first, expected)) => {
                            // Compare the INTERSECTION. A file only one harness
                            // writes (Codex's agents/openai.yaml sidecar) is
                            // additive — the other harness ignores it, and
                            // neither install destroys it. A file BOTH write
                            // must agree, or the second overwrites the first.
                            for (path, contents) in &files {
                                if let Some((_, other)) = expected.iter().find(|(p, _)| p == path) {
                                    assert_eq!(
                                        contents,
                                        other,
                                        "{} and {} both write {}/{} but disagree on its contents",
                                        first,
                                        h.key,
                                        dest.display(),
                                        path
                                    );
                                }
                            }
                        }
                        None => {
                            by_dest.insert(dest, (h.key, files));
                        }
                    }
                }
            }
        }
    }

    /// THE SAFETY PROPERTY of migration: an unchanged managed copy is
    /// recognizable, and anything else is not touched.
    ///
    /// A local edit and an older version's copy are INDISTINGUISHABLE from
    /// here — both merely "differ" — so the only safe rule is to remove what
    /// provably matches and keep everything else. This test pins both halves.
    #[test]
    fn migration_recognizes_a_managed_copy_and_refuses_to_guess_about_the_rest() {
        let tmp = TempDir::new().unwrap();
        let s = skill("switch-work");
        let claude = harness("claude-code");
        let rendered = render_skill(&s, claude);

        // Nothing there at all.
        assert_eq!(classify(&rendered, &tmp.path().join("absent")), None);

        // Byte-identical to this binary's render → provably managed.
        let managed = tmp.path().join("managed");
        write_files(&rendered, &managed).unwrap();
        assert_eq!(classify(&rendered, &managed), Some(Disposition::Unchanged));

        // A single edited byte is enough to make it unrecognizable, which is
        // the conservative direction.
        let edited = tmp.path().join("edited");
        write_files(&rendered, &edited).unwrap();
        let md = edited.join("SKILL.md");
        let mut body = fs::read_to_string(&md).unwrap();
        body.push_str("\n<!-- my note -->\n");
        fs::write(&md, body).unwrap();
        assert_eq!(classify(&rendered, &edited), Some(Disposition::Differs));

        // An OLDER version's copy also reads as Differs — the point being that
        // migration cannot tell it from the edit above, so it keeps both.
        let older = tmp.path().join("older");
        write_files(&rendered, &older).unwrap();
        fs::write(older.join("SKILL.md"), "---\nname: switch-work\n---\nold\n").unwrap();
        assert_eq!(classify(&rendered, &older), Some(Disposition::Differs));
    }

    /// The retired destination is the one this installer used to write and no
    /// longer does, and it is retired for a stated, checkable reason.
    #[test]
    fn the_retired_codex_destination_is_no_longer_a_live_destination() {
        let (parts, why) = RETIRED_DESTINATIONS
            .iter()
            .find(|(p, _)| p.contains(&".codex"))
            .expect("the stale Codex path is tracked");
        assert_eq!(*parts, &[".codex", "skills"]);
        assert!(
            why.contains(".agents/skills"),
            "the reason must name the replacement"
        );

        // No live harness writes it any more, at either scope.
        for h in HARNESSES {
            assert_ne!(
                h.user_dir, *parts,
                "{} still writes the retired path",
                h.key
            );
            assert_ne!(h.project_dir, *parts);
        }
    }

    /// Legacy skills carry a short pointer at the new surface, and it stays
    /// short: a description is loaded at startup for every installed skill, so
    /// a paragraph of migration notice on nine of them is a permanent tax.
    #[test]
    fn legacy_descriptions_point_at_the_new_surface_without_bloating_the_catalog() {
        for s in bundled_skills() {
            let d = s.summary();
            match s.profile() {
                Profile::Legacy => {
                    // The summary is truncated for display, so check the source.
                    let body = &s.files.iter().find(|(p, _)| *p == "SKILL.md").unwrap().1;
                    let rest = body.strip_prefix("---\n").unwrap();
                    let fm = &rest[..rest.find("\n---\n").unwrap()];
                    // Two accurate phrasings, not one uniform one. A workflow
                    // skill the core surface replaced says "superseded"; a
                    // specialist skill it does NOT replace says "optional",
                    // because calling business-intel superseded by
                    // advance-work would simply be false.
                    assert!(
                        fm.contains("Superseded by")
                            || fm.contains("internal doctrine")
                            || fm.contains("outside the default two-skill surface"),
                        "legacy skill {} does not tell a reader where it now stands",
                        s.name
                    );
                    // 1024 is the spec ceiling; the pointer must not push any
                    // description through it.
                    let desc: String = fm
                        .lines()
                        .skip_while(|l| !l.starts_with("description:"))
                        .map(str::trim)
                        .collect::<Vec<_>>()
                        .join(" ");
                    assert!(
                        desc.len() <= 1100,
                        "{} description grew to {}",
                        s.name,
                        desc.len()
                    );
                }
                Profile::Core | Profile::All => assert!(
                    !d.contains("Superseded"),
                    "{} is a core skill and must not advertise itself as superseded",
                    s.name
                ),
            }
        }
    }

    /// A plugin package is built from the SAME render the installer writes.
    ///
    /// That equality is the whole reason `package` generates rather than the
    /// repository checking a plugin directory in: a committed copy is a second
    /// source, and a second source drifts.
    #[test]
    fn a_plugin_package_matches_what_the_installer_writes() {
        for key in ["claude-code", "codex"] {
            let h = harness(key);
            let manifest = plugin_manifest(h).expect("documents a plugin format");
            let skills = bundled_skills();
            let core: Vec<&BundledSkill> = skills
                .iter()
                .filter(|s| s.profile() == Profile::Core)
                .collect();
            let files = plugin_files(h, &core, &manifest);

            // The manifest is present and names this crate's real version.
            assert!(
                files
                    .iter()
                    .any(|(p, c)| *p == manifest.0 && c.contains(env!("CARGO_PKG_VERSION")))
            );

            // Every skill file equals the installed render, byte for byte.
            for s in &core {
                for (rel, contents) in render_skill(s, h).files {
                    let want = format!("skills/{}/{rel}", s.name);
                    let got = files
                        .iter()
                        .find(|(p, _)| *p == want)
                        .unwrap_or_else(|| panic!("{key} plugin is missing {want}"));
                    assert_eq!(
                        got.1, contents,
                        "{key} plugin's {want} differs from the install"
                    );
                }
            }
            // Core only — a plugin carrying eleven legacy skills would undo
            // the catalog budget the core profile exists to protect.
            assert!(!files.iter().any(|(p, _)| p.contains("skills/roadmap/")));
        }
    }

    /// Codex's sidecar allows implicit invocation, on both core skills.
    ///
    /// Codex has no frontmatter control, so this file is where the policy is
    /// stated at all — and the policy is that a model may reach these skills.
    /// They are mutating, and what holds that in check is each skill's own
    /// contract, not a flag that also severs the natural-language triggers
    /// their descriptions advertise.
    #[test]
    fn the_codex_manifest_allows_implicit_invocation_for_both_core_skills() {
        let codex = harness("codex");
        for name in ["switch-work", "advance-work"] {
            let files = render_skill(&skill(name), codex).files;
            let (_, yaml) = files
                .iter()
                .find(|(p, _)| p == "agents/openai.yaml")
                .unwrap_or_else(|| panic!("{name} has no Codex manifest"));
            assert!(
                yaml.contains("allow_implicit_invocation: true"),
                "{name}'s Codex manifest does not allow implicit invocation"
            );
            let parsed: serde_yaml::Value =
                serde_yaml::from_str(yaml).expect("the manifest is valid YAML");
            assert!(
                parsed.get("interface").is_some(),
                "{name} manifest has no interface"
            );
            assert!(
                parsed.get("dependencies").is_some(),
                "{name} declares no tools"
            );
        }
    }

    /// A harness with no documented plugin format is refused, not improvised.
    #[test]
    fn packaging_refuses_a_harness_with_no_documented_plugin_format() {
        for key in ["cline", "roo", "kiro", "amp", "goose", "windsurf"] {
            assert!(
                plugin_manifest(harness(key)).is_none(),
                "{key} has no first-party plugin format in the verified matrix"
            );
        }
    }

    /// The wrapper is a POINTER. If it ever starts carrying the procedure, the
    /// single-source guarantee is gone and nothing else would notice.
    #[test]
    fn a_wrapper_points_at_the_skill_and_never_copies_it() {
        let w = harness("windsurf");
        let spec = w.wrapper.as_ref().expect("windsurf ships a workflow");
        let s = skill("advance-work");
        let body = wrapper_body(&s, w, spec.kind);

        assert!(
            body.contains("advance-work/SKILL.md"),
            "must name the source"
        );
        assert!(body.contains("Manual-only workflow"));
        assert!(
            body.chars().count() <= spec.max_chars,
            "over the documented {}-character limit",
            spec.max_chars
        );

        // The giveaway that it has become a copy: the skill's own procedural
        // headings appearing here.
        let rendered_skill = render_skill(&s, w);
        let canonical = skill_md(&rendered_skill);
        for heading in canonical.lines().filter(|l| l.starts_with("## ")).take(6) {
            assert!(
                !body.contains(heading),
                "wrapper reproduces the skill's section {heading:?} — it is a copy, not a pointer"
            );
        }
    }

    /// Only the harness whose own docs justify one gets a wrapper. A wrapper
    /// everywhere would be eleven redundant command-menu entries.
    #[test]
    fn only_windsurf_ships_a_wrapper() {
        let with: Vec<&str> = HARNESSES
            .iter()
            .filter(|h| h.wrapper.is_some())
            .map(|h| h.key)
            .collect();
        assert_eq!(with, ["windsurf"]);
        assert_eq!(harness("windsurf").tier, SupportTier::NativeWithWrapper);
    }

    #[test]
    fn paths_are_built_with_native_separators() {
        let home = Path::new("/tmp/home");
        let root = harness("claude-code").root(Scope::User, home, Path::new("/p"));
        assert_eq!(root, home.join(".claude").join("skills"));
        assert_eq!(
            join_rel(&root, "advance-work/references/build.md"),
            root.join("advance-work")
                .join("references")
                .join("build.md")
        );
    }

    #[test]
    fn home_resolution_falls_back_to_the_windows_variables() {
        let lookup = |vars: Vec<(&'static str, &'static str)>| {
            move |key: &str| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| PathBuf::from(v))
            }
        };

        assert_eq!(
            resolve_home(lookup(vec![
                ("HOME", "/home/dev"),
                ("USERPROFILE", "C:\\x")
            ]))
            .unwrap(),
            PathBuf::from("/home/dev")
        );
        assert_eq!(
            resolve_home(lookup(vec![("USERPROFILE", "C:\\Users\\dev")])).unwrap(),
            PathBuf::from("C:\\Users\\dev")
        );
        assert_eq!(
            resolve_home(lookup(vec![
                ("HOMEDRIVE", "C:"),
                ("HOMEPATH", "\\Users\\dev")
            ]))
            .unwrap(),
            PathBuf::from("C:\\Users\\dev")
        );
        assert!(resolve_home(lookup(vec![])).is_err());
    }
}
