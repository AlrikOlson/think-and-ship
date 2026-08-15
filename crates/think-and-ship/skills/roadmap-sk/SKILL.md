---
name: roadmap-sk
description: >-
  Superseded by advance-work in build mode; kept for existing users. Advance a
  roadmap whose chunks are backed by Spec Kit (speckit) SDD artifacts. Same one-
  chunk-per-invocation loop as /roadmap, but the plan is derived from and
  verified against `specs/<feature>/spec.md`, `plan.md`, and `tasks.md` instead
  of being free-form. Bootstraps a roadmap directly from a spec's prioritized
  user stories (each independently-testable story becomes one chunk), runs the
  speckit pipeline (specify → clarify → plan → tasks → implement → analyze) as
  the chunk's execution, and gates completion on the project constitution plus
  `/speckit-analyze`. Use when the user types `/roadmap-sk`, or in any speckit-
  initialized repo (`.specify/` present) when they ask to advance the roadmap.
  Falls back to plain `/roadmap` behavior when the project is not speckit-
  initialized. Not for one-off tasks.
---

# /roadmap-sk — roadmap chunks backed by Spec Kit artifacts

This is `/roadmap` with Spec Kit as the source of truth for **what a chunk is** and **what "done" means**.

Plain `/roadmap` chunks are free-form prose. In a speckit project that is a waste: the repo already
contains prioritized, independently-testable user stories with acceptance scenarios, a plan with a
Constitution Check, and a dependency-ordered task list. `/roadmap-sk` uses those as the roadmap
rather than inventing a parallel one.

**Everything in `/roadmap` still applies** — native `roadmap_*` state is the source of truth,
`ROADMAP.md` is a generated view, one chunk per invocation, think open+close are mandatory, verify
before done. This document only describes what changes. Read `{{SKILLS_DIR}}/roadmap/SKILL.md`
for the base loop if you have not.

> **Operating doctrine: `/craft`.** Same as `/roadmap` — tri-MCP interleave, atomicity,
> commit-on-`main` with the project trailer, verify-with-the-real-gate, honest negative findings.
> **If the project has a GUI**, `/craft` §B applies: build from design tokens, everything in
> Storybook, and the verify stage is `/gui-scrutiny` (Playwright, light + dark, DOM assertions).

## The central mapping

This is the whole idea. A speckit spec already *is* a roadmap; read it as one.

| Spec Kit artifact | Roadmap / think / ship |
|---|---|
| `specs/<feature>/` | A **feature-level** chunk, or a family of chunks |
| **A prioritized user story (P1, P2, …)** | **One chunk.** The template mandates they be independently testable and individually shippable — that is the definition of a chunk |
| Story priority `P<n>` | Chunk `priority` (P1 → 100, P2 → 200, …) and `deps` on the previous story |
| Story's Acceptance Scenarios | Chunk `acceptance[]` |
| `spec.md` FR-xxx / SC-xxx | Acceptance detail; cite ids in `ship_set_objective.acceptance_criteria` |
| `plan.md` Constitution Check | `ship_check(type: "review", name: "constitution-check")` |
| `plan.md` Complexity Tracking | Justification for any scope deviation; also `think_record_step` |
| `research.md` decisions (R0, R1, …) | `think_record_step(pinned: true)` — load-bearing, do not re-derive |
| `data-model.md` entities/invariants | Implementation contract; assert invariants in tests |
| `contracts/**` | Interface contract; the thing verify actually checks |
| `quickstart.md` scenarios | The verify script for the chunk |
| `tasks.md` task (T001, …) | `ship_plan(action: "add", task_id: "T001", …)` |
| A new `[NEEDS CLARIFICATION]` | `roadmap_add_chunk(status: "backlog")`, or run clarify |
| A discovered feature | A new speckit feature via specify, then a chunk |

**Chunk id convention** (portable, sortable, greppable):
`<feature-number>-<short-slug>` — e.g. `001-us1-browse`, `001-us2-runs`, `001-parity-suite`.
Keep the feature number prefix so chunks from different features never collide.

## Step 0 — Load MCP tools

Identical to `/roadmap`, and **staged** — do not bulk-load the families up front. The opening call
loads only what steps 1–3 call (`roadmap_status`, `roadmap_next`, `think_engine_status`,
`think_record_step`); every other tool is loaded at the step that uses it. See
`{{SKILLS_DIR}}/roadmap/SKILL.md` step 0 for the per-step load lines and `/craft` §A0 for why.

Nothing in the speckit pipeline needs an extra MCP tool — specify, clarify, plan, tasks, implement
and analyze are agent-executed prompts, not MCP calls. The only addition is `Bash`, for the prereq
probe in step 2.

## Step 1 — Detect Spec Kit and resolve its config (portable)

**Never hardcode paths, script languages, or command names.** Resolve everything from the project.

```bash
test -d .specify || echo "NOT-A-SPECKIT-PROJECT"
```

If `.specify/` is absent → **say so once, then run plain `/roadmap`.** Do not fabricate SDD
artifacts in a project that does not use them.

Otherwise resolve four things, in this order:

**1. Script flavor** — from `.specify/init-options.json` key `script`:

| `script` | Directory | Prereq probe |
|---|---|---|
| `py` | `.specify/scripts/python/` | `check_prerequisites.py` |
| `sh` | `.specify/scripts/bash/` | `check-prerequisites.sh` |
| `ps` | `.specify/scripts/powershell/` | `check-prerequisites.ps1` |

Not every project ships every flavor — pick the one that exists, preferring the declared `script`.

**2. Command invocation separator** — from `.specify/integration.json`:
`integration_settings.<integration>.invoke_separator`, either `-` or `.`.
So the plan command is `/speckit-plan` **or** `/speckit.plan` depending on the project's agent.
**Getting this wrong means every command you emit is wrong.** When in doubt, the prereq scripts
print correctly-separated command names in their own error messages — read one.

**3. Feature numbering** — `.specify/init-options.json` key `feature_numbering`:
`sequential` (`NNN-slug`) or `timestamp` (`YYYYMMDD-HHMMSS-slug`). Affects chunk ids and sorting.
`branch_numbering` is a deprecated alias.

**4. Hooks** — `.specify/extensions.yml`, under `hooks.before_*` / `hooks.after_*`.
**Do not run these yourself.** The slash commands dispatch them. Just know they exist so a
surprise commit or branch mid-chunk is explicable. Never evaluate a hook `condition`.

## Step 2 — Probe feature state (the portable status call)

The prereq script is the supported machine-readable probe. Three forms, all `--json`:

```bash
# Paths only — never validates, safe to call any time
<probe> --json --paths-only
# → {REPO_ROOT, BRANCH, FEATURE_DIR, FEATURE_SPEC, IMPL_PLAN, TASKS}

# Which design docs exist (requires plan.md)
<probe> --json
# → {FEATURE_DIR, AVAILABLE_DOCS:[research.md, data-model.md, contracts/, quickstart.md]}

# Implementation-ready check (requires plan.md AND tasks.md)
<probe> --json --require-tasks --include-tasks
```

**Traps, all of which cost real time:**

- **`BRANCH` is a lie in a non-git-branch workflow.** It reports the *feature directory* name (or
  `SPECIFY_FEATURE`), not `git branch --show-current`. A project can sit on `main` with feature
  `003-foo` active. Never use it as a git branch name.
- **`--paths-only` reports paths that may not exist.** `TASKS` is populated even when `tasks.md`
  is absent. Test for the file; do not infer existence from the path.
- **The non-`--paths-only` forms *fail* (non-zero) when prerequisites are missing.** That failure
  is information, not an error to route around — it tells you which speckit step is next.

### Selecting the feature — use the environment variable, not the state file

Two independent axes, and the distinction matters:

| Mechanism | Scope | Use for |
|---|---|---|
| `SPECIFY_INIT_DIR` | Which project (dir containing `.specify/`) | Multi-repo / member projects |
| `SPECIFY_FEATURE_DIRECTORY` | Which feature, **this invocation only** | ✅ Chunk-scoped work |
| `.specify/feature.json` | Which feature, **persistently** | The user's active feature |

**Prefer `SPECIFY_FEATURE_DIRECTORY`.** It targets a feature without mutating project state.
Writing `.specify/feature.json` silently changes what the *user's* next bare `/speckit-*` command
operates on — a nasty surprise mid-session. Only write it when the user is deliberately switching
features.

```bash
SPECIFY_FEATURE_DIRECTORY=specs/001-my-feature <probe> --json --paths-only
```

## Step 3 — Bootstrap the roadmap from the spec (the high-value path)

When `roadmap_status` returns zero chunks and the project has at least one `specs/<feature>/spec.md`,
**derive the roadmap instead of asking the user to invent one.**

Read `spec.md` and extract the prioritized user stories. The spec template mandates:

> User stories should be PRIORITIZED as user journeys ordered by importance. Each user story must be
> INDEPENDENTLY TESTABLE — if you implement just ONE of them, you should still have a viable MVP.

That is a chunk specification. Map it directly:

- **One chunk per user story**, `priority = <n> * 100`.
- `deps`: story `P<n>` depends on `P<n-1>` **only when genuinely sequential**. Independently
  testable means often they are not — do not invent a chain. Wire deps to real prerequisites.
- `acceptance[]`: the story's Acceptance Scenarios, verbatim where they are already testable.
- `title`: the story's own title. `name`: ≤24 chars.
- `content`: `{version:1, summary, facts:[{label:"Story",value:"US1 (P1)"}, …], sections:[…]}`.

Then add the chunks the stories do *not* cover, which is where judgment enters:

| Chunk kind | When to add | Typical deps |
|---|---|---|
| **Task generation** | `tasks.md` absent | none — unblocks everything |
| **Foundational / scaffold** | Greenfield: repo setup, toolchain, CI, design tokens, Storybook | task generation |
| **Cross-cutting guarantees** | `plan.md` names invariants every story relies on (path confinement, byte-fidelity writes, process handling) | scaffold |
| **Contract / parity suites** | `contracts/**` or an SC demanding cross-surface equivalence | the story it verifies |
| **Cross-platform / a11y / perf** | SCs that are global rather than per-story | the last story chunk |

Cross-cutting chunks are the ones a naive story-only bootstrap misses, and they are usually the
hardest. Pull them from `plan.md`'s Structure Decision and `research.md`'s risk list.

**Then stop and let the user review**, exactly as `/roadmap` does on bootstrap. Do not implement in
the same invocation as the bootstrap.

If `spec.md` does not exist at all → the first chunk is running specify. If there is a hand-written
`ROADMAP.md`, seed from it first: `THINK_AND_SHIP_PERSIST=true think-and-ship roadmap import --file ROADMAP.md`
(`--dry-run` first).

**`ROADMAP.md` is a build output.** The exporter emits trailing blank lines that trip
markdownlint MD012, so in a repo whose CI lints `**/*.md` it belongs in `.gitignore`, not in
the commit. Regenerate it on demand; never hand-edit it as the plan of record.

## Step 4 — Start the chunk, seeded from the spec

`roadmap_start_chunk(id)` → returns `chunk:<id>`.

Then `ship_set_objective`, seeded from artifacts rather than from your own paraphrase:

- `description`: chunk title + the story's "Why this priority"
- `acceptance_criteria`: the story's Acceptance Scenarios **plus** the FR/SC ids it satisfies
  (`FR-012`, `SC-003`) — ids make the ship report auditable against the spec
- `constraints`: the constitution principles the chunk touches + `plan.md` Technical Context
- `scope`: `chunk:<id>` + the feature dir + the modules from `plan.md`'s Structure Decision

Then `ship_plan` — **prefer real `tasks.md` ids over invented ones**:

```
ship_plan(action:"add", task_id:"T014", title:"<tasks.md T014 text>", task_type:"implement")
```

Using `T0xx` keeps ship, `tasks.md`, and `/speckit-implement` talking about the same units. Only
invent ids (`explore`, `verify-<chunk>`) for work `tasks.md` does not model. Task ids must be unique
per objective and are not reusable without `ship_reset`.

## Step 5 — think open

As `/roadmap`. Omit `step_number` (or take `think_engine_status.next_step_number` — **never**
`total_steps`). `execution_ref` → the first ship task. Then
`roadmap_link(id, cross_ref: "think:<N>")`.

Additionally: **pin the `research.md` decisions this chunk depends on.** They were reasoned through
once; re-deriving them wastes a session and risks silently reversing a decision. One pinned step
citing `R<n>` is enough.

## Step 6 — Run the speckit pipeline for this chunk

Emit the commands with the **resolved separator** from step 1.

Pipeline order and prerequisites:

| Step | Requires | Produces |
|---|---|---|
| specify | — | `spec.md` |
| clarify | `spec.md` | `spec.md` (resolves `[NEEDS CLARIFICATION]`) |
| plan | `spec.md` | `plan.md`, `research.md`, `data-model.md`, `quickstart.md`, `contracts/` |
| tasks | `plan.md` | `tasks.md` |
| implement | `tasks.md` | source changes |
| analyze | spec + plan + tasks | consistency report, no artifact |
| checklist | `spec.md` | `checklists/*.md` |

Run only what the chunk needs. Most chunks in a planned feature start at implement, because
specify/plan/tasks already ran. Use the probe (step 2) to decide — do not guess.

**These are agent-executed prompts, not CLI subcommands.** `specify` cannot run them. Either invoke
the slash command in-session, or dispatch via the workflow engine when a project has it:

```bash
specify workflow run <file>.yml --json    # stdout = one JSON object; stderr = live progress
```

Check `specify workflow --help` before relying on it; not every version has the same surface.

**Never bypass hooks.** If `.specify/extensions.yml` registers `before_implement`, running the
slash command runs the hook; hand-editing files does not. Two different behaviors.

## Step 7 — Explore with ministr, research with serpapi

As `/roadmap`. In a speckit project, `data-model.md` and `contracts/**` tell you *what* to look for;
ministr tells you *where it is*. Read the artifact first, then search — it is faster and stops you
re-litigating a settled design.

`ministr_references` before touching shared code. `ministr_bridge` before any cross-language
boundary. Skip serpapi only when the chunk is purely mechanical.

## Step 8 — Implement

As `/roadmap`. Two speckit-specific rules:

- **`tasks.md` is a live document.** Tick items off as they land. A chunk that ships with its tasks
  unticked leaves the next session unable to tell what is done.
- **When implementation contradicts the plan, amend the plan** — do not silently diverge. A
  `plan.md` that no longer describes the code is worse than no plan. Record the deviation in
  `think_record_step` and in Complexity Tracking.

## Step 9 — Verify (three gates, not one)

**Gate 1 — the project's real test/lint commands.** Run them exactly as CI does; version-pinned
tools differ from whatever is on `PATH`. Prefer `ship_check(command: "...")` so the server captures
the real exit code — a self-reported `passed: true` is `verified: false` and gets flagged.

```
ship_check(type:"test", name:"pytest", command:"...", required:true)
ship_check(type:"lint", name:"ruff",   command:"...", required:true)
```

**Gate 2 — `quickstart.md` scenarios** for this chunk. This is the artifact that says what proving
the story means. If a scenario cannot be run yet, say so rather than quietly skipping it.

**Gate 3 — speckit consistency.** Run analyze; record it:

```
ship_check(type:"review", name:"speckit-analyze", passed:<real result>, required:true)
ship_check(type:"review", name:"constitution-check", passed:<real result>, required:true)
```

Analyze cross-checks spec ↔ plan ↔ tasks and treats a constitution MUST conflict as CRITICAL. It is
the cheapest way to catch a chunk that drifted from its spec.

**GUI projects**: gate 2 is `/gui-scrutiny` (`/craft` §B) — Playwright, light + dark, DOM
assertions, anti-slop review. Not a screenshot glance.

**A failed required gate means the chunk is not done.** Fix and re-verify.

## Step 10 — Ship, complete, close

1. `ship_finalize(artifacts, summary)`
2. `roadmap_complete_chunk(id, ship_ref: "task:<verify-task>")`
3. `think_record_step` close — what shipped, deviations, **honest negative findings**,
   `execution_ref: "objective:shipped"`, `pinned: true` if load-bearing
4. `roadmap_link(id, cross_ref: "think:<N>")`

## Step 11 — Mutate the roadmap

As `/roadmap`, with speckit-specific triggers:

| Discovery | Action |
|---|---|
| New requirement, same feature | Update `spec.md` (clarify), then `roadmap_update_chunk` acceptance |
| New requirement, new feature | Run specify → then `roadmap_add_chunk` for its stories |
| A story turned out to be two | `roadmap_add_chunk` sub-chunks + deps, obsolete the parent |
| A story is no longer wanted | `roadmap_obsolete_chunk(id, reason)` **and** amend `spec.md` |
| Plan proved wrong | Amend `plan.md`; `roadmap_reprioritize` as a *proposal* only |
| Constitution conflict | Resolve by changing spec/plan/tasks — **never by diluting a principle**. If the principle itself is wrong, that is a constitution amendment with a version bump, and its own chunk |

Discoveries go to `backlog`, never straight to `pending`. Re-prioritization is a proposal; the
human disposes.

Then regenerate the view if the project keeps one:
`THINK_AND_SHIP_PERSIST=true think-and-ship roadmap export --format markdown > ROADMAP.md`

## Step 12 — Report, then hand off

Report as `/roadmap` does, plus:
- which speckit steps ran, and which artifacts changed
- the analyze + constitution verdicts (say "not run" if not run)
- spec/plan amendments made, and why

Then **always** end with the copy-pasteable `/roadmap-sk` handoff block for the next session:
where we are, the exact next chunk id + title, the feature dir, the resolved separator and probe
path, proven templates to clone, gotchas that cost time this run, the exact verify commands and
their honest-PASS condition, the current `think:N` high-water mark, and honest open items.

A run that ships a chunk without the handoff block is **incomplete**.

## Edge cases

- **Not a speckit project** — say so once, run plain `/roadmap`.
- **Speckit project, no spec yet** — first chunk is running specify. Do not hand-write a spec.
- **`tasks.md` missing** — the probe fails with an actionable message. First chunk generates it.
- **Multiple features in flight** — one chunk targets one feature. Set `SPECIFY_FEATURE_DIRECTORY`
  per invocation; do not rewrite `.specify/feature.json` unless switching deliberately.
- **`[NEEDS CLARIFICATION]` still in the spec** — do not guess. Run clarify, or surface the question.
  A chunk built on an unresolved marker will be rebuilt.
- **Spec and code disagree** — the code is what *is*, the spec is what *should be*. Decide which is
  wrong, fix that one, and say which you chose.
- **Constitution blocks the chunk** — stop and surface it. Do not implement around a MUST.
- **Persistence off** — `think_engine_status.persistence_enabled == false` means native state is
  in-memory only. Flag it loudly; the roadmap will not survive the session.
- **`specs/` gitignored** — some speckit repos (notably spec-kit itself) ignore `specs/` because it
  is the tool's own dogfooding output. If the project drives real work through SDD, that is a bug:
  the roadmap's source of truth is untracked. Surface it; do not silently `git add -f`.

## What this skill is NOT

- Not a spec writer — that is specify/clarify.
- Not a bulk implementer — one chunk per invocation. Use `/roadmap-run` for a frontier.
- Not a replacement for `/roadmap` in non-speckit projects — it degrades to it on purpose.
- Not a commit automaton — offer, don't push.
