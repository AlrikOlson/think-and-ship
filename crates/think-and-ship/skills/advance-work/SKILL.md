---
name: advance-work
description: >-
  Do exactly one unit of work in the workstream you are already focused on,
  record it, and stop. Run it when the user types the advance-work command, or
  says "advance the current work", "do the next unit", "keep going on this",
  or "next one". Reads the per-caller focus set by switch-work and honours its
  mode: shape completes one planning or research decision, build completes one
  ready roadmap chunk behind the project's real gates, listen processes one
  stakeholder signal. Always emits a receipt naming the unit, the evidence and
  the next candidate. One unit per invocation — it never batches chunks, never
  continues into adjacent work, and never leaves the focused workstream even
  when it is empty. Not for "switch to X", "run every remaining chunk", or
  unrelated one-off fixes.
---

# advance-work

One bounded, evidenced unit inside the current focus. Then stop.

Stopping is not a limitation of this skill — it is the product. A run that does
two units has destroyed the user's ability to review one.

## The universal algorithm

Every mode follows the same seven steps. Only step 4 differs.

1. **Load the focus.** `roadmap_focus_get` with your lane — the same lane
   `switch-work` used, normally the worktree root (`git rev-parse --show-toplevel`).
   No focus → stop, see [No focus](#no-focus-is-an-actionable-stop).
2. **Reconstruct state.** Roadmap frontier for the focused workstream, current
   ship objective, reasoning trace head, pending signals, and Spec Kit artifacts
   if this project has them (see [speckit.md](references/speckit.md)).
3. **Select ONE unit** eligible in this workstream *and* this mode. If nothing
   is eligible, emit a no-work receipt and stop — do not widen the search.
4. **Execute only that unit**, under its mode's rules:
   [shape.md](references/shape.md) · [build.md](references/build.md) ·
   [listen.md](references/listen.md).
5. **Verify.** Run the checks that mode requires. Real commands, real exit codes.
6. **Record** native think / ship / roadmap / signal state and cross-reference it.
7. **Recompute the next candidate — do not execute it.** Emit the receipt
   ([receipt.md](references/receipt.md)) and stop.

## Never

- **Change the group or the mode.** Only `switch-work` does that. If the focus
  is wrong, say so and stop; do not fix it by moving.
- **Fall through to a globally ready chunk.** The frontier you act on is the
  focused workstream's. A chunk outside it is not a fallback, it is out of scope.
- **Batch.** Two chunks is two invocations. "While I'm here" is the failure.
- **Declare success over a red, skipped or unrun check.** A self-reported pass
  with no command behind it is not evidence.
- **Treat an empty workstream as permission to pick another.** Absence of work
  is a finding, and the receipt reports it.
- **Hard-require an optional MCP server.** See [Optional tools](#optional-tools).

## Selecting the one unit

| Mode | Eligible unit | Where it comes from |
|---|---|---|
| `shape` | One unresolved question, requirement, or plan area | The workstream's backlog, open questions, or a stale plan |
| `build` | One `pending` chunk in the workstream, unblocked, deps done | The focused frontier's `next` |
| `listen` | One unresolved signal relevant to the workstream | Pending signals, relevance established with evidence |

Take the frontier's `next` when there is one. If you deviate, say why in the
receipt — a deviation nobody records is indistinguishable from a mistake.

## Optional tools

Some capabilities are enrichment, not requirements:

- **A code-intelligence MCP server** (e.g. ministr) makes grounding cheaper. If
  it is absent, use your harness's ordinary code search and file reading.
- **A web-search MCP server** (e.g. SerpAPI) makes external research cheaper. If
  it is absent, use your harness's ordinary web capability.

If neither is available, say so in the receipt's evidence section and describe
what you *could* verify. **Report the reduced evidence surface honestly; never
claim grounding you did not do, and never refuse work you can still do.**

## Modes at a glance

Full rules live in the references — read only the one you are in.

- **[shape](references/shape.md)** — one planning or research decision. May
  amend a spec, plan, or roadmap unit; may record a validated decision or a
  negative finding. **Must not modify implementation source**, and must not run
  an implementation task with research framing.
- **[build](references/build.md)** — start the chunk, set a ship objective from
  its real acceptance criteria, open the mandatory reasoning trace, ground in
  the existing code, implement, run the project's real gates, and complete ship
  and roadmap state **only after required gates pass**.
- **[listen](references/listen.md)** — one signal: ground it in the code and
  the product's current state, research only if needed, record enrichment, then
  promote, dismiss, defer or surface it. **Never a second signal.**

## Spec Kit

If `.specify/` exists and a feature matches this workstream, the Spec Kit
adapter engages **automatically** — you do not invoke a separate skill. It
supplies the requirements and the definition of done. See
[speckit.md](references/speckit.md).

If `.specify/` is absent, proceed on native roadmap state. **Do not fabricate a
`.specify/` tree, and do not initialize Spec Kit** to make a project look like
one; adopting it is a human decision.

## The receipt

Every invocation ends with one, completed or stopped. Exact shape and worked
examples: [receipt.md](references/receipt.md).

```
Focus: <group>
Lane: <lane>
Mode: <shape|build|listen>
Unit: <stable id and title>
Result: <completed|blocked|no-ready-work|awaiting-human>
Evidence:
  - <check, artifact, or source reference>
Native records:
  - <chunk:/task:/think:/signal:/check: refs>
Discoveries:
  - <new facts or "none">
Next candidate: <id/title or none>
Stop reason: one-unit boundary
```

## Stopping conditions

### No focus is an actionable stop

Nothing to advance *into*. Do not guess a workstream.

```
Focus: none
Lane: /Users/dev/code/acme
Mode: none
Unit: none
Result: awaiting-human
Evidence:
  - roadmap_focus_get returned focus:null for this lane
Native records:
  - none
Discoveries:
  - none
Next candidate: none
Stop reason: no focus set — run switch-work first
```

### No ready work is an honest receipt

The workstream exists and has nothing runnable. This is a result, not a failure,
and **not** a reason to look at another workstream.

```
Focus: Platform
Lane: /Users/dev/code/acme
Mode: build
Unit: none
Result: no-ready-work
Evidence:
  - focused frontier: ready 0, blocked 4
  - platform-tls-rotation blocked on an external certificate authority
Native records:
  - none
Discoveries:
  - none
Next candidate: none
Stop reason: one-unit boundary
```

### A required check went red

The unit is **not** complete. Do not finalize ship or roadmap state.

```
Focus: Authentication
Lane: /Users/dev/code/acme
Mode: build
Unit: auth-session-rotation — Rotate session tokens on privilege change
Result: blocked
Evidence:
  - cargo test --workspace: FAILED (exit 101), 2 tests in session::rotate
Native records:
  - chunk:auth-session-rotation (still in_progress)
  - check:cargo-test (failed, required)
Discoveries:
  - rotation invalidates the refresh token, which the spec did not anticipate
Next candidate: auth-session-rotation (same unit, after the failure is fixed)
Stop reason: required check failed — completion refused
```

### Awaiting human

A choice that is genuinely the user's — an irreducible product decision, or a
reprioritization proposal that needs judgment. Record the proposal, do not
accept it on their behalf, and stop.

## Not this skill

| The user says | Do this instead |
|---|---|
| "switch to billing" | `switch-work` |
| "run every remaining roadmap chunk" | Decline the batch; offer repeated invocations |
| "give me a board briefing" | A reporting skill, if installed |
| "fix this unrelated typo" | Just fix it — it is not a roadmap unit |

If the user explicitly asks for several units, do **one**, and say plainly that
they can invoke this again. The one-unit boundary is not negotiable by
rephrasing.
