---
name: roadmap-refresh-sk
description: >-
  Superseded by advance-work in shape mode; kept for existing users. Research-
  driven refresh of a roadmap whose chunks are backed by Spec Kit (speckit) SDD
  artifacts. Same research-and-reshape loop as `/roadmap-refresh`, but the spec,
  plan, research decisions, contracts and task list are both an input to the
  research and a possible output of it — a finding routes to the roadmap, the
  spec (via clarify), the plan, or the constitution, and choosing which is the
  judgment this skill adds. Runs `/speckit-analyze` as a free staleness detector
  and scans for drift between the artifacts and the code. Use when the user
  types `/roadmap-refresh-sk`, or in any speckit-initialized repo (`.specify/`
  present) when they ask to refresh, revisit, or research the roadmap. Falls
  back to plain `/roadmap-refresh` when the project is not speckit-initialized.
  Not for implementing anything.
---

# /roadmap-refresh-sk — refresh a roadmap backed by Spec Kit artifacts

This is `/roadmap-refresh` for a project whose roadmap chunks are derived from Spec Kit artifacts.

Plain `/roadmap-refresh` researches the outside world and reshapes free-form chunks. In a speckit
project that is half the job, because the project already carries a **written record of what it
believes**: prioritized user stories, numbered requirements, success criteria, research decisions
with their reasoning, a plan with a Constitution Check, and a dependency-ordered task list. Those
artifacts go stale in ways a chunk description cannot, and they are where most refresh findings
actually belong.

**Everything in `/roadmap-refresh` still applies** — native `roadmap_*` state is the source of
truth, `ROADMAP.md` is a generated view, think open + synthesize + close are mandatory, serpapi is
load-bearing, re-prioritization is a proposal, and this skill **never implements**. Read
`{{SKILLS_DIR}}/roadmap-refresh/SKILL.md` for the base loop if you have not. This document only
describes what changes.

> **Operating doctrine: `/craft`.** Same as `/roadmap-refresh` — tri-MCP interleave, ground in the
> real code before researching an alternative, honest negative findings. GUI gaps become backlog
> chunks; they are not fixed here.

Companion skills:

- `/roadmap-sk` — advance the roadmap by implementing one chunk against its spec.
- `/roadmap-refresh-sk` — pause implementation; research; reshape the roadmap **and the artifacts**.

## The one idea

A finding has four possible homes, and putting it in the wrong one is the characteristic failure of
refreshing a speckit project.

| The finding is that… | It belongs in | How |
|---|---|---|
| A chunk's description, files, or acceptance are stale | The **roadmap** | `roadmap_update_chunk` — as the base skill |
| Work exists that no chunk covers | The **roadmap** | `roadmap_add_chunk(status: "backlog")` |
| A **requirement** is wrong, missing, or ambiguous | `spec.md` | Run clarify. **Never hand-edit the spec.** |
| An **approach** is wrong — the plan no longer describes reality | `plan.md` / `research.md` | Amend, and record the deviation in Complexity Tracking |
| A **principle** is wrong | The **constitution** | An amendment with a version bump, and its own chunk. Never dilute a principle to fit a finding |

Routing is the judgment. A stale library version in a chunk description is a roadmap edit; the same
staleness in `research.md`'s R4 is a decision being overturned and must say so by name.

## What changes, step by step

Only the steps that differ from `/roadmap-refresh` are listed. The rest are unchanged.

### Step 0.5 — Detect Spec Kit and resolve its config

Before anything else:

```bash
test -d .specify || echo "NOT-A-SPECKIT-PROJECT"
```

If `.specify/` is absent → **say so once, then run plain `/roadmap-refresh`.** Do not fabricate SDD
artifacts in a project that does not use them.

Otherwise resolve the same four things `/roadmap-sk` resolves, the same way — script flavor from
`.specify/init-options.json`, the **command invocation separator** from `.specify/integration.json`
(`-` or `.`, so it is `/speckit-analyze` or `/speckit.analyze`), feature numbering, and whether
`.specify/extensions.yml` registers hooks. Getting the separator wrong means every command you emit
is wrong. See `{{SKILLS_DIR}}/roadmap-sk/SKILL.md` step 1 — it is the same resolution and there is no
reason to describe it twice.

Then probe feature state with the prereq script (`--json --paths-only` is always safe;
`--json --require-tasks --include-tasks` tells you whether there is anything to refresh *against*).

### Step 1.5 — Run analyze first. It is free staleness detection.

**Before researching anything**, run analyze with the resolved separator.

Analyze cross-checks `spec.md` ↔ `plan.md` ↔ `tasks.md` for exactly the class of drift a refresh
looks for: duplication, ambiguity, requirements with no task, tasks with no requirement,
terminology drift, and constitution conflicts. It is read-only, it costs one invocation, and it
routinely finds things a human sweep does not — a coverage table that disagrees with itself, a
requirement whose only task was obsoleted, a phase with no checkpoint.

Its findings are **inputs to this refresh**, not a separate report to hand the user. Fold them into
the scope decision at step 2.

If analyze reports a CRITICAL constitution conflict, that outranks whatever scope the user asked
for. Surface it before researching anything else.

### Step 2 — Pick the scope, with artifact-specific staleness signals

The base skill's heuristics still apply. These signals are additional and are usually stronger,
because they are mechanical:

| Signal | What it means | How to check |
|---|---|---|
| `[NEEDS CLARIFICATION]` still in `spec.md` | A decision was deferred and never made | Read the spec |
| An FR or SC reachable from no task | The requirement is unowned | Analyze reports it; or scan `tasks.md` for the id |
| A task reachable from no requirement | Either the coverage table is stale or the task is unmotivated | Compare the task ids against the coverage table |
| A **ticked** task whose code does not exist | The most dangerous kind of stale — the plan says done and nothing is there | `ministr_survey` / `ministr_symbols` for the thing it claims to have built |
| An **unticked** task whose code does exist | Cheap win; the map is behind the territory | Same |
| `plan.md`'s Constitution Check run against an older constitution version | The gate was passed against rules that have changed | Compare the version in `plan.md` against `.specify/memory/constitution.md` |
| `quickstart.md`'s Definition of Done with unticked or unaudited items | The feature's own completion criteria are unresolved | Read the DoD |
| A pinned version or floor in `spec.md` / `plan.md` | External assumption, decays fastest | serpapi it |
| A `research.md` decision older than the code it governs | The reasoning may no longer hold | `ministr` the affected area |

The ticked-but-absent and unticked-but-present rows are the two worth running every time. They are
cheap, they are mechanical, and each is a direct contradiction between the map and the territory.

### Step 3 — Open the think step, and pin what you must not re-derive

As the base skill, plus: **pin the `research.md` decisions the scope depends on.** They were
reasoned through once, at length, with their alternatives recorded. A refresh that silently
re-derives R4 and lands on the opposite answer without noticing it *is* R4 is the specific waste
this costs.

State in the opening step which `R<n>` decisions are in scope and that you intend either to confirm
or to overturn them — by name.

### Step 4 — Ground in the artifacts first, then the code

The base skill grounds in the code before researching the world. In a speckit project there is a
step before that: **the artifacts already answer many of the questions**, and they answer them with
reasoning attached.

Read in this order, and stop as soon as the question is answered:

1. `spec.md` — what the project promised, in FR/SC terms.
2. `plan.md` — Technical Context, Structure Decision, Constitution Check, Complexity Tracking.
3. `research.md` — the decisions and, crucially, the alternatives that were rejected and why.
4. `data-model.md` / `contracts/**` — the invariants and the interface the code is held to.
5. **Then** `ministr` for what the code actually does now.

`data-model.md` and `contracts/**` tell you *what* to look for; ministr tells you *where it is*.
Reading the artifact first is faster and stops you re-litigating a settled design.

**The drift scan is a first-class refresh output.** Where the artifacts and the code disagree,
record it. The rule from `/roadmap-sk` applies: the code is what *is*, the spec is what *should
be* — decide which one is wrong, fix that one, and say which you chose. Do not quietly rewrite the
spec to match code that was a mistake.

### Step 5 — Research, with the rejected alternatives in hand

Unchanged, with one addition that saves whole cycles: before searching "should we use X or Y",
check whether `research.md` already recorded that comparison. If it did, the question is not
"which is better" but "**has anything changed since that decision**" — a narrower, cheaper and far
more answerable query. Cite the decision id in the query framing so the trace shows what is being
re-tested.

### Step 7 — Route each mutation to its artifact

The base skill's seven roadmap categories all still apply. These are the additional routes, and the
discipline on each:

8. **Requirement change** → run clarify, then `roadmap_update_chunk` the affected chunks' acceptance
   to match. **Never hand-edit `spec.md`.** Clarify exists to keep the spec's structure and its
   markers coherent, and hand-edits are how a spec acquires two contradictory statements of the
   same requirement. If clarify cannot run in this session, write the question down and surface it
   rather than guessing.

9. **New feature, not just a new chunk** → run specify, then `roadmap_add_chunk` for its stories.
   A finding big enough to need its own `spec.md` should not be smuggled in as a backlog chunk in
   somebody else's feature.

10. **Plan / approach change** → amend `plan.md` and, if a decision is overturned, `research.md` —
    naming the decision id and what changed. Add a Complexity Tracking row for any deviation this
    creates. A `plan.md` that no longer describes the code is worse than no plan.

11. **Task list change** → `tasks.md` is a live document. A refresh may add tasks, mark tasks
    obsolete, or correct a coverage table. Keep the task ids stable; a renumbered task breaks every
    cross-reference in the trace, in the roadmap and in prior commits.

12. **Constitution amendment** → this is the one that needs a human. Propose it, with the principle,
    the evidence, and the version bump it implies. **Never resolve a constitution conflict by
    diluting the principle**, and never amend it inside a refresh — it is its own chunk, with its
    own review.

Everything else about mutation discipline is unchanged: surgical, no rewriting of the in-progress
chunk, discoveries to `backlog` never straight to `pending`, re-prioritization proposes only.

### Step 8 — Provenance names the artifacts

In addition to `roadmap_record_refresh` and `roadmap_link`: **the refresh summary must name which
artifacts changed.** A refresh that edited `plan.md` and `tasks.md` and recorded only "refreshed
Phase 3" leaves the next session unable to tell whether the spec still means what it says.

If the project's `specs/` is tracked, offer to commit the artifact changes; do not push. If `specs/`
is gitignored — some speckit repos ignore it because it is the tool's own dogfooding output — say
so, because it means the roadmap's source of truth is untracked, which is a finding in itself.

### Step 10 — Report

As the base skill, plus:

- Which analyze findings fed the refresh, and their disposition (fixed here / filed as a chunk /
  left with a reason).
- Which artifacts changed, by name.
- Any `research.md` decision confirmed or overturned, by id.
- Any constitution amendment proposed — separately and prominently, because it needs a decision.
- Whether `[NEEDS CLARIFICATION]` markers remain, and how many.

## Constraints beyond the base skill

- **Never hand-edit `spec.md`.** Clarify writes it. This skill is not a spec writer.
- **Never dilute a constitution principle** to make a finding fit. Amend deliberately or don't.
- **Keep task ids stable.** They are cross-referenced from think steps, roadmap chunks and commits.
- **Analyze is an input, not a deliverable.** Run it, fold it in, don't hand the user the raw report
  as though it were the refresh.
- **A decision has an id — use it.** "We reconsidered the storage choice" is unauditable;
  "R4 (SQLite over Postgres) still holds, re-tested against 2026 guidance" is a refresh.
- **Still no implementation.** Every one of the artifact edits above is documentation. A finding
  that needs code becomes a chunk.

## Edge cases beyond the base skill

- **Not a speckit project** — say so once, run plain `/roadmap-refresh`.
- **Speckit project, no `spec.md`** — there is nothing to refresh against. Suggest specify; do not
  hand-write a spec.
- **`tasks.md` missing** — the prereq probe fails with an actionable message. The refresh can still
  reshape the roadmap, but say that the task-coverage signals were unavailable.
- **Analyze cannot run** (prerequisites missing) — say so and proceed without it, flagging that the
  cheapest staleness signal was skipped. Do not simulate its output.
- **Multiple features in flight** — one refresh targets one feature. Set
  `SPECIFY_FEATURE_DIRECTORY` per invocation rather than writing `.specify/feature.json`, which
  silently changes what the user's next bare `/speckit-*` command operates on.
- **The refresh would overturn a decision the in-progress chunk depends on** — do not apply it. Say
  which decision, which chunk, and let the user finish or pivot deliberately.
- **`[NEEDS CLARIFICATION]` in the scope** — do not guess and do not research around it. Run
  clarify, or surface the question. A refresh built on an unresolved marker will be redone.
- **The constitution has been amended since `plan.md`'s Check** — that is a finding on its own, and
  usually a chunk: the plan was gated against rules that no longer apply.

## What this skill is NOT

- Not an implementer — that is `/roadmap-sk`.
- Not a spec writer — that is specify/clarify.
- Not a bootstrapper — `/roadmap-sk` derives a roadmap from a spec's user stories.
- Not a replacement for `/roadmap-refresh` in non-speckit projects — it degrades to it on purpose.
- Not a commit/push automaton — offer, don't push.
