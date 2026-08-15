---
name: roadmap
description: >-
  Superseded by advance-work in build mode; kept for existing users. Advance an
  ever-evolving project roadmap by implementing the next chunk. The roadmap
  lives as native `roadmap_*` state in the think-and-ship MCP server (ROADMAP.md
  is a generated *view*, not the source of truth). Pick the next ready chunk,
  think-MCP plan it, ship-MCP track execution, ministr-MCP explore the affected
  code, serpapi-MCP research where prior art helps, implement + verify the
  chunk, then mutate native roadmap state with discoveries and re-
  prioritization. Use when the user types `/roadmap`, asks to "do the next thing
  on the roadmap", or asks to "keep going on v1.0/v2.0/whatever". Not for one-
  off tasks — those should be done directly.
---

# /roadmap — drive an evolving roadmap one chunk at a time

You are the operator of a long-running roadmap. Each invocation does **one chunk** end-to-end (plan → implement → verify → mutate the roadmap). The roadmap is *not* a frozen plan — it changes shape as implementation surfaces new facts.

The roadmap is **native state** in the unified **think-and-ship** MCP server, exposed through the `roadmap_*` tool family. You mutate it with tool calls, not by hand-editing markdown. `ROADMAP.md` is a **generated view** (`think-and-ship roadmap export`), not the source of truth. Two more MCP servers — `serpapi` (research) and `ministr` (code exploration) — are mandatory infrastructure, not optional aids.

> **Operating doctrine (apply automatically): `/craft`.** This skill runs under the
> house style in `{{SKILLS_DIR}}/craft/SKILL.md` — the tri-MCP interleave (think →
> ministr-ground → serpapi-2026 → think-synthesise), atomicity, commit-on-`main` with
> the project trailer, verify-with-the-real-gate (never mask an exit code), honest
> negative findings, and object-oriented "huge transformation" framing. **If the
> project has a GUI** (Storybook / web / Tauri / desktop), the chunk also obeys
> `/craft` §B — build from design tokens, put **everything in Storybook**, and make the
> **verify stage `/gui-scrutiny`**: empirical Playwright review, light + dark, visual
> *and* mechanical (DOM assertions, not vibes), critiqued like a UX expert. The user
> should not have to restate any of this per invocation.

The think-and-ship server has three tool families:

- **`think_*`** = reasoning traces (the *thinking* track).
- **`ship_*`** = execution traces (the *doing* track).
- **`roadmap_*`** = the long-horizon plan-of-plans that sits above ship objectives and links to think steps.

A `roadmap` chunk is realized by a `ship` objective, motivated by `think` reasoning. The three families cross-reference into one graph.

## When to invoke this skill

- User typed `/roadmap` (optionally with a hint).
- User said "do the next thing on the roadmap" / "advance the v1.0 plan" / "keep going on rikttp" / equivalent.
- User is mid-multi-session project and wants the next slice.

Do **not** invoke for one-off tasks ("fix this bug", "add this feature"). Do those directly. The roadmap loop is for sustained iteration.

## Inputs

- No args → pick the next ready chunk (`roadmap_next`).
- `/roadmap <chunk-id-or-title-fragment>` → work on a specific chunk (helpful when re-ordering).
- `/roadmap status` → just report current state (`roadmap_status`) and exit (no implementation).
- `/roadmap small` / `/roadmap big` → bias toward a chunk of that size.
- Anything else after `/roadmap` → treat as freeform guidance for chunk selection.

## Roadmap state (native `roadmap_*`)

The roadmap is a set of **chunks** held in the server. Each chunk has: a stable `id` slug (e.g. `phase-26e`), `title`, `status`, `priority` (lower sorts earlier), `description`, `acceptance[]`, `deps[]` (chunk ids that must be `done` first), `cross_refs[]` (links into `think:`/`task:`/`action:`/`check:`), and a `shared` flag (committed vs gitignored partition).

Chunk lifecycle (`status`): `backlog → pending → in_progress → blocked → done`, plus `obsoleted`. Transitions are validated server-side; illegal jumps are rejected.

**Persistence:** native state survives across sessions only when `THINK_AND_SHIP_PERSIST=true` (the server's default is off). If a `roadmap_status` returns an empty roadmap on a project you know has chunks, suspect persistence is disabled or the data dir differs.

**`ROADMAP.md` is a generated view.** Regenerate it any time with `think-and-ship roadmap export --format markdown > ROADMAP.md` (run with `THINK_AND_SHIP_PERSIST=true` so the CLI reads the same on-disk state). Never hand-edit it as the source of truth; if a project still has a hand-written `ROADMAP.md` and an empty native roadmap, seed it once with `think-and-ship roadmap import --file ROADMAP.md` (see "No native roadmap yet").

## The loop

### 0. Load MCP tools — the OPENING set only

**Load in stages. Do not bulk-load the families up front.** See `/craft` §A0
("Staged tool loading") for the doctrine and the reason; this section is its
application to `/roadmap`.

The opening call loads only what steps 1–3 actually call:

```
ToolSearch(
  query: "select:mcp__think-and-ship__roadmap_status,mcp__think-and-ship__roadmap_next,mcp__think-and-ship__think_engine_status,mcp__think-and-ship__think_record_step",
  max_results: 4
)
```

That is the whole opening budget. Every other tool is loaded **at the step that
uses it**, and each step below names its own. The load lines are:

| Load at | Issue |
|---|---|
| Step 4, once a chunk is picked | `select:…roadmap_start_chunk,…ship_set_objective,…ship_plan,…ship_start` |
| Step 5, grounding | `select:…ministr_survey,…ministr_symbols,…ministr_definition,…ministr_read` — add `…ministr_references,…ministr_impact,…ministr_related` only before modifying shared code, `…ministr_bridge` only at an IPC/FFI boundary, `…ministr_extract` for atomic claims from prose, `…ministr_usage` when asking whether a symbol is still worth keeping |
| Step 5, research (skip if purely internal) | `select:mcp__serpapi__search` |
| Step 6, implementing | `select:…ship_record,…ship_complete,…ship_block` |
| Step 7, verifying | `select:…ship_check` |
| Step 8, closing | `select:…roadmap_complete_chunk,…roadmap_link,…ship_finalize` |
| Step 9, roadmap mutations — only the verbs this run will call | `select:…roadmap_add_chunk` / `…roadmap_update_chunk` / `…roadmap_set_status` / `…roadmap_obsolete_chunk` / `…roadmap_reprioritize` |
| Only when needed | `…think_pin_step`, `…think_trace_checkpoint`, `…think_search_trace`, `…think_get_step`, `…think_revise_estimate`, `…think_step_impact`, `…think_set_branch_status`, `…roadmap_record_refresh`, `…roadmap_export`, `…ship_status`, `…ship_export`, `…ship_reset` |

A run that never obsoletes a chunk must never load `roadmap_obsolete_chunk`. A
run that touches no shared code must never load `ministr_impact`. If you find
yourself loading a tool "just in case", that is the anti-pattern this staging
exists to stop.

`ship_status` is the exception worth calling out: load it **after a context
compaction** to rebuild state, not before.

What each family is for (reference — this table is documentation, not a
shopping list; loading a tool is a separate, deliberate act):

- **`roadmap_*`** (the plan-of-plans — source of truth):
  - `roadmap_status` — counts + the next-ready chunk + the priority-sorted chunk list. Call first to reconstruct the plan.
  - `roadmap_next` — the lowest-priority `pending` chunk that carries no blocker and whose deps are all `done`. A blocker-carrying chunk is skipped, not hidden: it keeps its priority and its place in `roadmap_status`, with a `blocker_kind` token on its row saying why (and `counts.blocked_by` tallying the board by kind).
  - `roadmap_start_chunk` — mark a chunk `in_progress`; returns the `chunk:<id>` backref to wire into the ship objective + a think step.
  - `roadmap_complete_chunk` — mark `done`, attaching a proof-of-ship cross-ref (e.g. `task:<id>`).
  - `roadmap_add_chunk` — add a chunk (id, title, status, priority, description, acceptance, deps, shared).
  - `roadmap_update_chunk` — patch descriptive fields (title/priority/description/acceptance/deps).
  - `roadmap_set_status` — transition a chunk's status (validated).
  - `roadmap_obsolete_chunk` — mark `obsoleted` with a reason (kept for history).
  - `roadmap_reprioritize` — record a re-prioritization *proposal* (does NOT reorder — human decision).
  - `roadmap_link` — attach a `think:`/`task:`/`action:`/`check:`/`chunk:` cross-ref to a chunk.
  - `roadmap_record_refresh` — record refresh provenance (summary + the think steps behind it).
  - `roadmap_export` — markdown/json projection (the ROADMAP.md-shaped view).

- **`think_*`** (reasoning — the "thinking" track):
  - `think_record_step` — open / close / mid-flight reasoning steps (mandatory open + close).
  - `think_engine_status` — check trace state before the first step. Step numbers are project-global; `next_step_number` is the next safe one. `total_steps` is a COUNT of retained steps, NOT the head — numbering from it writes an orphan step (think:1424).
  - `think_trace_checkpoint` — whole-trace health at end of long iterations.
  - `think_pin_step` — pin a load-bearing finding for future iterations.
  - `think_search_trace` / `think_get_step` — recall prior discoveries.
  - `think_revise_estimate` — adjust `estimated_total` if the chunk grows.
  - `think_step_impact` — before revising a prior step, see what depends on it.
  - `think_set_branch_status` — mark a branch active/merged/abandoned.

- **`ship_*`** (execution — the "doing" track):
  - `ship_set_objective` — define the chunk's goal + acceptance criteria.
  - `ship_plan` — break the chunk into concrete tasks.
  - `ship_start` — begin work on a task.
  - `ship_record` — log actions (code, test, debug, research). Cross-ref to think via `think_step`.
  - `ship_complete` — close a task with artifacts produced.
  - `ship_block` — mark a task blocked with reason.
  - `ship_check` — record quality gates (test, lint, typecheck, build).
  - `ship_finalize` — finalize the objective, review all checks.
  - `ship_status` — full state snapshot (recovery after context loss).
  - `ship_reset` — wipe execution state before a fresh objective.

- **`serpapi`** (research): `serpapi_search` — design-choice validation, prior art, 2026 SOTA.
- **`ministr`** (code exploration): `ministr_toc` / `ministr_survey` / `ministr_symbols` / `ministr_definition` / `ministr_read` / `ministr_references` are the PRIMARY exploration surface; `ministr_bridge` is mandatory before touching any cross-language boundary.

If any server isn't available, note it and proceed — but flag the gap in the final report.

### 1. Load roadmap state + check ship state

Call `roadmap_status`. Identify:
- the chunk currently `in_progress` (if any — finish or close it before starting new work);
- the next-ready chunk (`next` field, or call `roadmap_next`) — the top `pending` chunk with no blocker and all deps `done`;
- recently-`done` chunks (for context on what's already shipped).

Also call `ship_status` — if there's an active objective from a prior session, either continue it or `ship_reset`.

If `/roadmap status` was requested, stop here and report state (the `roadmap_status` snapshot + ship status).

If the user gave a hint, prefer the matching chunk; otherwise take `roadmap_next`. If `roadmap_status` returns an empty roadmap, see "No native roadmap yet" under Edge cases.

### 2. Start the chunk + set up execution tracking

Mark the chunk in progress and capture its backref:

```
roadmap_start_chunk(id: "<chunk-id>")   # → returns backref "chunk:<id>"
```

Then `ship_set_objective` with:
- `description`: the chunk title + what "done" looks like
- `acceptance_criteria`: the chunk's `acceptance[]`
- `constraints`: scope boundaries
- `scope`: files/modules likely touched (mention the `chunk:<id>` backref)

Then `ship_plan` to add tasks for each sub-step you anticipate (typically 2–5 per chunk):
```
ship_plan(action: "add", task_id: "explore",   title: "Explore affected code",   task_type: "research")
ship_plan(action: "add", task_id: "implement", title: "Write the implementation", task_type: "implement")
ship_plan(action: "add", task_id: "test",      title: "Add tests",                task_type: "test")
ship_plan(action: "add", task_id: "verify",    title: "Run full verification",    task_type: "review")
```

> Task ids are unique per objective and can't be reused across a `ship_reset`-less session — if a prior phase already used `verify`, pick a fresh id like `verify-<chunk>`.

### 3. Open with a `think` step

Mandatory. Use `think_record_step`. Step numbers are **project-global**: OMIT `step_number` and the engine appends at the head, or take it from `think_engine_status.next_step_number`. Never from `total_steps` — that is a count, not the head.
- `purpose`: "Open Phase N — <chunk title>"
- `context`: what's been shipped before this; what this chunk addresses
- `thought`: the sub-plan — concrete sub-steps, file list, risk areas
- `outcome`: the sub-plan locked in
- `next_action`: the first concrete file to touch
- `execution_ref`: `"task:explore"` (cross-ref to the first ship task)
- `dependencies`: prior step numbers if continuing an existing trace

Then `roadmap_link(id: "<chunk-id>", cross_ref: "think:<N>")` to wire the reasoning into the chunk graph.

### 4. Explore the affected code with `ministr`

Start the exploration task: `ship_start(task_id: "explore")`

Mandatory unless the chunk is pure docs/config:
- `ministr_toc` for a structural overview.
- `ministr_survey(query: "...")` for any "where does X happen?" question.
- `ministr_symbols(query: "...")` for "find this struct/fn".
- `ministr_definition(id: ...)` / `ministr_read(id: ...)` for full source.
- `ministr_references(id: ...)` **before modifying shared code**.

Log what you found: `ship_record(type: "research", description: "...", think_step: <current>)`

Complete the exploration task: `ship_complete(task_id: "explore")`

### 5. Research with `serpapi` when the chunk benefits from prior art

Mandatory for chunks involving design choices, technique selection, or novelty claims. Use `serpapi_search`.

Log research actions: `ship_record(type: "research", description: "searched for X, found Y", think_step: <current>)`

Skip only if the chunk is purely mechanical.

### 6. Implement

Start the implementation task: `ship_start(task_id: "implement")`

- Make edits with `Edit` / `Write` (read first if existing).
- Log significant actions: `ship_record(type: "code", description: "added X to Y", files_touched: [...], tools_used: ["Edit"])`
- For *load-bearing* mid-flight decisions, record a `think_record_step` with `execution_ref: "task:implement"`.
- When you produce artifacts, record them: `ship_complete(task_id: "implement", artifacts: [{type: "file", ref: "path", description: "..."}])`

### 7. Verify

Start the verification task: `ship_start(task_id: "verify")`

Use the project's actual verification commands:
- Rust: `cargo test && cargo clippy --all-targets -- -D warnings`
- Node/TS: `npm test && npm run lint && npm run typecheck`
- Python: `pytest && ruff check && mypy`
- Go: `go test ./... && go vet ./...`
- Unknown: ask the user once, then record the answer (as a chunk note or in the README).

Record each gate:
```
ship_check(type: "test", name: "cargo test", passed: true, details: "596 tests pass", required: true)
ship_check(type: "lint", name: "cargo clippy", passed: true, details: "no warnings", required: true)
```

If verification fails, **do not mark the chunk done**. Record the failure (`ship_check(passed: false, ...)`), fix the issue, and re-verify.

Complete the task: `ship_complete(task_id: "verify")`

### 8. Ship with `ship`

Call `ship_finalize(artifacts: [...], summary: "Phase N shipped: <one-line>")`. This reviews all checks and warns about any required gates that failed. The ship report goes into your final message.

### 9. Mark the chunk done (native)

Mark the chunk done and attach the proof-of-ship cross-ref:

```
roadmap_complete_chunk(id: "<chunk-id>", ship_ref: "task:verify")
```

If the chunk is `shared` and the server runs with `THINK_AND_SHIP_SYNC_TARGET=repo-git`, completion commits the roadmap session as a git-native Agent Trace record.

### 10. Close with a `think` step

Mandatory. Use `think_record_step` again:
- `purpose`: "Close Phase N — outcome"
- `thought`: what shipped; bugs discovered + fixes; plan deviations + reasons; honest negative findings
- `outcome`: one-paragraph result
- `next_action`: what the next roadmap chunk should be
- `execution_ref`: `"objective:shipped"`
- `pinned: true` for genuine load-bearing findings

For long traces, follow with `think_trace_checkpoint`. Link it: `roadmap_link(id: "<chunk-id>", cross_ref: "think:<N>")`.

### 11. Mutate the roadmap (native), then regenerate the view

Mutate **native state**, not markdown:
- The finished chunk is already `done` (step 9). Confirm via `roadmap_status`.
- **Discoveries** → `roadmap_add_chunk(status: "backlog", ...)`. Do not auto-promote to pending.
- **Obsoleted chunks** → `roadmap_obsolete_chunk(id, reason)`; ask whether to keep as history (the server keeps it either way).
- **Re-prioritization** → `roadmap_reprioritize(id, suggested_priority, reason)` — a *proposal* only. Surface it; the server never reorders on its own.
- **Split a too-big chunk** → `roadmap_add_chunk` the sub-chunks with `deps` wiring, `roadmap_obsolete_chunk` or `roadmap_update_chunk` the parent.

Then regenerate the human-readable view if the project keeps a `ROADMAP.md`:
```
THINK_AND_SHIP_PERSIST=true think-and-ship roadmap export --format markdown > ROADMAP.md
```
(Or call `roadmap_export` and write the result.) Treat `ROADMAP.md` as a build output — never the place you record changes.

### 12. Report

Final user-facing message:
- What chunk shipped (title + 1 sentence).
- Test/lint state (X/Y passing, what's new).
- ship report: task completion stats, check summary, warnings.
- Honest findings worth surfacing (negative results, surprises, tradeoffs).
- Roadmap delta: chunks completed/added/obsoleted; re-prioritization proposals needing a user decision.
- Suggest the next chunk title (`roadmap_next`) — don't auto-run it.
- Offer to commit code (don't auto-commit unless the user has a standing instruction). Note: `ROADMAP.md` is often gitignored — check before staging it.

### 13. Emit the handoff prompt — ALWAYS, the last thing every run

End every run with a copy-pasteable `/roadmap` prompt for the NEXT session, in ONE ```
fenced code block (starting with the literal `/roadmap` line), so the next session does
zero re-learning. Write it fresh from the CURRENT state — never a stale copy. It MUST be
tight, SPECIFIC to the next chunk, and include:
- **Where we are** — what just shipped (1 line) + the EXACT next chunk (`roadmap_next` id +
  title + status). If it's an umbrella, say "decompose it" with suggested dep-ordered subs.
- **This run's job** — the one concrete objective + any dep wiring to apply.
- **Proven templates to clone** — the specific existing files/recipes this kind of chunk
  should copy, so the next run doesn't rediscover them.
- **Gotchas that cost time** — the non-obvious traps you hit THIS run, carried forward.
- **The real gates** — the exact verify commands + their honest-PASS condition, and the
  current `think:N` high-water mark (so the next run numbers its steps right).
- **Honest open items** — anything blocked/deferred/half-true the next run should know.
A run that ships a chunk but omits the handoff prompt is **incomplete**.

## Constraints and discipline

- **Native state is the source of truth.** Mutate via `roadmap_*` tools; `ROADMAP.md` is a generated view (`export`). Never hand-edit the markdown as the plan of record.
- **Atomic chunks.** One chunk per invocation. Split (`roadmap_add_chunk` sub-chunks + `deps`) if too big.
- **Verify before marking done.** A failing check invalidates "done". No exceptions — `roadmap_complete_chunk` only after green gates.
- **think open + close are mandatory.** Mid-flight steps for load-bearing decisions only.
- **ship tracks everything the agent DOES.** Objective at start, plan tasks, record actions, check gates, finalize at end.
- **Cross-reference the three families.** `roadmap_start_chunk` returns `chunk:<id>`; wire it into the ship objective + a think `execution_ref`; `roadmap_link` the resulting `think:`/`task:` refs back. The combined trace should be one graph.
- **ministr for exploration, not Grep/Glob.** Reserve shell search for the rare case ministr isn't indexed.
- **serpapi for design choice + claim validation.** Don't search when well-known; do search when comparing alternatives.
- **Re-prioritization is a proposal.** `roadmap_reprioritize` records a suggestion; the human decides. Discovery goes to `backlog`.
- **Don't silently expand scope.** Record scope expansion in think, `ship_block` if needed, or split the chunk.
- **Pinned think steps are durable context.** Use sparingly for genuine load-bearing findings.

## Edge cases

- **No native roadmap yet**: `roadmap_status` returns zero chunks. If the project has a hand-written `ROADMAP.md`, seed native state once: `THINK_AND_SHIP_PERSIST=true think-and-ship roadmap import --file ROADMAP.md` (use `--dry-run` first to review). Otherwise **bootstrap**: ask the user for goals (default a brief read of `README.md`/`CHANGELOG.md`/`git log`), then `roadmap_add_chunk` 3–6 reasonable `pending` chunks and stop — let the user iterate before implementing.
- **Persistence disabled**: if state vanishes between sessions, the server is running without `THINK_AND_SHIP_PERSIST=true`. Flag it; native state is in-memory only until enabled.
- **No ready chunks**: `roadmap_next` is null (all done or all blocked). Report blockers; offer to bootstrap the next phase.
- **Verification harness unknown**: ask once, then record it (a chunk note or the README's Development section).
- **ship has stale state from a prior session**: `ship_reset` before starting fresh, or `ship_status` to decide whether to continue.
- **A server is unavailable**: note the gap in the final report and proceed without. Core requirements (think open/close, ship objective/finalize, roadmap start/complete) are mandatory when the server exists; skip gracefully if not.
- **Chunk requires multi-session work**: split it into sub-chunks (`roadmap_add_chunk` + `deps`) before starting.

## Pairing with /loop

`/loop /roadmap` invokes this skill on a self-paced cadence. Each tick advances one chunk. Strictly one-chunk-per-tick so the user can interrupt.

## What this skill is NOT

- Not a planning tool — use `/plan` to design the roadmap initially.
- Not a one-shot implementer — use direct prompts for single bug fixes / features.
- Not a commit/push automaton — offer to commit; don't push without explicit instruction.
- Not a roadmap-rewriter — re-prioritization stays an explicit user decision (`roadmap_reprioritize` proposes; the user disposes).
