---
name: handoff
description: >-
  Superseded by the advance-work receipt; kept for existing users. Produce a
  tight, copy-pasteable session-handoff prompt for the NEXT session, grounded in
  think-and-ship state (roadmap_*/ship_*/think_*/signal_*) + the current code —
  digging for the maximum RELEVANT context while carefully excluding stale,
  superseded, or misleading context. Use when the user types /handoff, says
  "write a handoff", "hand this off", "context for the next session", "what
  should the next session know", "pick up where I left off", or when a long
  session is about to end/compact. NOT for doing the work — only for packaging
  the runway.
---

# /handoff — package the runway for the next session

You produce ONE artifact: a copy-pasteable prompt the next session pastes in to start
with zero re-learning. The whole value is **signal density** — maximum *relevant* context,
zero *misleading* context. A think-and-ship trace is append-only, so it is full of
superseded decisions, abandoned branches, and stale `next_action`s; naively dumping it
poisons the handoff. Your job is to dig, then **filter ruthlessly and verify before
carrying forward**.

This skill does NOT do the work. It reads state and writes the handoff. If asked to also
do the work, do that first under the appropriate skill, then run this.

## The one law: source-of-truth over narrative

- **Native roadmap state + the current ship objective + the current code are TRUTH.**
  The think trace is *reasoning history* — useful for the "why", dangerous for the "what
  is true now". When they disagree, truth wins and you say so.
- **Never carry a think-step claim forward unverified.** A step says "the gate is
  `just foo`", "the flag is `bEnabled`", "the file is `Bar.cpp`" — these were true *when
  written*. Before putting a load-bearing fact in the handoff, confirm it against the
  CURRENT code (ministr/Read/grep) or current roadmap state. Unverifiable → label it
  "(per think:N, unverified)" or drop it.
- **Scope to the next action.** A handoff is a runway, not a memoir. If a fact doesn't
  change what the next session DOES, leave it out.

## The loop

### 1. Load tools — the OPENING set only
**Load in stages** (see `/craft` §A0). A handoff is pure reading, so the spine
comes first and the rest only if the spine says it matters:
```
select:mcp__think-and-ship__roadmap_status,mcp__think-and-ship__ship_status
```
Then, and only as each turns out to be relevant:

| Load at | Issue |
|---|---|
| The roadmap has a ready next chunk worth naming | `select:…roadmap_next` |
| Recalling reasoning behind the in-flight chunk | `select:…think_engine_status,…think_search_trace,…think_get_step` |
| The project actually uses signals | `select:…signal_status,…signal_pending` |
| A full plan projection is genuinely needed | `select:…roadmap_export` |

Then load the project's code-exploration tools if present (ministr `ministr_survey`/`ministr_symbols`/`ministr_definition`; else fall back to Grep/Read) — again, at the point you explore, not before. If think-and-ship isn't wired in this project, degrade gracefully: build the handoff from git log + the code + any ROADMAP/TODO docs, and say so.

### 2. Gather the source-of-truth state (the spine of the handoff)
- `roadmap_status` — counts, `next` (the next-ready chunk), the in-progress chunk, recent
  `done`, any `has_reprioritize_proposal`, `pending_signals`. This is the backbone: what
  ships next is here, not in the trace.
- `roadmap_next` — the exact next chunk (id/title/status/acceptance/deps). If it's an
  umbrella (deps on its own sub-chunks, or oversized), the next run DECOMPOSES it — note
  that + suggest the dep-ordered subs.
- `ship_status` — is there an unfinished objective/active task to resume, or is it clean?
- `signal_pending` (researched, high-confidence) and a glance at `signal_status` — a
  genuine stakeholder/bug signal is high-value runway; test noise is not (judge by source
  + body, not count).

### 3. Mine the trace for the "why" + the gotchas — then FILTER
- `think_engine_status` → `next_step_number`, the high-water `think:N` + 1 (the next session
  needs it to number new steps). Note it. NOT `total_steps` — that is a count of retained
  steps, not the head, and numbering from it writes an orphan step (think:1424).
- Read the most RECENT closes (the `recent_steps` rollup from any think call, or
  `think_search_trace` on the active chunk's id/keywords, or `think_get_step` on the
  pinned/recent ones). You want: the last 1–3 close steps for the current work, plus any
  pinned step that states a load-bearing fact or a hard-won gotcha.
- **Filter aggressively. EXCLUDE:**
  - Steps whose `next_action` has since been DONE (cross-check against roadmap `done`).
  - Abandoned/superseded branches and any decision later reversed (a later step revises it).
  - Anything about an `obsoleted` chunk or a removed/deferred direction.
  - Old "the plan is X" that the current roadmap contradicts.
  - Generic process narration that just restates the skill being handed to.
- **KEEP (after verifying):** the proven templates/patterns to clone, the non-obvious
  traps that cost time, the current open risks, and the single concrete next objective.

### 4. Verify the load-bearing facts against current reality
For each fact you intend to put in the handoff that the next session will ACT on (a file
to clone, a recipe to run, a gotcha to avoid), confirm it still holds:
- a named file/symbol/recipe exists → `ministr_symbols`/`ministr_definition` or a quick
  `grep`/`Read`;
- a gate command exists → grep the justfile/package.json/Makefile;
- the "next chunk" matches `roadmap_next` (not a stale think `next_action`).
Drop or downgrade anything you can't confirm. This step is what separates a handoff that
saves an hour from one that sends the next session down a deleted path.

### 5. Write the handoff prompt
ONE ``` fenced block, starting with the literal invocation line the next session will use
(`/roadmap-aaa`, `/roadmap`, `/ceo`, or a bare task line — match how this work is driven).
Tight and SPECIFIC to the next action. Include, in this order:

- **Where we are** — 1–2 lines: what just shipped, and the EXACT next chunk (`roadmap_next`
  id + title + status). If umbrella → "decompose it" + suggested dep-ordered sub-chunks.
- **This run's job** — the single concrete objective + any state mutation to apply first
  (deps to wire, status to set, an active ship task to resume).
- **Proven templates to clone** — the specific verified files/recipes this kind of work
  copies, so the next run doesn't rediscover them.
- **Gotchas that cost time** — the verified non-obvious traps carried forward from the trace.
- **The real gates** — the exact verify commands + their honest-PASS condition, and the
  current `think:N` high-water mark.
- **Honest open items** — anything blocked, deferred, half-true, or unverified (label it).

Then a one-line note OUTSIDE the block: what you verified vs. what you're passing through
unverified, and anything you deliberately excluded as stale (so the user can object).

## Discipline
- **Density over completeness.** If it doesn't change the next action, cut it.
- **Truth over optimism.** Surface blocks/deferrals/red gates plainly; never present
  unfinished work as done in the handoff.
- **No fabrication.** Don't invent a file/recipe/flag to make the runway look complete —
  an unverified pointer is worse than an honest "ground this first".
- **Don't mutate state.** This skill only READS think-and-ship + the code and WRITES the
  prompt. No `roadmap_*`/`ship_*` writes, no commits, no code edits.
- **Match the driver.** If the work is driven by a specific skill (`/roadmap-aaa`, `/ceo`),
  the handoff's first line is that invocation; otherwise a plain next-task instruction.

## What this is NOT
- Not an implementer or a roadmap mutator (that's `/roadmap` / `/roadmap-aaa` / `/ceo`).
- Not a status report for the human (that's the skill's own final message) — it's a prompt
  written FOR the next agent session.
- Not a trace dump — it is a filtered, verified, next-action-scoped runway.
