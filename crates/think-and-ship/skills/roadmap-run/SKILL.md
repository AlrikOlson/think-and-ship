---
name: roadmap-run
description: >-
  Superseded by repeated advance-work calls; kept for existing users. Advance
  SEVERAL roadmap chunks in one session, as a dependency-ordered run rather than
  one chunk. Selects a frontier (by theme, count, or explicit ids), executes
  each chunk under the full `/roadmap` per-chunk contract — own think
  open/close, own ship objective, own gates, own commit — and between chunks
  does the thing a single-chunk run structurally cannot: re-derives the frontier
  and checks whether what just landed SUBSUMED something still queued. Use when
  the user types `/roadmap-run`, says "do the next few chunks", "knock out the
  perf work", "all in one run", "keep going until X", or names a group of
  related chunks. NOT for one chunk (use `/roadmap`) and NOT for re-planning
  (use `/roadmap-refresh`).
---

# /roadmap-run — several chunks in one session, without losing atomicity

You are the operator of a **run**: a bounded sequence of roadmap chunks executed in one session. The unit of work is still the chunk. What this skill adds over calling `/roadmap` N times is everything that only exists *between* chunks.

**This skill does not redefine how a chunk is done.** The per-chunk contract is `/roadmap`'s loop, and you follow it in full for every chunk — think open, ministr grounding, serpapi where prior art helps, implement, verify against the real gates, `roadmap_complete_chunk`, think close, commit. Read `{{SKILLS_DIR}}/roadmap/SKILL.md` and obey it per chunk. What follows is only the run-level layer.

> **Operating doctrine (apply automatically): `/craft`.** Same as `/roadmap`: tri-MCP interleave, atomicity, commit-on-`main` with the project trailer, verify-with-the-real-gate (never mask an exit code), honest negative findings. If the project has a GUI, `/craft` §B applies and the verify stage of each chunk is `/gui-scrutiny`.

## Why this exists (the three things one-at-a-time cannot do)

1. **Subsumption.** Chunks that were filed separately often collapse into each other once one of them lands. A 64×-cheaper solve can make "compact the cache", "warm the cache" and "the warm path is too slow" all moot at once. A single-chunk run finishes and stops; it never looks at the queue it just invalidated. **A run must.**
2. **Frontier re-derivation.** Completing a chunk changes `deps` satisfaction, which changes what is ready. The right second chunk is often not the one that was second when the run started.
3. **Amortized setup with un-amortized rigor.** One tool load, one codebase grounding, one mental model — but still one commit, one gate pass and one think close-out *per chunk*. Cheap where it should be cheap; strict where it must not be.

## When NOT to use it

- One chunk → `/roadmap`.
- Re-planning, research, re-prioritization → `/roadmap-refresh`. **A run does not re-litigate priorities.** If the run discovers the plan is wrong, it *stops and says so*; it does not quietly re-sort the map.
- The chunks are large, exploratory, or design-heavy. A run is for work whose shape is already known. If chunk 1 turns out to be a research project, that is a stop condition, not a challenge.

## Inputs

- `/roadmap-run` → take the next 3 ready chunks.
- `/roadmap-run <n>` → take the next `n`.
- `/roadmap-run <theme>` → e.g. `perf`, `the scene work`, `everything blocking X`. Resolve to a chunk set and **show it before starting**.
- `/roadmap-run <id> <id> <id>` → exactly these, in this order.
- `/roadmap-run until <condition>` → e.g. `until the warm blast is under budget`. Bounded by the stop conditions below regardless.
- `--dry-run` → produce the plan (§2) and stop. No implementation.

## The run

### 0. Load tools and state

Staged, as `/roadmap` step 0 lists — the opening call loads only `roadmap_status`, `roadmap_next`, `think_engine_status` and `think_record_step`, and every other tool is loaded at the step that uses it. A run spans several chunks, so the per-step loads recur per chunk rather than once for the run; that is the point, not an inefficiency. Then `roadmap_status` and `ship_status`.

If a chunk is already `in_progress`, resolve it before starting a run. If `ship_status` shows a live objective, `ship_reset` or finish it. **A run must not start on top of unfinished state.**

### 1. Select the frontier

Build the candidate set: chunks that are `pending`, that carry **no blocker**, and whose `deps` are all `done`, in priority order. Then filter, and **state every exclusion out loud**:

- **Blocked by something that is not a dependency** — the chunk's `roadmap_status` row carries a `blocker_kind` (`premise_refuted` | `premise_unmet` | `awaiting_human` | `external`; the field is absent when nothing blocks it), and `roadmap_next` already refuses it. **Excluded, and this one is not a judgment call.** A run that derives its own frontier from the chunk list is the one path that can walk past the engine's refusal, so apply it explicitly rather than assuming the priority order did it for you. Name the kind when you exclude one — `counts.blocked_by` totals them by kind, so the run can say how much of the board is stuck and on what.
- **Human-gated** — the description says a play-test, a design decision, a visual review or a user confirmation is part of closing it. **Excluded. Never take one headless inside a run.**
- **Backlogged at the user's request** — excluded.
- **Exploratory** — the chunk's own description contains an open design question rather than a known shape. Excluded; it wants `/roadmap`'s undivided attention or a refresh.
- **Coupled to something excluded** — if it only makes sense after an excluded chunk, exclude it too.

If filtering empties the set, say so and stop. **An empty frontier is a finding, not a failure.**

### 2. Write the run plan, and show it before touching code

A short ordered list. For each chunk: id, one-line goal, why it is in this position, and — critically — **what it might subsume**. Then:

- **the gate set** each chunk must pass (the project's real commands),
- **the stop conditions** (§5), stated explicitly,
- **the expected shape of the end state**.

Show it. If `--dry-run`, stop here. Otherwise proceed without waiting — the plan is a contract you are announcing, not a question you are asking, unless something in it needs a decision only the user can make.

### 3. Execute each chunk under the full `/roadmap` contract

Per chunk, no shortcuts:

- `roadmap_start_chunk` → `ship_set_objective` (this clears the prior chunk's tasks — that is correct and wanted) → `ship_plan`.
- **think open.** Number by omitting `step_number`.
- Ground with `ministr`. Research with `serpapi` where prior art decides something.
- Implement.
- **Verify against the real gates, with `ship_check(command:)` and no pipe.** A piped gate returns the pipe's exit code and is worthless.
- `ship_finalize` → `roadmap_complete_chunk`.
- **think close**, pinned if load-bearing.
- **Commit — one commit per chunk.** Never batch a run into one commit: the whole point of chunk atomicity is that each is independently revertable and independently explicable.

Task ids must be unique per objective; prefix them with the chunk (`occl-explore`, `lattice-explore`) so a run's traces stay legible.

### 4. Between chunks — the part that only exists here

After each chunk closes, before starting the next, do all four:

**(a) Subsumption check.** For every remaining chunk in the run *and* the ones just outside it: is it still necessary? The chunk that just landed may have made it moot, cheaper, or wrong. If a queued chunk is dead, `roadmap_obsolete_chunk(id, reason)` **with the measurement as the reason** — not a hunch. If it is diminished but alive, `roadmap_update_chunk` its description to say what changed. **This is the highest-value step in the skill; do not skip it because the queue "looks fine".**

**(b) Re-derive the frontier.** Recompute what is ready. If a better next chunk now exists, take it and say why the plan changed.

**(c) Check the stop conditions** (§5).

**(d) Append one line to the run ledger** — chunk id, outcome, gate state, commit sha, and anything subsumed. Keep it in your reply text as you go. **This ledger is what survives context compaction**; the per-chunk think close-outs are the durable copy of the detail.

### 5. Stop conditions — any one ends the run

Stop, report honestly, and do not continue:

1. **A required gate is red and one fix attempt did not clear it.** Never stack a second chunk's changes on a broken tree. Leave the tree green (revert if needed) or leave it clearly broken and say exactly how.
2. **A chunk needs a human** — a decision, a play-test, a visual judgement, an approval.
3. **Scope explosion.** A chunk turns out to be materially bigger than filed. Record the finding, `roadmap_update_chunk` its description with what you learned, stop.
4. **The plan is falsified.** The run reveals the ordering or the premise is wrong. Stop and recommend `/roadmap-refresh`; do not silently re-plan mid-run.
5. **The budget is spent** — the count, the theme, or the `until` condition is met.
6. **Chunk 1 went badly.** Treat the first chunk as a probe on the run's premise. If it blew scope or fought its gates, the premise that these chunks are small and known is false. Stop at one.
7. **Context is running short.** Better to stop cleanly with a handoff than to be compacted mid-chunk. If you are deep in context and a chunk remains, stop and hand off.

### 6. Close the run

- `roadmap_status` for the final state.
- **One `think_trace_checkpoint`** for the run (not per chunk) — the whole-trace health view is a run-level question.
- Regenerate the roadmap view if the project keeps one, and commit it if tracked. One roadmap commit at the end of the run is fine; the *code* commits are per chunk.
- If anything was obsoleted or re-described, that is part of the report, not a footnote.

### 7. Report

Lead with the ledger — a table is right here:

| chunk | outcome | gates | commit |
|---|---|---|---|

Then, and keep it tight:

- **What the run actually changed**, in one line per chunk.
- **Subsumptions** — what got obsoleted or shrunk, and the measurement that decided it. This is usually the most interesting output; give it room.
- **Honest findings** — negative results, surprises, anything that fought back.
- **Why the run stopped** — which stop condition, named.
- **Roadmap delta** and any `roadmap_reprioritize` proposals needing a user decision (proposals only; a run never reorders on its own).
- **The handoff prompt**, per §8.

### 8. Emit the handoff prompt — always, last thing

Exactly as `/roadmap` step 13 requires: one fenced block starting with the literal `/roadmap` or `/roadmap-run` line, written fresh from the final state. It must name the exact next chunk, this run's gotchas, the real gates with their honest-PASS condition, the `think:N` high-water mark, and the open items. **A run that ships chunks but omits the handoff is incomplete.**

## Discipline

- **Per-chunk atomicity is not negotiable.** Own objective, own gates, own commit, own think close. A run is a sequence of complete chunks, never one big chunk wearing a list.
- **Green between chunks.** The tree passes the real gates before the next chunk starts. This is what makes a mid-run stop safe.
- **The run does not re-prioritize.** Discoveries go to `backlog`; re-ordering is a proposal. `/roadmap-refresh` owns the map.
- **Obsolete with evidence.** Subsumption is the point of this skill and also its main way to do damage. A chunk is dead when a measurement says so, never when the queue merely looks redundant.
- **Never mask an exit code.** `ship_check(command:)`, no pipe, every gate, every chunk.
- **Prefer stopping to pushing.** Three chunks done cleanly with a good handoff beats five done sloppily. The stop conditions are the feature.
- **Say what you skipped.** Every excluded chunk, every deferred acceptance criterion, named in the report.

## Edge cases

- **A chunk fails its gates and the fix is obvious and small** → fix it, re-verify, continue. One attempt. A second failure is stop condition 1.
- **Two queued chunks touch the same code** → order them so the more invasive lands first, and say so in the plan. If they genuinely conflict, take one and re-describe the other.
- **A chunk completes but its acceptance is only partly met** → do not `roadmap_complete_chunk`. Update the description with what is left and stop, or split the remainder into a new chunk.
- **The run finishes early because everything got subsumed** → an excellent outcome. Report it as such, with the evidence.
- **`roadmap_next` points at an excluded chunk** → that is expected and fine; the run selects its own frontier. Say plainly that you skipped it and why.
- **No `ROADMAP.md`** → skip the regeneration step; native state is the source of truth regardless.
