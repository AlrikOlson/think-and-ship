# Harness support matrix

Where `think-and-ship skills install` may write, what metadata each harness accepts,
and how a person invokes a skill there.

**Every cell below is copied from a first-party page, and every page carries the date
it was read.** A cell that says *not documented* means the vendor's own documentation
was checked and did not state it — it is not an invitation to guess. The installer is
forbidden from writing a destination this file does not list.

**Verified: 2026-08-08.** Re-verify before changing any destination; these vendors move.

---

## The portable core

Every harness in this file consumes the [Agent Skills](https://agentskills.io/specification)
layout: a directory whose name matches the skill, containing `SKILL.md` with YAML
frontmatter, plus optional `references/`, `scripts/` and `assets/`.

The spec (read 2026-08-08) defines exactly six frontmatter fields:

| Field | Required | Constraint |
|---|---|---|
| `name` | yes | 1–64 chars, lowercase `a-z0-9-`, no leading/trailing/consecutive hyphen, **must match the parent directory name** |
| `description` | yes | 1–1024 chars |
| `license` | no | license name or bundled file reference |
| `compatibility` | no | ≤ 500 chars |
| `metadata` | no | string→string map |
| `allowed-tools` | no | space-separated tool list (experimental) |

Body guidance from the same page: keep `SKILL.md` **under 500 lines** and the loaded
instructions **under ~5000 tokens**; keep file references **one level deep**; use
relative paths from the skill root.

`think-and-ship`'s canonical skill sources carry **only `name` and `description`**.
Everything a particular harness needs beyond that is added by the renderer at install
time, per the "supported metadata" column below.

---

## The shared-directory rule

`.agents/skills` is not one harness's directory. It is **native** to Codex, Amp and
Goose, and **also read** by Copilot, Cursor, Gemini CLI (as an alias), Windsurf, Roo
Code and OpenCode. A Claude-flavoured or Cursor-flavoured render dropped there would be
handed to seven other agents that never agreed to those fields.

So the installer applies one rule without exception:

> **Anything written to a `.agents/skills` destination is the portable render — the six
> spec fields and nothing else.** A harness that needs its own frontmatter is written to
> its own native directory instead.

The single permitted addition inside a `.agents/skills` skill directory is Codex's
`agents/openai.yaml` sidecar, which is a separate file rather than frontmatter and is
ignored by every harness that does not know it.

---

## Why no manual-only key

Four harnesses document a way to take a skill out of model discovery while leaving it
on the slash catalog: `disable-model-invocation: true` (Claude Code, Cursor),
`metadata."opencode/autoinvoke": false` (OpenCode), and
`policy.allow_implicit_invocation: false` (Codex). The installer emitted all four for
`switch-work` and `advance-work`, on the reasoning that both mutate state and should
therefore only ever be started by a person.

**That was wrong, and it is no longer emitted anywhere.**

It contradicted the skills it was applied to. Both descriptions enumerate
natural-language triggers — *"advance the current work"*, *"do the next unit"*,
*"keep going on this"*, *"switch to \<workstream\>"*, *"focus billing"* — and the key
makes every one of them unreachable while the description goes on advertising them. The
observed failure is worse than the abstract one: a user asks their agent, in prose, to
advance the work; the harness refuses the invocation; the agent reports that the skill
cannot be run and asks the user to type the slash command instead. Nothing about that
protected anyone.

The guard was also in the wrong layer. What keeps `advance-work` from running away is
its contract — one unit per invocation, the project's real gates, a receipt that
reports `no-ready-work` honestly rather than widening the search — and what keeps
`switch-work` from moving someone's work is its Boundaries section, which forbids every
mutating verb, plus a report naming the lane and workstream on every run. Those hold
whichever way the skill was entered. A frontmatter key that only changes the doorway
adds nothing to them.

What remains true, and is carried in the descriptions rather than in metadata: the
primary way to run these is for a person to type the command. Both descriptions still
front-load *"Run it when the user types…"*, and
`both_core_skills_declare_their_manual_intent_in_the_description` fails the build if
that stops being so.

`no_render_suppresses_model_invocation` fails the build if any of the four keys returns
to any render, in any harness's dialect.

---

## Matrix

Support tiers: **native** = the harness's own documentation describes Agent Skills
discovery. **equivalent** = no Agent Skills support, but a stable documented first-party
primitive carries the same procedure, and it is labelled as an adaptation rather than as
compatibility. **unsupported** = no stable documented primitive; recorded with evidence,
never approximated.

### Claude Code — native

| | |
|---|---|
| Source | <https://code.claude.com/docs/en/skills> |
| Verified | 2026-08-08 |
| User scope | `~/.claude/skills/<name>/SKILL.md` |
| Project scope | `.claude/skills/<name>/SKILL.md` |
| Shared `.agents/skills` | **not documented** — the page lists personal, project and plugin locations only |
| Precedence | enterprise > personal > project; a skill at any level overrides a bundled skill of the same name; plugin skills are namespaced `plugin-name:skill-name` and cannot conflict |
| Manual invocation | `/<directory-name>` |
| Implicit invocation | on by default — Claude may load a skill when relevant |
| Manual-only control | `disable-model-invocation: true` |
| Supported metadata | the six spec fields **plus** Claude Code extensions: `argument-hint`, `arguments`, `when_to_use`, `user-invocable`, `disallowed-tools`, `model`, `effort`, `context`, `agent`, `background`, `hooks`, `paths`, `shell` |
| Supporting files | any files in the skill directory; loaded on demand |
| Reload | live — Claude Code watches skill directories and picks up `SKILL.md` edits within the session. A newly created *top-level* skills directory needs a restart |
| Symlinks | followed; the same target reachable from two locations loads once |
| Packaging | plugin (`<plugin>/skills/<name>/SKILL.md`); a skill folder with `.claude-plugin/plugin.json` loads as `<name>@skills-dir` |
| think-and-ship render | native directory, portable frontmatter + `argument-hint`. **`disable-model-invocation` is deliberately NOT emitted** — see [Why no manual-only key](#why-no-manual-only-key) |

> **Cloud caveat, from the same page:** Cowork and cloud sessions do not read
> `~/.claude/skills/` on your machine. Cloud sessions load project skills committed to
> the cloned repository's `.claude/skills/`. This is why `--scope project` matters and is
> not a convenience flag.

> **Portability caveat, from the same page:** outside Claude Code — claude.ai uploads and
> the Skills API — only the six spec fields are accepted, and an extension field produces
> `Unexpected key(s) in SKILL.md frontmatter`. The canonical sources therefore stay on the
> six.

### Codex — native

| | |
|---|---|
| Sources | <https://learn.chatgpt.com/docs/build-skills>, <https://developers.openai.com/plugins/build/skills> |
| Verified | 2026-08-08 |
| User scope | `$HOME/.agents/skills` |
| Project scope | `$CWD/.agents/skills`, `$CWD/../.agents/skills`, `$REPO_ROOT/.agents/skills` |
| Also documented | `/etc/codex/skills` (admin), plus skills bundled with Codex |
| Shared `.agents/skills` | **this is Codex's own location** — portable render only |
| Manual invocation | `$skill-name`; `/skills` lists what is available |
| Manual-only control | `policy.allow_implicit_invocation: false` in `agents/openai.yaml` |
| Supported metadata | `SKILL.md`: `name`, `description`. Sidecar `agents/openai.yaml`: `interface` (display name, description, icons, colors, default prompts), `policy`, `dependencies` |
| MCP dependency | declared in `agents/openai.yaml` under `dependencies.tools` with `type: "mcp"`, `value`, `description`, `transport`, `url` |
| Packaging | plugin manifest pointing at a `skills` directory; a plugin may package one skill or group related ones |
| Catalog budget | **not documented as a number**; the plugins page references import limits without stating one |
| think-and-ship render | `.agents/skills` destination, portable frontmatter, `agents/openai.yaml` sidecar with `allow_implicit_invocation: true` — see [Why no manual-only key](#why-no-manual-only-key) |

> **This corrects a stale destination.** Versions of this repo before Iteration 3 wrote
> `~/.codex/skills`. No first-party page lists that path today. Migration is covered in
> [MIGRATION.md](MIGRATION.md).

### GitHub Copilot — native

| | |
|---|---|
| Sources | <https://docs.github.com/en/copilot/concepts/agents/about-agent-skills>, <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/create-skills> |
| Verified | 2026-08-08 |
| User scope | `~/.copilot/skills` or `~/.agents/skills` |
| Project scope | `.github/skills`, `.claude/skills` or `.agents/skills` |
| Surfaces | Copilot cloud agent, Copilot code review, Copilot CLI, the GitHub Copilot app, and agent mode in Visual Studio Code and JetBrains IDEs |
| Manual invocation | name the skill in the prompt with a leading slash — e.g. `Use the /frontend-design skill to …` |
| Manual-only control | `disable-model-invocation` is documented for Copilot command/skill frontmatter; the create-skills page itself lists only `name`, `description`, `license`, `allowed-tools` as skill fields |
| Supported metadata | `name`, `description`, `license`, `allowed-tools` (explicitly listed) |
| Reload | `/skills reload` during a session |
| Install / list | `copilot skill add <FILE \| URL \| DIRECTORY>`, `copilot skill list`, `/skills list` |
| think-and-ship render | `~/.copilot/skills` (user) and `.github/skills` (project), portable frontmatter + `license` |

> **Honest gap:** the create-skills page does not list `disable-model-invocation` among
> skill fields, though GitHub documents it for command frontmatter. The renderer therefore
> does **not** emit it for Copilot; the manual-invocation guard lives in the skill body
> instead. Re-check on the next verification pass.

### Cursor — native

| | |
|---|---|
| Source | <https://cursor.com/docs/skills> |
| Verified | 2026-08-08 |
| User scope | `~/.cursor/skills/`, `~/.agents/skills/` |
| Project scope | `.cursor/skills/`, `.agents/skills/`; also any nested project subdirectory, scoped to files beneath it |
| Legacy compatibility | `.claude/skills/`, `.codex/skills/`, `~/.claude/skills/`, `~/.codex/skills/` |
| Precedence | **not documented** among these locations |
| Manual invocation | type `/` in Agent chat and pick the skill |
| Manual-only control | `disable-model-invocation: true` — makes the skill behave as a traditional slash command |
| Supported metadata | `name`, `description`, `paths`, `disable-model-invocation`, `metadata` |
| Editor and CLI | skills are discovered at Cursor start; the CLI documents a slash-command surface |
| think-and-ship render | `.cursor/skills` native directory, portable frontmatter only. **`disable-model-invocation` is deliberately NOT emitted** — see [Why no manual-only key](#why-no-manual-only-key) |

### Gemini CLI — native

| | |
|---|---|
| Source | <https://geminicli.com/docs/cli/using-agent-skills/> |
| Verified | 2026-08-08 |
| User scope | `~/.gemini/skills/`, or `~/.agents/skills/` as an alias |
| Project scope | `.gemini/skills/`, or `.agents/skills/` as an alias |
| Precedence | lowest to highest: built-in → extension → user → workspace. Same name, higher location wins |
| Manual invocation | **not documented on this page**; `/skills list` enumerates |
| Implicit invocation | consent-gated — "every time a skill is triggered during a session, the agent must ask for permission to activate it" |
| Manual-only control | **not documented** (the per-activation consent prompt is the vendor's own guard) |
| Supported metadata | **not documented on this page** — the renderer emits the portable six only |
| Install / link | `gemini skills install <url>` (user scope by default, `--scope workspace` for project), `gemini skills link ./path`, `gemini skills uninstall <name>` |
| Reload | `/skills reload` or `/skills refresh`; also `/skills list`, `/skills enable`, `/skills disable` |
| Packaging | extension skills are a documented discovery tier |
| think-and-ship render | `.gemini/skills` native directory, portable frontmatter only |

### Windsurf Cascade — native (skill) + equivalent (workflow wrapper)

| | |
|---|---|
| Sources | <https://docs.windsurf.com/windsurf/cascade/skills> (307 → <https://docs.devin.ai/desktop/cascade/skills>), <https://docs.devin.ai/desktop/cascade/workflows> |
| Verified | 2026-08-08 |
| User scope | `~/.codeium/windsurf/skills/` |
| Project scope | `.windsurf/skills/` |
| Enterprise | `/Library/Application Support/Windsurf/skills/` (macOS), `/etc/windsurf/skills/` (Linux/WSL), `C:\ProgramData\Windsurf\skills\` (Windows) |
| Shared `.agents/skills` | read: `.agents/skills/`, `~/.agents/skills/`; and `.claude/skills/`, `~/.claude/skills/` when Claude Code config reading is enabled |
| Manual invocation | `@skill-name` in the Cascade input |
| Manual-only control | **none for Skills.** Windsurf's own comparison table says Workflows are "Manual only via `/slash-command`" and that Cascade never invokes a workflow automatically |
| Supported metadata | `name`, `description` |
| Supporting files | files beside `SKILL.md` become available once the skill is invoked |
| Workflow wrapper | `.windsurf/workflows/*.md` (workspace), `~/.codeium/windsurf/global_workflows/*.md` (global); **12000 characters max per file**; invoked `/<name-of-workflow>` |
| think-and-ship render | native skill in `.windsurf/skills` **plus** a thin `/switch-work` and `/advance-work` workflow that defers to the skill — because the vendor's own guidance makes Workflows the manual-only primitive |

### OpenCode — native

| | |
|---|---|
| Source | <https://opencode.ai/v2/docs/skills> |
| Verified | 2026-08-08 |
| User scope | `~/.config/opencode/skills` |
| Project scope | `.opencode/skills` (searched upward from the working directory) |
| Compatibility reads | `~/.claude/skills`, `~/.agents/skills`, `.claude/skills`, `.agents/skills` |
| Skill id | derived from the **file path, not frontmatter** — `git-release/SKILL.md` → id `git-release`; case-sensitive and exact |
| Precedence | built-in → `.claude/skills` → `.agents/skills` → `~/.config/opencode/skills` → project `.opencode/skills` → explicit `skills` config entries; later wins on matching id |
| Manual invocation | `/<id>` in the V2 CLI, unless `slash` is `false` |
| Manual-only control | `metadata."opencode/autoinvoke": false` omits the skill from model discovery while leaving it on the slash catalog |
| Supported metadata | `name`, `description`, `slash`, `metadata."opencode/slash"`, `metadata."opencode/autoinvoke"` |
| Permissions | rules use `skill` as the action and the skill id as the resource, with `allow` / `deny` / `ask` |
| think-and-ship render | `.opencode/skills` native directory, portable frontmatter only. **`opencode/autoinvoke: false` is deliberately NOT emitted** — see [Why no manual-only key](#why-no-manual-only-key) |

### Cline — native

| | |
|---|---|
| Source | <https://docs.cline.bot/customization/skills> |
| Verified | 2026-08-08 |
| User scope | `~/.cline/skills/` (macOS/Linux), `C:\Users\<USER>\.cline\skills\` (Windows) |
| Project scope | `.cline/skills/` (recommended); also `.clinerules/skills/`, `.claude/skills/` |
| Shared `.agents/skills` | **not documented** |
| Manual invocation | type `/` in chat and pick the skill, e.g. `/aws-deploy` |
| Implicit invocation | yes — Cline matches the request against skill descriptions and activates via the `use_skill` tool |
| Manual-only control | **none documented.** The only control is a per-skill enable/disable toggle in the UI |
| Supported metadata | `name` (must exactly match the directory), `description` (max 1024 chars) |
| Body guidance | instructions level is documented as "under 5k tokens" |
| think-and-ship render | `.cline/skills` native directory, portable frontmatter only |

> **Mitigation for the missing control, as required.** Because Cline can auto-activate a
> mutating skill, the rendered description is narrowed to explicit-invocation phrasing and
> the skill body's **first step** is an explicit-invocation guard: if the skill was not
> invoked by name, it reports that and stops without mutating anything. This is a
> mitigation, not a substitute for a vendor control, and is recorded as a limitation.

### Roo Code — native

| | |
|---|---|
| Source | <https://roocodeinc.github.io/Roo-Code/features/skills/> |
| Verified | 2026-08-08 |
| User scope | `~/.roo/skills/<name>/SKILL.md`; Windows `%USERPROFILE%\.roo\skills\<name>\SKILL.md`; also `~/.agents/skills/<name>/SKILL.md` |
| Project scope | `.roo/skills/<name>/SKILL.md`; also `.agents/skills/<name>/SKILL.md` |
| Mode-specific | `~/.roo/skills-<mode>/`, `.roo/skills-<mode>/`, `~/.agents/skills-<mode>/`, `.agents/skills-<mode>/` |
| Precedence | project overrides global; `.roo/` beats `.agents/` at the same level; mode-specific outranks generic at the same project level |
| Manual invocation | **not documented on this page**; skills activate on-demand when a request matches the description |
| Manual-only control | **not documented** |
| Supported metadata | `name` (exact directory match, 1–64 chars, lowercase alphanumeric and hyphens, no leading/trailing/consecutive hyphen), `description` (1–1024 chars) |
| Symlinks | supported; the symlink name becomes the skill identifier |
| Reload | file watchers detect changes during development |
| think-and-ship render | **generic** `.roo/skills`, not `skills-code/` — these skills span shape, build and listen, so scoping them to Code mode would hide them from exactly the modes that need them |

### Amp — native

| | |
|---|---|
| Sources | <https://ampcode.com/news/agent-skills>, <https://ampcode.com/news/user-invokable-skills> |
| Verified | 2026-08-08 |
| User scope | `~/.config/agents/skills/` |
| Project scope | `.agents/skills/` (the documented default install location) |
| Compatibility reads | `.claude/skills/`, `~/.claude/skills/` |
| Manual invocation | command palette (`Ctrl-O` in the CLI, `Cmd/Alt-Shift-A` in the editor extensions) → `skill: invoke` |
| Manual-only control | **not documented** |
| Supported metadata | `name` (must match the directory), `description` |
| Listing | `amp skills list`, `--json` for machine-readable output |
| think-and-ship render | `.agents/skills` (project) and `~/.config/agents/skills` (user), portable render only |

### Goose — native

| | |
|---|---|
| Source | <https://block.github.io/goose/docs/mcp/skills-mcp/> |
| Verified | 2026-08-08 |
| User scope | `~/.config/agents/skills/` |
| Project scope | `.agents/skills/` |
| Manual invocation | **not documented**; Goose discovers skills at startup and uses them when relevant |
| Manual-only control | **not documented** |
| Supported metadata | `name`, `description` |
| think-and-ship render | `.agents/skills` (project) and `~/.config/agents/skills` (user), portable render only |

### Kiro — native

| | |
|---|---|
| Source | <https://kiro.dev/docs/skills/> |
| Verified | 2026-08-08 |
| User scope | `~/.kiro/skills/` |
| Project scope | `.kiro/skills/` |
| Shared `.agents/skills` | **no** — the documentation does not list `.agents/skills` |
| Manual invocation | type `/` in the chat input followed by the skill name |
| Manual-only control | **not documented** |
| Supported metadata | `name` (matches folder, ≤64 chars), `description` (≤1024 chars), `license`, `compatibility`, `metadata` |
| think-and-ship render | `.kiro/skills` native directory, portable frontmatter only |

### Continue — unsupported

| | |
|---|---|
| Sources checked | <https://docs.continue.dev/reference>, <https://docs.continue.dev/ide-extensions/agent/how-it-works> |
| Verified | 2026-08-08 |
| Finding | Continue documents agents composed of **models, rules and tools (MCP servers)** configured through `config.yaml`. No first-party page found in this sweep describes `SKILL.md` discovery or an equivalent manual procedural-command primitive |
| Decision | **not supported.** Rules are a behavioural-guideline primitive, not a manual procedural command, so adapting these two skills to it would misrepresent the contract. Re-evaluate on the next verification pass |

---

## Extras evaluated

Third-party aggregators list many more agents as "Agent Skills compatible". Aggregators
are not evidence. Of the harnesses named as candidates:

| Candidate | Outcome |
|---|---|
| Amp | **added — native**, first-party docs, `.agents/skills` + `~/.config/agents/skills` |
| Goose | **added — native**, first-party docs, `.agents/skills` + `~/.config/agents/skills` |
| Kiro | **added — native**, first-party docs, `.kiro/skills` + `~/.kiro/skills` |
| Continue | **not added**, see above |
| OpenHands, Aider | not added — no first-party Agent Skills documentation located in this sweep. Recorded as unevaluated-by-evidence rather than unsupported-by-evidence, which is a weaker claim and the honest one |

---

## What this file obliges the installer to do

1. Never write a destination absent from this file.
2. Write only the portable render to any `.agents/skills` destination.
3. Emit a harness's own frontmatter only into that harness's own native directory, and
   only fields its "supported metadata" row lists.
4. Carry the support tier into `skills list` output so a user can see what is native,
   what is an adaptation, and what is unsupported.
5. Re-verify this file before changing any row. The date at the top is the claim.
