# Workflows

> **Explanation** — background and reasoning; nothing here is needed to operate the tool ([all docs](README.md)).

How the tool families combine into working loops: roadmap-driven development,
signal-driven development, and the bundled agent skills that drive both.
The tools themselves are documented in [TOOLS.md](TOOLS.md).

## Roadmap-driven development

The `roadmap_*` family turns a project plan into **native server state**
instead of a hand-edited markdown file. A roadmap is an ordered set of
*chunks* (phases); each carries a stable id, status, priority, acceptance
criteria, dependencies, and cross-references into the `think_*`/`ship_*`
traces that realized it.

The loop, one chunk at a time:

```
roadmap_next            # the most urgent pending chunk (smallest priority number) whose deps are all done
roadmap_start_chunk     # → mark in_progress, get a chunk:<id> backref
  ship_set_objective    #   wire chunk:<id> into the execution objective
  think_record_step     #   record the reasoning; roadmap_link it back
  … implement + verify …
roadmap_complete_chunk  # mark done, attach the proof-of-ship task:<id>
```

Discoveries become `roadmap_add_chunk(status: "backlog", …)`; a plan that
drifts gets `roadmap_update_chunk` / `roadmap_obsolete_chunk`;
re-prioritization is a **proposal** (`roadmap_reprioritize`) that a human
accepts, never an automatic reorder.

`ROADMAP.md` is a **generated view**, not the source of truth — regenerate
it any time:

```sh
# seed native state once from an existing hand-written ROADMAP.md
THINK_AND_SHIP_PERSIST=true think-and-ship roadmap import --file ROADMAP.md

# regenerate the human-readable view from native state
THINK_AND_SHIP_PERSIST=true think-and-ship roadmap export --format markdown > ROADMAP.md
```

The `/roadmap` and `/roadmap-refresh` Claude Code skills drive this loop as
thin clients over the `roadmap_*` tools — `/roadmap` advances one chunk;
`/roadmap-refresh` researches and re-shapes the map.

## Signal-driven development

The `signal_*` family — lifecycle, tools, and how a signal is submitted — is
documented in [TOOLS.md](TOOLS.md#signal_--the-stakeholder-track-10-tools).

The `/signals` Claude Code skill drives this loop as a thin client over the
`signal_*` tools (sibling to `/roadmap` and `/roadmap-refresh`): a **churn**
mode that grounds each signal in the code (`ministr`) and the world (`serpapi`),
reasons (`think`), and records enrichment, then promotes or dismisses; and a
**surface** mode that raises the ready ones during normal work.

## Agent skills

The tools are only half the story — the *procedures* that drive them ship as
[Agent Skills](https://agentskills.io/specification), bundled inside the binary
and installable into twelve coding agents, so they're available in every project
rather than copied per-repo.

**Two skills install by default.** Pick where you are working, then do one thing
there:

```
switch-work authentication build
advance-work
advance-work

switch-work billing shape
advance-work
```

| Skill | What it does |
|-------|--------------|
| `switch-work` | Choose the workstream and mode. Changes no code, no chunk, no order. |
| `advance-work` | Do exactly one evidenced unit in that focus, record it, and stop. |

### Focus

`switch-work` sets a focus of `{project, lane, group, mode}`.

- **group** is the roadmap's existing `group` field — the same workstream a
  tracker maps to a project. No second taxonomy is introduced.
- **lane** is *the caller*. Focus is stored per lane, so two worktrees or two
  concurrent agents cannot overwrite one another. There is no project-wide slot
  to clobber: the store is a set of records keyed by lane. A lane-less call is
  refused rather than defaulted, because a shared default is that clobber
  wearing a per-caller name. Use your worktree's absolute path, or a session id
  that survives restarts.
- Focus persists when think-and-ship persistence is enabled; every report says
  whether it does, so a focus that will not survive a restart says so up front.

Two MCP tools back it: `roadmap_focus_get` (read-only — it cannot create a focus
as a side effect of being asked) and `roadmap_focus_set` (the only writer; an
unknown or ambiguous workstream writes nothing and returns the exact candidates).

### Modes

| Mode | One unit is | Boundary |
|------|-------------|----------|
| `shape` | A planning or research decision | Modifies no implementation source |
| `build` | A ready roadmap chunk | Refuses completion while a required check is red, skipped or unverified |
| `listen` | A stakeholder signal | Never a second signal in the same invocation |

Every invocation — completed, blocked, no-work or awaiting-human — ends with a
receipt naming the unit, the evidence, the native records, and the next
candidate. The next candidate is *recomputed, not executed*: that is what makes
stopping after one unit a handoff rather than a refusal.

**Spec Kit is an adapter, not a separate skill.** When `.specify/` exists and a
feature matches the workstream, `advance-work` uses its artifacts as the
requirements and the definition of done. When it is absent, nothing is
fabricated and no methodology is initialized.

**Optional MCP servers degrade honestly.** A code-intelligence server (ministr)
and a web-search server (SerpAPI) make grounding and research cheaper; without
them the skill uses the harness's ordinary tools and says so in the receipt's
evidence. Neither is a hard requirement, and neither absence is a reason to
refuse work.

### Installing

```sh
# core profile (switch-work + advance-work) for every detected agent
think-and-ship skills install

# one agent, or all twelve; user scope (default) or this repository
think-and-ship skills install --client codex
think-and-ship skills install --client all --scope project --dry-run

think-and-ship skills list
```

Destinations, invocation syntax and supported frontmatter for all twelve agents
are in **[HARNESSES.md](HARNESSES.md)**, each cell quoted from a first-party page
with the date it was read. The installer is forbidden from writing a destination
that file does not list.

One canonical source, rendered per agent: the two core skills carry no
substitution tokens at all, so their bodies are byte-identical everywhere and
only frontmatter differs. Anything written to a shared `.agents/skills`
destination carries the six Agent Skills spec fields and nothing else, because
eight agents read that directory.

A skill that already exists is left alone unless you pass `--force`, and even
then only bundled files are rewritten — anything you added alongside them is kept.

### The older skills

`/roadmap`, `/roadmap-run`, `/roadmap-refresh`, the two `-sk` variants,
`/signals`, `/handoff` and `/craft` still work and still install, but no longer
by default — `--profile legacy` or `--profile all`. `/business-intel`,
`/gui-blueprint` and `/gui-scrutiny` are optional specialists rather than
superseded; nothing in the core surface replaces them.

`think-and-ship skills migrate` retires the destination this installer used to
write (`~/.codex/skills`, which Codex does not read and which another agent's
compatibility list still discovers). It previews by default and removes only a
directory it can prove is an unchanged managed copy — see
**[MIGRATION.md](MIGRATION.md)**.
