---
name: roadmap-refresh
description: >-
  Superseded by advance-work in shape mode; kept for existing users. Research-
  driven refresh of an existing project roadmap. Don't implement anything — pick
  a topic (a phase, an area, a technology, or the whole landscape), run a think
  + ministr + serpapi research cycle, then mutate native `roadmap_*` state to
  reflect new findings (rewrite descriptions, add discoveries to backlog, mark
  obsolete items, surface re-prioritization proposals for user review, record
  the refresh provenance). Companion to `/roadmap` — `/roadmap` advances one
  chunk; `/roadmap-refresh` updates the map itself. Use when the user types
  `/roadmap-refresh`, says "the roadmap feels stale", "let's revisit
  priorities", "research where we should go next", or equivalent.
---

# /roadmap-refresh — research and re-shape the roadmap

You are the operator of a research-and-refresh cycle for an evolving project roadmap. Each invocation does **one focused refresh** end-to-end (frame question → research → synthesize → propose mutations → apply). The roadmap is a living document; this skill is how it stays current with the codebase, the team's discoveries, and the outside world.

The roadmap is **native state** in the unified **think-and-ship** MCP server, exposed through the `roadmap_*` tool family. You mutate it with tool calls, not by hand-editing markdown — `ROADMAP.md` is a generated *view* (`think-and-ship roadmap export`). Two more MCP servers — `serpapi` (research, the workhorse here) and `ministr` (code grounding) — are mandatory infrastructure.

> **Operating doctrine (apply automatically): `/craft`.** This refresh runs under the
> house style in `{{SKILLS_DIR}}/craft/SKILL.md` — lean hardest on the tri-MCP
> interleave (§A1) and honest negative findings (§A5): **ground in the real code with
> ministr before researching an alternative** (it prevents wasteful "redo everything"
> proposals). File `/craft` §B (GUI) gaps as **backlog chunks** — don't fix them inline
> (this skill doesn't implement).

This skill is the **companion to `/roadmap`**:

- `/roadmap` — advance the roadmap by implementing one chunk.
- `/roadmap-refresh` — pause implementation; research; reshape the roadmap.

> The think-and-ship server's reasoning family is `think_*` (the legacy `deliberate_*` names still work as deprecated aliases). This skill uses `think_*` and `roadmap_*`; it does not touch `ship_*` (no execution).

## When to invoke this skill

- User typed `/roadmap-refresh` (optionally with a topic).
- User said "the roadmap feels stale" / "let's revisit priorities" / "what should we be doing next?" / "do some research and update the roadmap" / equivalent.
- After a significant chunk lands and direction may have shifted.
- After external news (new library release, standards change, competitor announcement) that may affect priorities.
- Before a new phase to validate the current plan is still the best plan.

Do **not** invoke this skill when:
- The user wants to implement the next chunk → use `/roadmap`.
- The user wants to bootstrap a brand-new roadmap → use `/roadmap` (which can bootstrap) or `/plan`.
- The user is fixing a one-off bug or feature → do it directly.

## Inputs

- No args → broad refresh: scan the whole roadmap (`roadmap_status`), pick the section most likely to have drifted, research it, propose mutations.
- `/roadmap-refresh <topic-or-phase>` → focus on a specific area. Examples: `Phase 3`, `auth`, `observability`, `the v2.0 backlog`, `Rust async`.
- `/roadmap-refresh dry-run` (or appended `--dry-run` / `--no-write`) → produce findings + a written mutation proposal, but do **not** mutate native state. Used when the user wants a research report first.
- `/roadmap-refresh status` → just report current state (`roadmap_status` + last refresh note if recorded, which sections look most stale) and exit.
- Anything else after `/roadmap-refresh` → treat as freeform research scope.

## Roadmap state (native `roadmap_*`)

The roadmap lives as **chunks** in the think-and-ship server (read via `roadmap_status`), not as a markdown file you parse. Each chunk has an `id`, `title`, `status` (`backlog`/`pending`/`in_progress`/`blocked`/`done`/`obsoleted`), `priority`, `description`, `acceptance[]`, `deps[]`, and `cross_refs[]`. `ROADMAP.md`, if present, is a generated view (`roadmap_export` / `think-and-ship roadmap export`) — never the source of truth.

If `roadmap_status` returns zero chunks, this skill has nothing to refresh. Tell the user and suggest `/roadmap` (which can seed native state from an existing `ROADMAP.md` via `import`, or bootstrap fresh). Do not create a roadmap from this skill.

## The loop

### 0. Load MCP tools — the OPENING set only

**Load in stages. Do not bulk-load the families up front.** See `/craft` §A0
("Staged tool loading") for the doctrine and the reason; this section is its
application to `/roadmap-refresh`.

A refresh is mostly *reading*. Most invocations never call a mutation verb at
all, and a dry-run never can. So the opening call loads only steps 1–3:

```
ToolSearch(
  query: "select:mcp__think-and-ship__roadmap_status,mcp__think-and-ship__think_engine_status,mcp__think-and-ship__think_record_step",
  max_results: 3
)
```

Everything else is loaded **at the step that uses it**:

| Load at | Issue |
|---|---|
| Step 4, grounding | `select:…ministr_survey,…ministr_symbols,…ministr_definition,…ministr_read` — add `…ministr_toc` for layout, `…ministr_references,…ministr_impact` only if the refresh may propose changing shared code, `…ministr_bridge` only at an IPC/FFI boundary, `…ministr_solid` only when proposing a refactor chunk |
| Step 5, research | `select:mcp__serpapi__search` — skip entirely when the scope is purely internal |
| Step 7–8, **only the mutation verbs this refresh will actually call** | `…roadmap_update_chunk` / `…roadmap_add_chunk` / `…roadmap_obsolete_chunk` / `…roadmap_reprioritize` / `…roadmap_set_status` |
| Step 8, provenance | `select:…roadmap_record_refresh,…roadmap_link` |
| Only when needed | `…roadmap_next`, `…roadmap_export`, `…think_pin_step`, `…think_trace_checkpoint`, `…think_search_trace`, `…think_get_step`, `…think_revise_estimate`, `…think_step_impact`, `…think_set_branch_status` |

Decide the mutation set at the END of step 6 (synthesis), when you know which
categories apply — then load exactly those verbs. Loading all five before you
know is the specific mistake this staging exists to prevent, and `--dry-run`
must load **none** of them.

What each tool is for (reference — this table is documentation, not a shopping
list; loading a tool is a separate, deliberate act):

- **`roadmap_*`** (the map you're refreshing — native state):
  - `roadmap_status` — counts + the next-ready chunk + the priority-sorted chunk list. Start here.
  - `roadmap_update_chunk` — patch a chunk's description / acceptance / deps / priority-fields in place (the most common refresh mutation).
  - `roadmap_add_chunk` — add a newly-discovered chunk (default `status: "backlog"`).
  - `roadmap_set_status` — transition a chunk (e.g. `pending → blocked` when research reveals a new dependency).
  - `roadmap_obsolete_chunk` — mark a chunk `obsoleted` with a reason (overtaken by events).
  - `roadmap_reprioritize` — record a re-prioritization *proposal* (suggested priority + reason). Does NOT reorder — human decision.
  - `roadmap_link` — attach a `think:<N>` (or other) cross-ref so the reasoning behind a mutation is traceable.
  - `roadmap_record_refresh` — record the refresh itself: a summary + the `think` step numbers that motivated it (first-class provenance).
  - `roadmap_export` — regenerate the markdown/json view after mutating.
- **`think_*`** (the research-reasoning trace):
  - `think_record_step` — open / mid-flight / close reasoning steps (mandatory open + close).
  - `think_engine_status` — check trace state before the first step. Numbering is project-global; `next_step_number` is the next safe one. `total_steps` is a COUNT of retained steps, NOT the head — numbering from it writes an orphan step (think:1424).
  - `think_trace_checkpoint` — whole-trace health at end of long refreshes.
  - `think_pin_step` — pin a load-bearing research finding so future iterations see it.
  - `think_search_trace` / `think_get_step` — recall prior discoveries from earlier refreshes or chunks.
  - `think_revise_estimate` — adjust `estimated_total` if research expands the trace.
  - `think_step_impact` — before revising a prior step, see what depends on it.
  - `think_set_branch_status` — mark a branch active/merged/abandoned when you fork research directions.
- **`serpapi`** (the workhorse for `/roadmap-refresh` — heavier use than `/roadmap`):
  - `serpapi_search` — landscape scans, prior art, current SOTA, 2026 norms, competitor moves, standards/library updates.
  - Default to current-year results (`as_ylo: <current_year>`) when the question is "what's the modern answer".
  - Use `engine: "google_scholar"` for academic questions; default google for OSS / production-blog / standards material.
- **`ministr`** (ground every external finding in the actual code):
  - `ministr_toc` / `ministr_survey` / `ministr_symbols` / `ministr_definition` / `ministr_read` / `ministr_references` — the PRIMARY exploration surface.
  - `ministr_extract` — atomic claims from a section.
  - `ministr_bridge` — cross-language boundaries; mandatory before suggesting changes that cross IPC/FFI.
  - `ministr_solid` — SOLID-signal audit (useful when proposing refactor chunks).
  - `ministr_related` / `ministr_impact` — dependency chains and blast radius for any proposed change.
  - `ministr_usage` — usage statistics on symbols (informs "is this still used / worth keeping?").

If any MCP server isn't available, note it and proceed with what you have — but flag the gap in the final report. **Do not silently degrade the core interleave**: serpapi is load-bearing for this skill; if it's down, say so explicitly and tell the user the refresh will be code-only.

### 1. Load roadmap state

Call `roadmap_status`. Identify:
- All chunks and their status (backlog / pending / in_progress / blocked / done / obsoleted).
- The current `in_progress` chunk if any (the refresh shouldn't blindside it).
- Recently-`done` chunks (context on what's shipped — informs which assumptions are now testable).
- Any chunk descriptions with explicit "stale by" markers, dated references, or specific library versions.

If `/roadmap-refresh status` was requested, stop here. Report:
- Total chunk count by status.
- Sections that look most likely to need a refresh (mentions of specific library versions, dated references, "we'll see" notes).
- The last `roadmap_record_refresh` note if one exists.

### 2. Pick the refresh scope

If the user gave a topic, use it. Otherwise pick by this priority:

1. The chunk(s) adjacent to the current `in_progress` chunk (where decisions about what's next matter most).
2. A `pending` chunk whose description references specific external assumptions (library versions, API surfaces, "the current best practice is X") — those decay fastest.
3. The `backlog` chunks — they tend to accumulate stale ideas.
4. As a last resort, the whole roadmap (broad sweep). Warn the user this will produce a wider but shallower refresh.

State the picked scope in one sentence: *"Refreshing Phase 3 (real-time sync) — the WebSocket library choice was made 8 months ago and worth re-validating."*

### 3. Open with a `think` step

Mandatory. Use `think_record_step`. Step numbers are project-global: OMIT `step_number` and the engine appends at the head, or take it from `think_engine_status.next_step_number`. Never from `total_steps` — that is a count, not the head.
- `purpose`: "Open refresh — <scope>"
- `context`: what's currently in the roadmap for this scope; what assumptions look testable now
- `thought`: the research plan — concrete questions to answer; sources to consult; what would change the roadmap
- `outcome`: research plan locked in
- `next_action`: the first concrete query or code area to investigate
- `dependencies`: prior step numbers if continuing an existing trace (refreshes often build on `/roadmap` close-out steps that pinned discoveries)

### 4. Ground in the codebase with `ministr`

Before researching the outside world, ground in what the code actually says today:

- `ministr_toc` for structural overview of areas relevant to the scope.
- `ministr_survey(query: "...")` for "what does this codebase currently do about X?"
- `ministr_symbols(query: "...")` to enumerate the implementation surface of the affected area.
- `ministr_definition(id: ...)` / `ministr_read(id: ...)` to confirm specifics before claiming "we use X".
- `ministr_references(id: ...)` to assess blast radius if the refresh might propose changing a shared abstraction.
- `ministr_bridge` if the scope touches any IPC/FFI boundary.
- `ministr_usage` / `ministr_impact` for "is this code path even live?" before recommending we double down or rip it out.

A roadmap refresh that recommends switching libraries based on outdated assumptions about what's in the code is worse than no refresh. **Verify the premise before researching the alternative.**

### 5. Research the outside world with `serpapi`

This is the load-bearing step. Spend more here than `/roadmap` would. For each open question from step 3, run a targeted query:

- **"What's the current best practice?"** — default google, current year, look for production blog posts + project READMEs + standards docs.
- **"What does the academic literature say?"** — `engine: "google_scholar"`, `as_ylo: <current_year - 1>`.
- **"Is library/protocol/standard X still actively maintained?"** — search the project name + recent activity.
- **"What's the competitive landscape?"** — "best X for Y 2026" and "X vs Y" comparisons.
- **"Has the API / behavior of dependency X changed?"** — recent changelogs / migration guides.

Cite 2–5 sources per question in the think trace. Quote concrete claims, not vibes. Thin results are a valid finding — "research found no evidence that Y is preferred over X".

Skip serpapi only if the scope is *purely* internal (e.g. "should we extract this helper into its own module?").

### 6. Synthesize with `think`

Mandatory mid-cycle step. Use `think_record_step` to synthesize:
- `purpose`: "Synthesis — refresh of <scope>"
- `thought`: what the research changes, doesn't change, or surfaces as a new question
- `outcome`: a concrete list of proposed roadmap mutations (see step 7 categories)
- `next_action`: "apply mutations via roadmap_*" (or "produce dry-run report if user requested")
- `dependencies`: cite the prior research steps with `relation` labels (`supports` / `refutes` / `depends_on`)

If the research surfaced **conflicting evidence**, branch via `branch_from` + `branch_name` and record both directions. Mark the losing branch `abandoned` after deciding.

### 7. Propose roadmap mutations → native tool calls

Each mutation category maps to a `roadmap_*` tool. Be explicit about which you're doing:

1. **Description / acceptance update** — goal unchanged, but description / file list / acceptance is stale → `roadmap_update_chunk(id, description?, acceptance?, deps?)`. Safe to apply without asking.

2. **New backlog discovery** — research surfaced a chunk that doesn't exist yet → `roadmap_add_chunk(id, title, status: "backlog", description, acceptance?, deps?)`. Do not auto-promote to `pending`.

3. **Obsoleted chunk** — a pending chunk is no longer needed → `roadmap_obsolete_chunk(id, reason)`. The server keeps it for history; ask the user only if they'd want it revived later.

4. **Re-prioritization candidate** — a chunk should move up/down → `roadmap_reprioritize(id, suggested_priority, reason)`. **This records a proposal; it does NOT reorder.** Surface it in the final report. Re-prioritization stays a user decision.

5. **Newly-blocked chunk** — research revealed a new dependency → `roadmap_set_status(id, "blocked")` and, if a new prerequisite is needed, `roadmap_add_chunk` it (then optionally add it to the blocked chunk's `deps` via `roadmap_update_chunk`).

6. **Newly-unblocked chunk** — a previously-blocked item is now ready → `roadmap_set_status(id, "pending")`. Note it in the report.

7. **Research note** — a finding that doesn't yet map to a mutation but is worth preserving → capture it as a `think` step (pinned if load-bearing) and record it via `roadmap_record_refresh`.

If `--dry-run` was requested, write the proposed mutations to the report instead of calling the tools. Stop before mutating.

### 8. Apply mutations + record provenance

Call the `roadmap_*` tools from step 7. Discipline:
- Keep mutations surgical. Don't rewrite every chunk.
- Do not change the `in_progress` chunk mid-flight unless the user explicitly asked.
- Do not delete history — `roadmap_obsolete_chunk` keeps obsoleted chunks; let the user decide on removal.
- **Record the refresh itself**: `roadmap_record_refresh(summary: "<scope>: <what changed>", think_steps: [<the think step numbers>])`. This makes the refresh first-class provenance.
- `roadmap_link` any chunk you mutated to the `think:<N>` step that justified it.
- Regenerate the view if the project keeps a `ROADMAP.md`: `THINK_AND_SHIP_PERSIST=true think-and-ship roadmap export --format markdown > ROADMAP.md` (or write `roadmap_export`'s output).

### 9. Close with a `think` step

Mandatory. Use `think_record_step` again:
- `purpose`: "Close refresh — <scope>"
- `thought`: which mutation categories applied; what stayed unchanged; honest negative findings
- `outcome`: one-paragraph summary
- `next_action`: usually "user reviews re-prioritization proposals" or "next `/roadmap` chunk: X"
- `pinned: true` for genuinely load-bearing findings (a deprecated library; a confirmed-current best practice; a competitor's published move)

For long refreshes, follow up with `think_trace_checkpoint`.

### 10. Report

Final user-facing message:
- Scope refreshed (one sentence).
- Top 3–5 findings from the research, with one source each.
- Mutations applied, by category (descriptions updated: N; backlog adds: N; obsoleted: N; re-prioritization proposals: N).
- Re-prioritization proposals that need a user decision, listed explicitly with the suggested direction (these are `roadmap_reprioritize` proposals — the server has NOT reordered).
- Suggest the next `/roadmap` chunk (`roadmap_next`) — don't auto-run it.
- Offer to commit if the project commits its roadmap view (don't auto-commit; `ROADMAP.md` is often gitignored).

Keep the report under ~30 lines unless the refresh genuinely touched many sections. Density beats verbosity.

## Constraints and discipline

- **Native state is the source of truth.** Mutate via `roadmap_*` tools; `ROADMAP.md` is a generated view. Never hand-edit the markdown.
- **Atomic refreshes.** One scope per invocation. A whole-roadmap refresh is allowed but should be a deliberate user choice, not the default.
- **Verify the premise before researching the alternative.** Use ministr to confirm what the code actually does today before recommending a change.
- **think open + close are mandatory.** Mid-flight research-synthesis steps are mandatory too — that's the whole point of the trace for a research cycle.
- **serpapi is load-bearing.** Skipping it turns this skill into "rewrite the roadmap from memory", the exact failure mode it exists to prevent.
- **ministr for code grounding, not Grep/Glob.** Reserve `Grep`/`Glob` for the rare case ministr isn't indexed.
- **Re-prioritization stays a user decision.** `roadmap_reprioritize` proposes; the user disposes. (Same rule as `/roadmap`.)
- **Record provenance.** Every refresh ends with `roadmap_record_refresh` citing the think steps behind it.
- **Don't touch implementation.** This skill mutates the roadmap, not the code. If a finding needs a code change, `roadmap_add_chunk` it; do not implement.
- **Pinned think steps are durable.** Use them for findings that should persist across future iterations.
- **Cite, don't assert.** Every external claim in the synthesis step should have a serpapi source behind it.

## Edge cases

- **No native roadmap**: `roadmap_status` returns zero chunks → tell the user, suggest `/roadmap` (which can seed from an existing `ROADMAP.md` via `import`, or bootstrap). Do not create one from this skill.
- **Roadmap exists but is all done**: the project may be at a phase boundary. Suggest the user define the next phase (via `/roadmap`) before running a refresh.
- **`in_progress` chunk overlaps the refresh scope**: ask before mutating it. Refreshing a chunk mid-implementation can invalidate work in flight.
- **Serpapi unavailable**: explicitly warn the user. The refresh will be code-only and miss outside-world signal. Suggest re-running when serpapi is back.
- **Ministr unavailable / corpus missing**: fall back to `Read` for code grounding; flag the gap. Do not skip grounding entirely.
- **Research returns conflicting evidence**: branch the think trace, record both, decide, mark the abandoned branch — don't paper over the conflict.
- **Research returns nothing actionable**: report that honestly and still `roadmap_record_refresh` it ("checked X, nothing changed") so the user doesn't re-run the same refresh next month.
- **Scope is too vague** (bare `/roadmap-refresh`, 50 chunks): pick a narrow scope per the step-2 heuristics and explain the choice; offer to broaden on follow-up.
- **Refresh would re-prioritize the chunk the user is currently working on**: record the `roadmap_reprioritize` proposal, do not apply a status change. Let the user finish or pivot deliberately.

## Pairing with `/roadmap` and `/loop`

- **Natural pairing**: `/roadmap-refresh` → user reviews and approves re-prioritization proposals → `/roadmap` to advance the (possibly re-ordered) next chunk.
- **With `/loop`**: `/loop /roadmap-refresh` runs refreshes on a self-paced cadence. Keep each tick scoped to one area so the user can interrupt.
- **Don't chain `/roadmap-refresh` straight into `/roadmap`** without a pause. The point of a refresh is to give the user a chance to reconsider direction before more code lands. Stop after the report.

## What this skill is NOT

- Not an implementer — that's `/roadmap`.
- Not a bootstrapper — `/roadmap` (or `/plan`) creates / seeds roadmaps.
- Not a roadmap-rewriter — mutations are surgical and re-prioritization stays user-driven (`roadmap_reprioritize` proposes only).
- Not a status-only tool — `/roadmap status` covers that. `/roadmap-refresh status` is a quick stale-section report only.
- Not a commit/push automaton — offer to commit the roadmap view; don't push without explicit instruction. The `ROADMAP.md` view is often gitignored, so check before staging.
